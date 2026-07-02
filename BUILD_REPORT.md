# BUILD REPORT - wezterm-tmux-shim Phase-0 Spike

Generated: 2026-06-30
Verified against: claude 2.1.196

## What Was Built

A native Windows `tmux.exe` shim in Rust that translates CC agent-teams tmux subcommands to `wezterm cli` calls.

**Files:**

- `src/main.rs` - main Rust source (631 lines, 20 KB)
- `Cargo.toml` - Rust crate manifest (binary target name = `tmux`)
- `.cargo/config.toml` - MSVC linker configuration
- `scripts/install.ps1` - install script (copies exe, prints manual steps)
- `scripts/uninstall.ps1` - uninstall script (restores backup, removes dir)
- `README.md` - user-facing documentation
- `BUILD_REPORT.md` - this file

## Build Result

**Target:** `x86_64-pc-windows-msvc` (MSVC toolchain, VS Build Tools 2022)

```
   Compiling tmux v0.1.0
    Finished `release` profile [optimized] target(s) in 9.67s
```

**Built exe:** `target\release\tmux.exe` (414,720 bytes, PE32+ MSVC)
**Install copy:** `%LOCALAPPDATA%\wezterm-tmux-shim\bin\tmux.exe`

**Note on toolchain:** The Rust stable default toolchain on this machine is `x86_64-pc-windows-msvc`.
The GNU (MinGW) build also compiled successfully but the resulting binary was quarantined or blocked by endpoint security (EPERM when spawned via Node.js spawnSync).
The MSVC build with VS Build Tools 2022 linker runs successfully.
The GNU build is kept in `target/x86_64-pc-windows-gnu/release/tmux.exe` for reference.

## Self-Test Results

All tests run via `node scripts/run_tests.js` (Node.js spawnSync) against `%LOCALAPPDATA%\wezterm-tmux-shim\bin\tmux.exe`.
WezTerm was running with pane ids 0 (tab 0) and 9 (tab 7) at test time.

### Test 1: has-session

```
tmux.exe has-session -t claude-hidden
exit: 0
stdout: (empty)
```

PASS - exits 0 as required.

### Test 2: display-message

```
tmux.exe display-message -p #{client_termtype}
exit: 0
stdout: tmux-256color
```

PASS - prints `tmux-256color`.

### Test 3: version

```
tmux.exe -V
exit: 0
stdout: tmux-wezterm-shim (claude 2.1.196)
```

PASS - embedded version string visible.

### Test 4: list-panes against live mux

```
tmux.exe list-panes -t x -F #{pane_id}
exit: 0
stdout:
  %1
  %0
```

PASS - lists panes mapped to tmux ids.
WezTerm pane 0 -> %1, WezTerm pane 9 -> %0 (allocation order from wezterm cli list output).

### Test 5: split-window (creates real pane)

```
tmux.exe split-window -d -t %0 -h -P -F #{pane_id}
exit: 0
stdout: %2
```

PASS - called `wezterm cli split-pane --pane-id 9 --horizontal`, got back WezTerm pane id 15.
New pane was allocated as tmux %2.
A real horizontal split appeared in the WezTerm window.

### Test 6: shim.log verification

Log file exists and was written to during all tests:

- INVOKE lines recorded for all 5 test invocations
- `wezterm cmd: wezterm cli list --format json` logged with stdout
- `wezterm cmd: wezterm cli split-pane --pane-id 9 --horizontal` logged, exit=0, stdout=15
- All -> exit lines recorded

Log location: `%LOCALAPPDATA%\wezterm-tmux-shim\shim.log`

## respawn-pane / Env Handoff Approach

WezTerm has no API to kill and restart a pane process.
The shim implements `respawn-pane -k -t <target> -- <cmd>` as follows:

1. Resolve `<target>` (tmux pane id or WezTerm pane id) to a WezTerm integer pane id.
2. Write a generated `.cmd` batch script to `%LOCALAPPDATA%\wezterm-tmux-shim\respawn_N.cmd` that:
   a. Emits `SET CLAUDE_CODE_*=value` lines for all stored environment variables.
   b. Invokes the `<cmd>` tail.
3. Send `<script_path>\r` to the pane via `wezterm cli send-text --pane-id <N> --no-paste`.

