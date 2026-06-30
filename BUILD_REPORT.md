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
