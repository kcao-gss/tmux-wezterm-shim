# VERIFY REPORT FINAL - wezterm-tmux-shim Phase-0 Spike

Verdict: WARNING

Mode: dev.
Posture: refute (score-7 goal).
Reviewed: src/main.rs (655 lines), Cargo.toml, scripts/install.ps1, scripts/uninstall.ps1, against BUILD_REPORT.md "Fix Round 1" and the prior VERIFY_REPORT.md.
Method: read each fixed file in full; checked the exact line each fix claims to touch; re-derived the PowerShell 5.1 Join-Path semantics that drove B2.

All six claimed fixes are VERIFIED.
The WARNING is for ONE new defect of the same class as B2 that the fix round did not cover: a 3-arg `Join-Path` still present in `uninstall.ps1:9`.
This does not block the live spike test (install + run path is clean); it only breaks clean uninstall on Windows PowerShell 5.1.

---

## Fix verification

### B1 - LOCALAPPDATA fallback backslashes - VERIFIED

src/main.rs:62 reads `r"C:\Users\Default\AppData\Local"` with all backslashes present.
The raw-string corruption from the prior review is gone; `state_dir()` now produces a valid path on the fallback leg.

### B1 - WezTerm fallback backslashes - VERIFIED

src/main.rs:142-144 no longer hardcodes a corrupted literal.
It resolves `ProgramFiles` (`std::env::var("ProgramFiles")`, default `r"C:\Program Files"`) and builds the path with `format!(r"{}\WezTerm\wezterm.exe", prog_files)`.
Backslashes are correct and the path is now env-derived as the prior review recommended.

### B2 - install.ps1 3-arg Join-Path - VERIFIED

scripts/install.ps1:12 is `Join-Path (Join-Path $env:LOCALAPPDATA "wezterm-tmux-shim") "bin"` (nested, 2-arg-per-call).
All other call sites were checked: line 13 is 2-arg, line 14 is nested, line 15 is 2-arg, line 16 is nested.
No 3-arg `Join-Path` remains in install.ps1.
The installer now runs end-to-end on Windows PowerShell 5.1 under `ErrorActionPreference = Stop`.

### W1 - atomic state + locking - VERIFIED

- `load_state_locked()` exists (src/main.rs:76): opens `state.lock` with read/write/create and calls `lock_exclusive()` (fs2) before reading state.json.
- `save_state_locked()` (src/main.rs:103): serializes to `state.json.tmp`, then `fs::rename` onto `state.json` - atomic write.
- `main()` (src/main.rs:650) calls `load_state_locked()` and binds the returned lock handle to `_lock`, holding the exclusive lock across the whole `dispatch` -> `save_state_locked` critical section, then releasing it at process exit.
- All save sites use `save_state_locked` (lines 273, 307, 377, 404, 443, 584). No bare `save_state`/`load_state` remain.
- Cargo.toml:14 adds `fs2 = "0.4"`.

The torn-read-then-wipe race and the duplicate-id allocation race from the prior W1 are both closed: only one process at a time holds the lock across read-modify-write, and the write is atomic.

### W2 - install.ps1 instructions - VERIFIED