**Limitation:** The target pane must be running an idle shell that accepts keyboard input.
If the pane is occupied (e.g. running a long process), the keystrokes go to that process.
WezTerm does not expose a `spawn into existing pane` or `kill pane process` API.
For CC agent-teams, panes are typically idle shells between agent invocations, so this path works in practice.

Environment variables are stored in `state.json` under `env_vars` and accumulated across `set-environment` calls.
Generic `CLAUDE_CODE_*` names are stored - any `set-environment -g NAME VALUE` call is persisted.

## State After Tests

```json
{
  "tmux_to_wez": {
    "%0": 9,
    "%1": 0,
    "%2": 15
  },
  "wez_to_tmux": {
    "9": "%0",
    "0": "%1",
    "15": "%2"
  },
  "next_pane": 3,
  "env_vars": {}
}
```

## Known Limitations (Summary)

- `new-session` / `new-window` are stubs that map the first live pane.
 No real tab or session isolation is provided.
- `respawn-pane` requires an idle shell in the target pane.
- Format token coverage is limited to the observed CC API surface.
- Not code-signed (may be blocked by corporate EDR on some machines).
- Verified against claude 2.1.196 only.

## Fix Round 1

- B1 (LOCALAPPDATA fallback): src/main.rs - missing backslashes in the raw-string fallback path corrected to `C:\Users\Default\AppData\Local`.
- B1 (wezterm.exe fallback): src/main.rs `wezterm_bin()` - replaced the broken raw-string literal with `%ProgramFiles%\WezTerm\wezterm.exe`, resolved via `std::env::var("ProgramFiles")`.
- B2 (install.ps1 3-arg Join-Path): scripts/install.ps1 - nested all 3-arg `Join-Path` calls (lines 12, 14, 16) into 2-arg `Join-Path (Join-Path ...) ...` form for PowerShell 5.1 compatibility; verify run confirmed all three call sites threw the same error, not just the one originally flagged.
- W1 (unlocked/non-atomic state file): Cargo.toml + src/main.rs - added `fs2` dependency; replaced `load_state`/`save_state` with `load_state_locked`/`save_state_locked`, which take an exclusive OS file lock on `state.lock` and write `state.json` atomically via temp-file + rename. All 6 call sites and `main()` updated.
- W2 (garbled install.ps1 instructions): scripts/install.ps1 - rewrote the PATH/TMUX/TMUX_PANE `Write-Host` lines using backtick-escaped `$` so they print literal, copy-pastable PowerShell snippets instead of string-concatenation artifacts.
- W3 (respawn-pane env value escaping): src/main.rs `cmd_respawn_pane` - env values now also strip CR/LF in addition to doubling `%`, preventing batch command injection via a stored env var containing a newline.

Fix round 1 applied. Code compiles. Self-test deferred pending WezTerm restart.

## Phase 1: Production Build

Generated: 2026-07-01
Verified against: claude 2.1.196

### NF2 fail-soft locking

`load_state_locked` previously called `.expect()` on both opening the lock file and acquiring the exclusive lock.
On a read-only `%LOCALAPPDATA%`, either call would panic and crash the shim, defeating the fail-soft design used everywhere else in `main.rs`.

Fix: the lock handle is now `Option<fs::File>`.
On open failure or lock failure, the shim logs the error via `log_line` and returns loaded-or-default state with `None` in place of the lock, rather than panicking.
`main()` already destructured the return value as `let (mut state, _lock) = load_state_locked();`, so the call site needed no change - only the inferred type of `_lock` changed.
State is still read and returned normally in this fallback path; only the exclusive-lock guarantee is given up, which only matters under concurrent shim invocations racing on the same state file.

`cargo build --release` passes after the change.

### BackendRegistry and `--teammate-mode` findings

Read-only binary analysis and live testing against `claude 2.1.196` established how CC selects its agent-teams backend:

- The backend choice is cached per session (`[BackendRegistry] Using cached backend`) once selected; it does not re-evaluate mid-session.
- The tmux backend is selected when `process.env.TMUX` is set (detector `insideTmux`); success is logged as `[BackendRegistry] Selected: tmux (running inside tmux session)`.
- Non-interactive sessions (`-p`, piped I/O) always force the in-process backend, regardless of `teammateMode`.
  Pane-based teammates only ever spawn from an interactive TTY session.
