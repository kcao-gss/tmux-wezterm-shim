# VERIFY REPORT - wezterm-tmux-shim Phase-0 Spike

Mode: dev. Posture: refute (score-7 goal).
Reviewed commit: 06e380b (worktree == HEAD, no uncommitted changes).
Primary file: src/main.rs (632 lines).
Method: read full source plus both scripts; cross-checked live wezterm cli behavior, the wezterm cli list JSON schema, and PowerShell parsing/semantics on this machine.

## Verdict: BLOCK

Two confirmed defects make the shim incorrect outside the exact happy path the self-test exercised:

1. Every fallback string path in src/main.rs has its backslashes stripped (raw-string corruption), so the WezTerm fallback and the LOCALAPPDATA fallback both point at nonexistent paths.
2. scripts/install.ps1 uses 3-argument Join-Path, which throws on this machine default Windows PowerShell 5.1 under ErrorActionPreference = Stop - the installer aborts before copying the exe.

The fail-soft / no-panic property (the most important invariant of the spike) does hold - I could not find a panic vector on attacker- or CC-controlled input. The BLOCK is for correctness/installability, not for a crash risk.

---

## BLOCK

### B1. Backslashes stripped from all fallback path literals

- src/main.rs:61 - raw literal reads C:UsersDefaultAppDataLocal (should be C:\Users\Default\AppData\Local)
- src/main.rs:122 - raw literal reads C:Program FilesWezTermwezterm.exe (should be C:\Program Files\WezTerm\wezterm.exe)

Problem: these are Rust raw string literals (r-quote form) with the backslashes missing, verified by inspecting the raw bytes. They compile fine (a raw string with no backslashes is legal), which is why the build succeeded - but they are semantically wrong paths.

- state_dir() fallback (line 61): if LOCALAPPDATA is unset, the shim writes state/log to a path literally named C:UsersDefaultAppDataLocal then a backslash then wezterm-tmux-shim (a folder named UsersDefaultAppDataLocal at the drive root). State and log silently land in the wrong place; on a locked-down root the create_dir_all fails and state never persists.
- wezterm_bin() fallback (line 122): if wezterm is not on PATH, the fallback returns C:Program FilesWezTermwezterm.exe, which does not exist. Every run_wezterm call then returns exit -1, list_wez_panes() is empty, and split/list/display all degrade to synthetic ids. The PATH-then-fallback resolution requirement (spec item 7) is effectively non-functional on the fallback leg.

Why the self-test missed it: the tests ran with wezterm on PATH and LOCALAPPDATA set, so neither fallback branch executed.

Fix: restore the backslashes. Either escape them in a normal string or keep the raw-string form with literal backslashes. Prefer building the WezTerm fallback from the ProgramFiles env var rather than a hardcoded path.

### B2. install.ps1 aborts on Windows PowerShell 5.1 (3-arg Join-Path)

- scripts/install.ps1:12 - Join-Path LOCALAPPDATA "wezterm-tmux-shim" "bin"
- scripts/install.ps1:13 - Join-Path LOCALAPPDATA "wezterm-tmux-shim" (this one is 2-arg, OK)

Problem: 3-argument Join-Path (the -AdditionalChildPath form) only exists in PowerShell 6+. This machine default powershell.exe is Windows PowerShell 5.1 (confirmed: 5.1.26100.8655), where the 3-arg call throws a positional-parameter error on the third argument. Verified by running it. With ErrorActionPreference = Stop (line 7) the script aborts on line 12 - it never copies tmux.exe. The exe presumably got into place during the spike because the report ran under pwsh 7 (also present on this box) or was copied manually; under the default .ps1 association it is dead on arrival.

Fix: nest the calls - Join-Path (Join-Path LOCALAPPDATA "wezterm-tmux-shim") "bin" - or guard the script with a requires -Version 7 statement. The nested form works in both 5.1 and 7 and is the safer choice for an installer.

---

## WARNING

### W1. State file: no locking and non-atomic writes; corrupt read silently wipes all mappings

- src/main.rs:73-91 (load_state / save_state), 82, 90-91.

CC fires several tmux invocations in quick succession (the brief calls this out explicitly). The read-modify-write cycle (load_state then mutate next_pane / maps then save_state) has no file lock and save_state uses a bare fs::write (no temp-file plus rename). Concurrent invocations can:

- Both read the same next_pane, both allocate the same tmux id, and the last writer wins - one pane mapping is lost or two WezTerm panes collide on one tmux id.
- A reader can observe a half-written state.json mid-write. serde_json::from_str(...).unwrap_or_default() (line 82) handles that without crashing - good for the no-panic invariant - but it does so by silently resetting State to default, discarding every existing pane mapping and all stored env vars. A later save_state then persists that wiped state.

So the corrupt-should-recreate requirement is met for no-crash, but recreation means total mapping loss, which under concurrency can happen during normal operation, not just on genuine corruption.