scripts/install.ps1:49, 53, 54 use backtick-escaped `` `$ `` inside double-quoted `Write-Host` strings, so they print literal, copy-pastable snippets:
- line 49 prints `$env:PATH = "<InstallDir>" + [IO.Path]::PathSeparator + $env:PATH`
- line 53 prints `$env:TMUX     = "wezterm-tmux-shim,0,0"`
- line 54 prints `$env:TMUX_PANE = "%0"`

The `$InstallDir` interpolation on line 49 is intentionally NOT escaped (it correctly substitutes the real path), while the literal `$env:` references are escaped. The prior single-quote-collapse garbling is gone.

### W3 - respawn env CR/LF escaping - VERIFIED

src/main.rs:547: `let escaped_v = v.replace('%', "%%").replace('\r', "").replace('\n', "");`
Env values now strip CR and LF in addition to doubling `%` before being emitted as `SET name=value` lines, closing the batch-injection-via-embedded-newline path.

---

## New findings

### NF1 (MEDIUM) - uninstall.ps1:9 still uses a 3-arg Join-Path (same class as B2)

- scripts/uninstall.ps1:9 - `$CCSettings = Join-Path $env:LOCALAPPDATA "claude-code" "settings.json"`

This is the identical defect that B2 fixed in install.ps1, left unfixed in uninstall.ps1.
Three positional path arguments invoke the `-AdditionalChildPath` form that only exists in PowerShell 6+.
On this machine default Windows PowerShell 5.1, under `ErrorActionPreference = Stop` (line 6), this line throws a positional-parameter error and the uninstaller aborts before restoring the settings.json backup or removing the install dir.

Note: install.ps1:16 builds the same `$CCSettings` value with the correct nested form `Join-Path (Join-Path $env:LOCALAPPDATA "claude-code") "settings.json"`, so the two scripts are now inconsistent.
The prior VERIFY_REPORT N4 asserted uninstall.ps1 "Join-Path calls are all 2-arg" - that was incorrect; line 9 is and was 3-arg.

Fix: nest it - `Join-Path (Join-Path $env:LOCALAPPDATA "claude-code") "settings.json"`.

Impact on the spike: NONE for install or runtime. Uninstall is a post-spike cleanup path, so this does not block the live test, but it should be fixed before the script is relied on for teardown.

### NF2 (LOW) - load_state_locked uses .expect() on lock-file open/acquire

- src/main.rs:84 (`.expect("failed to open state lock file")`) and src/main.rs:87 (`.expect("failed to acquire exclusive lock on state.lock")`).

These are panic vectors, which is a deviation from the otherwise strict fail-soft / no-panic invariant. They are on shim-owned infrastructure (the shim's own state dir / lock file), NOT on attacker- or CC-controlled argv/env input, so they do not regress the core invariant the spike cares about. But a locked-down or read-only `%LOCALAPPDATA%` would make the shim panic (non-zero exit) instead of degrading gracefully, which on a CC tmux call path means CC could see a crash rather than a silent success. Consider matching the rest of the file's `unwrap_or` / `let _ =` fail-soft style: on lock failure, log and proceed with `State::default()` rather than panicking.

This is a pre-existing trait of the W1 fix as written, not a new regression beyond it - flagged at LOW because the brief asked for a fail-soft spot-check.

---

## Fail-soft spot-check (refute)

No new fail-soft violations were introduced in the changed code beyond NF2 above:
- W3's added `.replace(...)` calls are infallible.
- save_state_locked uses `unwrap_or_default()` on serialization and `let _ =` / `.is_ok()` guards on all I/O - no panic path.
- The B1 `format!`/`unwrap_or_else` fallbacks are infallible.
- All argv/env handling, serde parsing, and dispatch retain the guarded, `unwrap_or_default()`, exit-0-on-unknown behavior confirmed in the prior review's N4. No new `unwrap()`/`expect()`/`panic!` on attacker- or CC-controlled input was added (the only two `.expect()` calls are the lock-file ones in NF2, on shim-owned infra).

---

## Review Summary  (mode: dev)

| Severity        | Count | Status |
|-----------------|-------|--------|
| CRITICAL/BLOCK  | 0     | pass   |
| HIGH/WARNING    | 0     | pass   |
| MEDIUM          | 1     | warn   |
| LOW             | 1     | note   |

Verdict: WARNING - all six Fix-Round-1 fixes (B1 x2, B2, W1, W2, W3) are independently VERIFIED in the current source; the code is ready for the live spike test. One same-class defect remains (uninstall.ps1:9 3-arg Join-Path, MEDIUM) plus a LOW fail-soft note on the lock-file .expect() calls - neither blocks the install or runtime spike path; both should be fixed before relying on teardown / locked-down-profile robustness.