- CC exposes a per-invocation CLI flag, `--teammate-mode <tmux|iterm2|in-process|auto>`, which it also propagates to any teammates it spawns.

### The global-flip hazard

Setting `teammateMode: "tmux"` globally in CC's `settings.json` was tested live and confirmed harmful: it makes every interactive CC session attempt the tmux backend, including sessions whose process PATH lacks the shim.
Those sessions fail hard ("To use agent swarms, you need tmux which requires WSL"), and the broken backend selection is cached for the session's remaining lifetime - reverting `settings.json` afterward does not recover an already-broken session; only a fresh session does.

Recommendation adopted throughout the docs: use the per-session `--teammate-mode tmux` flag plus session-scoped `PATH`/`TMUX`/`TMUX_PANE`, and never flip the global setting.

### Docs added

- `README.md`: rewritten for production use - requirements, build, install, an Activation section covering the safe per-session flag and the global-flip hazard, the interactive-only (`-p`) limitation, troubleshooting via `--debug-file` and `shim.log`, uninstall, and an expanded Limitations/Version Drift section.
- `docs/INTEGRATION_TESTING.md`: new file - a copy-pastable human recipe for confirming `[BackendRegistry] Selected: tmux` and a real WezTerm pane spawn, including the open question that the exact prompt phrasing needed to trigger `launchSwarm` is still unconfirmed and remains the one manual validation step.

### Known gap carried forward

`scripts/install.ps1` still prints a step recommending a global `settings.json` teammateMode change; this predates the Phase 1 findings above and was out of scope for this build (not listed in the Phase 1 task brief).
It should be revisited to align with the per-session activation guidance now documented in the README.

### Fix Round 2: teammates never start (empty panes)

Live testing of the native agent-teams path (via `shim.log`) found two bugs that together left every spawned teammate pane idle.

**Global-flag parsing.**
CC's native teammate path invokes the shim as `tmux.exe -S wezterm-tmux-shim <subcommand> ...`, reconstructing the socket name from the `TMUX` env var on every call.
`main()` previously read `argv[1]` directly as the subcommand, so it read `-S` instead and every call fell through to the `UNHANDLED` fail-soft path.
Fix: `main()` now skips tmux's global options (`-S`, `-L`, `-f`, `-c`, `-T` consume the following argument; `-2`, `-C`, `-CC`, `-D`, `-l`, `-q`, `-u`, `-v` are booleans) before reading the subcommand.

**`respawn-pane` wrote a batch file for a bash command.**
The native flow is `split-window -d -t <pane> (-v|-h) -- cat` (discarding the `cat`, leaving an idle shell), then `respawn-pane -k -t <pane> -- "<CMD>"` where `<CMD>` is a POSIX/bash command string built by CC, for example `cd '...' && env VAR=val '...\claude.exe' --agent-id ...`.
`cmd_respawn_pane` wrote this verbatim into a `.cmd` batch file and delivered it via `wezterm cli send-text`.
cmd.exe cannot parse bash syntax (`env VAR=val`, single-quoted paths, `&&` inside single-quoted segments the way bash does), so the launcher silently failed and the pane stayed idle.
Fix: `cmd_respawn_pane` now writes two files per respawn - `respawn_<id>.sh` (LF line endings, a shebang, `export NAME='value'` lines for each stored `env_vars` entry with bash single-quote escaping, then the raw `<CMD>` string unmodified) and `respawn_<id>.cmd` (a thin `@echo off` wrapper that invokes a discovered `bash.exe` on the `.sh` path).
Delivery is unchanged: the `.cmd` path plus `\r` is still sent via `wezterm cli send-text --pane-id <id> --no-paste`, since a `.cmd` always runs under cmd.exe regardless of the shell state of the target pane.
A new `bash_bin()` helper resolves the Git-for-Windows `bash.exe` to invoke, in order: `$SHELL` if it names an existing file, `C:\Program Files\Git\bin\bash.exe`, `C:\Program Files\Git\usr\bin\bash.exe`, then bare `bash` on PATH.
If none are found, the shim logs the failure and still writes the launcher files (fail-soft) rather than panicking.