Fix: write atomically (write to a temp file then fs::rename onto state.json) and serialize access with an OS file lock (lock file via OpenOptions share-mode, or the fs2/fd-lock crate) around the load-mutate-save critical section. At minimum, atomic rename removes the torn-read-then-wipe path.

### W2. install.ps1 lines 53-54 print garbled instructions

- scripts/install.ps1:53-54 (and line 49).

The trailing single-quote on line 53 opens a single-quoted PowerShell string that is only closed by the leading single-quote on line 54, so the two Write-Host statements collapse into one expression spanning both lines. It parses (confirmed - no parse error), but the printed guidance is mangled (literal plus-sign-and-quote fragments and a multiline blob, not the intended env:TMUX assignment). Since these lines are the user only activation instructions, the corruption defeats the script stated purpose of printing the steps to integrate.

Fix: use single-quoted literals so the dollar sign is not interpolated, and put each full assignment on one literal line. Same applies to line 49.

### W3. respawn-pane: env values with CR/LF break the generated batch script

- src/main.rs:524-525.

Only the percent sign is escaped (doubled) when emitting SET name=value lines. A stored env value containing a CR or LF would terminate the SET line early and inject the remainder as a separate batch command. CLAUDE_CODE_* values are CC-controlled rather than externally attacker-controlled, so exploitation risk is low, but a value with an embedded newline corrupts the launcher. Given the refute posture on a respawn/exec surface, this should be hardened, not assumed-safe.

Fix: reject or strip CR/LF from values before writing, or emit env via a safer mechanism (write a dotenv-style file the launcher reads, or use the quoted set form and validate values).

---

## NOTE

### N1. split-window synthetic-id fallback is unresolvable later

- src/main.rs:339-344. When wezterm cli split-pane stdout fails to parse, the handler allocates a fresh tmux id directly and bumps the counter, but does NOT insert it into tmux_to_wez / wez_to_tmux. That tmux id is then permanently unresolvable by resolve_target, so a later respawn-pane/select-pane against it falls through to first-live-pane. Fail-soft, but the id is a dead reference. Consider mapping it or re-querying via list_wez_panes.

### N2. respawn-pane command-tail re-quoting loses argument boundaries

- src/main.rs:530-532. For a multi-token command, joining cmd_parts[1..] with single spaces discards any original quoting. An argument that originally contained spaces (a path, a flag value with spaces) is silently re-split when the batch line runs. Matches the documented spike limitation, but worth tightening if respawn fidelity matters (quote each arg individually).

### N3. wezterm launcher path with spaces in send-text

- src/main.rs:540. The launcher path under LOCALAPPDATA is sent unquoted. If a username contains a space (so LOCALAPPDATA has a space), typing the bare path into cmd.exe fails. Quote the path in the send-text payload.

### N4. Confirmed-correct items (refute spot-checks that held)

- split-window mapping (spec item 3): -h maps to --horizontal (equivalent to --right, left/right) and -v maps to --bottom (top/bottom). Correct against live wezterm cli split-pane --help. New pane id is parsed from stdout (trimmed parse), matching WezTerm documented behavior of printing the pane-id on success. -P -F echoes the allocated tmux id. Good.
- WezPane deserialization (lines 151-162) matches the real wezterm cli list --format json schema; extra fields are ignored by serde and unwrap_or_default() keeps it fail-soft. Verified against live output.
- display-message current-pane resolution (lines 184-197) prefers TMUX_PANE then WEZTERM_PANE, per spec. apply_format (lines 169-180) preserves surrounding literal text via String::replace and substitutes pane_id / window_id / client_termtype / client_control_mode correctly.
- Fail-soft / no-panic: every argv/argument index access (argv[1], cmd_parts[0], filtered first two) is guarded by a length check; all serde_json::from_str use unwrap_or_default(); unknown subcommands and unknown flags hit the catch-all arm / dispatch default and exit 0. No unwrap()/expect()/panic! on untrusted input. The core invariant holds.
- uninstall.ps1 (item 8): backup-restore is correct (restores the .bak then removes it), Remove-Item -Recurse -Force is scoped to the install dir, does not touch PATH (prints a manual note instead), no auto-apply of settings. Reversible and safe. Its Join-Path calls are all 2-arg, so it does not hit the B2 problem.

---

## Review Summary  (mode: dev)

| Severity        | Count | Status |
|-----------------|-------|--------|
| CRITICAL/BLOCK  | 2     | fail   |
| HIGH/WARNING    | 3     | warn   |
| MEDIUM/NOTE     | 4     | note   |
| LOW             | 0     | pass   |

Verdict: BLOCK - fallback path literals lost their backslashes (main.rs:61,122) and install.ps1:12 uses a PowerShell-7-only 3-arg Join-Path that aborts on this machine default PowerShell 5.1; the no-panic fail-soft invariant itself holds.