Verified by generating a launcher against a scratch state directory and executing both the `.sh` directly under `bash.exe` and the `.cmd` wrapper directly under `cmd.exe`; both correctly exported stored env vars (including a value containing an embedded single quote) and ran the tail command.
`cargo build --release` passes with no warnings after both fixes.

### Fix Round 3: teammates spawn into the wrong WezTerm window

Live testing with two WezTerm windows open (one running the dispatching `claude` session, one unrelated) found teammates landing in the unrelated window instead of the dispatching session's own window.
`shim.log` traced the exact sequence: `display-message -t %0 -p '#{window_id}'` returned `@0` even though the dispatching claude process had `WEZTERM_PANE=7` in window 1, then `list-panes -t @0` returned panes from every window (not just window 0), then `split-window -t %2` resolved `%2` to a wezterm pane in window 0 and split there.
`state.json` showed `%0` stale-bound to wezterm pane 1 (window 0) from an earlier session, rather than to pane 7 (window 1), the pane the current process was actually launched in.
Three root causes combined to produce this:

**`cmd_list_panes` ignored its `-t @N` window filter.**
It always listed every live pane across every window.
Fix: `cmd_list_panes` now parses `-t @N` and, when the target is a window id, only lists panes whose wezterm `window_id` matches `N`. Panes are also marked with `#{pane_active}`/`#{pane_current}` set to `1` for the pane bound to `WEZTERM_PANE`, `0` otherwise, so CC can pick out its own pane from the list.

**The "current pane" was not anchored to `WEZTERM_PANE`.**
`resolve_current_pane` previously trusted `TMUX_PANE` unconditionally, but `TMUX_PANE` is reconstructed by CC from an earlier `display-message` reply and can go stale across sessions, leaving it bound to the wrong wezterm pane (and thus the wrong window).
Fix: `resolve_current_pane` now treats `WEZTERM_PANE` as authoritative. When both env vars are present, it force-rebinds `TMUX_PANE`'s tmux id to the real `WEZTERM_PANE` wezterm pane via a new `State::bind_pane` helper (which also evicts the old, now-incorrect mapping on both sides) before returning it. `cmd_display_message` was also fixed to report the `window_id` of this resolved current pane instead of unconditionally reporting the first pane in `wezterm cli list`'s output.

**`resolve_target`'s session/window-name fallback always picked the first pane found.**
`wezterm cli list --format json` lists windows in creation order, so "first pane" meant "first pane of window 0" regardless of which window the dispatching session was actually in.
Fix: `resolve_target` now resolves the current pane's window via `resolve_current_pane` first, and returns a pane from that window when one exists; it falls back to the previous first-pane behavior only if the current pane cannot be resolved.

Verified against a genuine two-window WezTerm instance (a scratch `LOCALAPPDATA` state directory, never the real one): with `WEZTERM_PANE` set to a pane in the second window, `list-panes -t @<other-window>` excluded the second window's pane, `list-panes -t @<own-window>` returned only the second window's pane, and `split-window -t <name>` (forcing the name-fallback path in `resolve_target`) created its new pane inside the second window alongside `WEZTERM_PANE`, not in window 0.
`cargo build --release` passes with no warnings after this fix.

### Fix Round 4: teammate panes never auto-closed

`cmd_split_window` creates the pane running the default shell, and `cmd_respawn_pane` delivers the teammate launcher by `wezterm cli send-text` into that shell, so the teammate process runs as a child of the persistent shell.
When the teammate exits, control returns to the still-alive shell and the pane stays open forever, regardless of how the teammate exited.
Separately, CC issues `set-option -p -t <pane> remain-on-exit failed` immediately before each `respawn-pane` (confirmed in `shim.log`), but `cmd_set_option` was a hard no-op, so this teardown intent was silently dropped.

**`cmd_set_option` now tracks `remain-on-exit`.**
It parses `-t <target>` plus the trailing `<name> [value]` positional pair, resolves `<target>` through the existing `resolve_target`, and, only when `<name>` is `remain-on-exit`, stores `<value>` in a new `State.remain_on_exit: HashMap<u64, String>` field keyed by wezterm pane id.
The field carries `#[serde(default)]` so existing `state.json` files without it still deserialize.
Every other option name (`pane-border-style`, `window-style`, `pane-active-border-style`, `pane-border-format`, etc.) remains a no-op, unchanged from before.

**`cmd_respawn_pane` now appends an auto-close teardown to the generated `.sh`.**
After the teammate command line, it looks up `state.remain_on_exit.get(&wez_target)` (defaulting to `"off"`, tmux's own default, when the pane was never given a policy) and appends `rc=$?` followed by:
- `"off"` (or any unset/unrecognized value): `wezterm cli kill-pane --pane-id <id>` unconditionally - pane always closes.
- `"failed"` (what CC actually sets): `if [ "$rc" -eq 0 ]; then wezterm cli kill-pane --pane-id <id>; fi` - pane closes only on a clean exit; a failing teammate leaves its pane open so the user can read the error.
- `"on"`: nothing is appended - pane never auto-closes.

The env-export lines, the raw command line, and the `.cmd` wrapper + `send-text` delivery are all unchanged.

Verified by exercising `cmd_set_option` with the exact argv CC sends (`set-option -p -t %N remain-on-exit failed`) followed by `cmd_respawn_pane`, and inspecting the generated `.sh` for all three `remain-on-exit` values in a scratch state directory (never the real one).
`cargo build --release` passes with no warnings after this fix.

### Fix Round 5: kill-pane/kill-window never closed teammate panes

`cmd_split_window` and `cmd_respawn_pane` were already correct, but the dispatch table routed `kill-pane`, `kill-window`, and `kill-session` to the shared UNHANDLED no-op path.
When CC or the user dismisses a teammate, or a teammate finishes its task, the teammate `claude` process stays alive and idle rather than exiting, so the remain-on-exit teardown appended in `cmd_respawn_pane` (Fix Round 4) never fires for it.
The real close signal is CC issuing `tmux kill-pane`, which the shim was silently dropping, leaving the WezTerm pane open forever.

**`cmd_kill_pane` is now handled.**
It parses `-t <target>` (a tmux pane id like `%N`, a bare wezterm integer, or a window id `@N` resolved through the existing name/window fallback in `resolve_target`), defaulting to the current pane via `resolve_current_pane` when `-t` is absent, matching tmux's own default.
It resolves the target to a wezterm pane id and runs `wezterm cli kill-pane --pane-id <id>`, logging the command and the wezterm exit code.
It is fail-soft throughout: an unresolvable target or a nonzero wezterm exit is logged and the shim still returns exit 0, never panicking or crashing CC.
A new `State::forget_pane` helper then removes the pane's `tmux_to_wez`/`wez_to_tmux` mapping and any `remain_on_exit` entry, best-effort and unconditional on the wezterm call's outcome, so a pane the shim believes is gone does not linger in `state.json` and get reused for a different pane later.

**`cmd_kill_window` is now handled, best-effort.**
It parses `-t @N` directly as a window id, or resolves a pane-style target to its window via `wezterm cli list`, then enumerates every live pane in that window and calls `wezterm cli kill-pane --pane-id <id>` on each, since WezTerm has no single "kill window" call.
Each killed pane also goes through `State::forget_pane`.

**`kill-session` intentionally remains on the UNHANDLED no-op path.**
The shim tracks no concept of a tmux "session" as a specific set of panes; honoring `kill-session` would mean mass-killing every pane the shim knows about, which could tear down unrelated WezTerm panes or windows the user still cares about.
A comment at the dispatch site documents this so the safety property is not silently lost in a future edit.

This is the real teammate-pane close path.
The remain-on-exit launcher teardown added in Fix Round 4 only covers processes that actually exit on their own; it is unchanged and still correct for short-lived commands, but it does not help for a dismissed or finished teammate whose `claude` process stays running.

Verified against a scratch `LOCALAPPDATA` state directory (never the real one): `kill-pane -t %N` resolved the seeded tmux id to its wezterm pane id, emitted `wezterm cli kill-pane --pane-id <id>`, and removed the pane's `tmux_to_wez`/`wez_to_tmux`/`remain_on_exit` entries from `state.json` even when the underlying `wezterm` call failed (pane already gone).
`kill-window -t @<nonexistent>` correctly matched zero real panes against a live WezTerm instance and killed nothing.
`cargo build --release` passes with no warnings after this fix.
