# wezterm-tmux-shim

A native Windows `tmux.exe` shim that translates the tmux subcommands Claude Code's agent-teams feature emits into `wezterm cli` calls.

## What This Is

Claude Code (CC) agent-teams mode dispatches teammates into terminal panes by shelling out to a real `tmux` binary.
Windows has no native tmux, and the macOS-side alternative (iTerm2) does not exist on Windows either.
This shim is a drop-in `tmux.exe` that sits in front of WezTerm - a native Windows terminal emulator with a scriptable CLI (`wezterm cli`) - and translates CC's tmux calls into WezTerm pane operations.

## Why This Exists

CC's agent-teams backend selection is effectively hard-coded around tmux on non-macOS platforms.
Rather than wait for native Windows support, this shim reverse-engineers the subset of the tmux CLI that CC actually calls (`new-session`, `split-window`, `list-panes`, `display-message`, `set-environment`, `respawn-pane`, and a handful more) and re-implements each one against `wezterm cli`.
See `docs/INTEGRATION_TESTING.md` for the mechanics of how CC picks this backend.

## Requirements

- Windows 10/11.
- [WezTerm](https://wezterm.org/) installed, with `wezterm.exe` either on PATH or in its default `%ProgramFiles%\WezTerm\` location.
- [Git for Windows](https://git-scm.com/download/win) installed, so `bash.exe` is available for teammate command execution (see respawn-pane in Supported Subcommands).
- Rust (MSVC toolchain) and VS Build Tools 2022, if building from source.
- Claude Code, verified against `claude 2.1.196` (see Limitations below for version-drift risk).

## Build

Requires the MSVC toolchain and VS Build Tools 2022 for the linker.
The GNU (MinGW) toolchain also compiles but its output has been seen quarantined by endpoint security on locked-down machines; prefer MSVC.

```powershell
# Install VS Build Tools 2022 with C++ workload
winget install Microsoft.VisualStudio.2022.BuildTools ...

# Set up MSVC environment (one-time per shell session)
# Adjust the MSVC version path to match your install
$VCTools = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207"
$WinSDK  = "C:\Program Files (x86)\Windows Kits\10\lib\10.0.26100.0"
$env:LIB = "$VCTools\lib\x64;$WinSDK\um\x64;$WinSDK\ucrt\x64"
$env:PATH = "$VCTools\bin\Hostx64\x64;" + $env:PATH

# Build
cd C:\Users\you\Projects\wezterm-tmux-shim
cargo build --release
# Output: target\release\tmux.exe
```

## Install

```powershell
.\scripts\install.ps1
```

The script copies `tmux.exe` to `%LOCALAPPDATA%\wezterm-tmux-shim\bin\` and prints (does NOT apply) the PATH/TMUX steps below.
It also backs up your CC `settings.json` to `settings.json.bak` in case you choose to experiment with the global setting mentioned in Activation.
You should not need that backup if you follow the recommended per-session flag instead.

## Activation

Recommended: activate per session with the `--teammate-mode` CLI flag.
Do not globally set `teammateMode` in CC's `settings.json`.

Set up the current PowerShell session, then launch CC with the flag:

```powershell
$env:PATH      = "$env:LOCALAPPDATA\wezterm-tmux-shim\bin" + [IO.Path]::PathSeparator + $env:PATH
$env:TMUX      = "wezterm-tmux-shim,0,0"
$env:TMUX_PANE = "%0"

claude --teammate-mode tmux
```

All three pieces matter.
The shim's bin directory must be on the PATH of the CC process itself, not just a child shell it later spawns.
`TMUX` must be set so CC's tmux-backend detector (`insideTmux`) reports true.
`TMUX_PANE` should match the pane id you are actually in; the shim will allocate one if it is missing, but setting it explicitly keeps CC's view of the "current pane" consistent with WezTerm's.

Rather than typing the steps above by hand each session, run [`scripts/launch-team.ps1`](scripts/launch-team.ps1).
It applies the same per-session flag and environment variables and supersedes any ad hoc personal launcher script.

### Natural activation (no manual steps each session)

The steps above are manual and per-session.
See [docs/ACTIVATION.md](docs/ACTIVATION.md) for how to fold the PATH/TMUX/TMUX_PANE setup into WezTerm or shell config so it applies automatically inside every WezTerm pane.

### Why not just flip settings.json globally

CC's `teammateMode` setting is read once per session into a cached `BackendRegistry` selection, and that cached choice sticks for the session's lifetime.
If you set `teammateMode: "tmux"` globally in `settings.json`, every interactive CC session on the machine will try to use the tmux backend, including ones whose process PATH does not have this shim, or that are not running inside WezTerm.
Those sessions fail hard with an error like "To use agent swarms, you need tmux which requires WSL," and the broken backend stays cached for that session even if you revert `settings.json` afterward.
The only fix at that point is starting a fresh session.
The per-session `--teammate-mode tmux` flag avoids all of this: it only affects the session you launch it in, and CC propagates the same flag to any teammates it spawns.

## Interactive-Only Limitation

Agent-team pane spawning only works in an interactive TTY session.
Non-interactive invocations (`-p`, piped input/output, or any non-interactive session) always force CC's in-process backend regardless of `teammateMode` or the `--teammate-mode` flag.
There is no way to get pane-based teammates out of a `-p` invocation.

## How It Works

1. `tmux.exe` (this shim) is placed first on the PATH that Claude Code uses.
2. When CC spawns `tmux has-session`, `tmux new-session`, `tmux split-window`, etc., it runs this shim.
3. The shim translates each subcommand to one or more `wezterm cli` calls.
4. A small JSON state file persists the mapping between tmux pane ids (`%N`) and WezTerm integer pane ids.
5. Every invocation is logged with arguments, wezterm commands run, stdout, and exit code to `shim.log`.

Both state and log files live in `%LOCALAPPDATA%\wezterm-tmux-shim\`.

## Verified Against

`claude 2.1.196` - this is the CC version the tmux API surface was reverse-engineered from.
The version string is embedded in the binary and printed by `tmux.exe -V`.

## Supported Subcommands

| Subcommand | Behavior |
| --- | --- |
| `has-session -t <name>` | Always exits 0 (session exists). |
| `new-session -d -s <name> [-P -F <fmt>] ...` | Maps first live WezTerm pane; prints format if -P. |
| `new-window -t <s> -n <name> -P -F <fmt> -- <cmd>` | Same as new-session (stub). |
| `split-window -d -t <target> (-h|-v) -P -F <fmt>` | Calls `wezterm cli split-pane`; maps new pane; prints tmux id. |
| `list-panes -t <t> -F <fmt>` | Calls `wezterm cli list`; prints one line per pane per format. |
| `display-message -p <fmt>` | Resolves format tokens and prints. |
| `select-pane -t <t> [-T title]` | Best-effort `wezterm cli set-tab-title`; otherwise no-op. |
| `set-option ...` | No-op (exit 0). |
| `resize-pane ...` | No-op (exit 0). |
| `respawn-pane -k -t <target> -- <cmd>` | Writes a bash `.sh` launcher plus a `.cmd` wrapper that invokes it via Git bash; sends the `.cmd` path to the pane via send-text. |
| `set-environment -g <NAME> <VALUE>` | Stores in state file. |
| `show-environment -g <NAME>` | Reads from state file. |
| `break-pane ...` | No-op (exit 0). |
| `select-layout`, `refresh-client`, `set-window-option`/`setw`, `rename-window`, `rename-session`, `move-window`, `swap-pane` | Known-accepted no-op (exit 0, no wezterm call). |
| Unknown subcommand | Logs `UNHANDLED: ...` and exits 0 (fail-soft); also prints a one-line hint to stderr. |

## Format Tokens

Supported `#{token}` values:

- `#{pane_id}` - tmux pane id of the context pane (e.g. `%3`)
- `#{window_id}` - window id (e.g. `@0`)
- `#{client_termtype}` - always `tmux-256color`
- `#{client_control_mode}` - always `0`

## Troubleshooting

Two logs cover the two sides of the integration.

CC's own backend selection: launch with `claude --debug-file <path>`, then search the file for `[BackendRegistry] Selected:`.
`Selected: tmux (running inside tmux session)` confirms CC picked the tmux backend; anything else means `TMUX` was not visible to the CC process, or `--teammate-mode tmux` was not passed.

The shim's own activity: `%LOCALAPPDATA%\wezterm-tmux-shim\shim.log` records every invocation - full argv, the `wezterm cli` command it ran, stdout, and exit code.
If CC is calling the shim but panes are not appearing as expected, this is the first place to look.

If you invoke the shim manually and a subcommand does nothing, check stderr first.
A truly unhandled subcommand prints `tmux-wezterm-shim: unhandled subcommand '<name>' (not implemented - see shim.log)`; a known-accepted no-op (see the Supported Subcommands table) does not print anything to stderr, only a `NOOP: ...` line in `shim.log`.

See `docs/INTEGRATION_TESTING.md` for a full copy-pastable walkthrough with expected output at each step.

## Uninstall

```powershell
.\scripts\uninstall.ps1
```

Removes the install directory (`bin`, `state.json`, `shim.log`) and restores the `settings.json` backup if one exists.
It does not touch PATH - if you added the shim's bin directory to a persistent PATH rather than per-session, remove it manually.

## Limitations / Version Drift

This is a Phase-1 build verified against a single CC release; treat it as unsigned, best-effort automation rather than a maintained integration.

**Verified against `claude 2.1.196` only.**
CC's internal tmux API surface is not a public contract and can change without notice in future releases.
If subcommands or argument forms change, the shim logs `UNHANDLED: ...` and exits 0 rather than crashing CC, but the corresponding feature will silently not work.
Check `shim.log` to detect drift.

**`new-session` / `new-window` are stubs.**
They map the first live WezTerm pane rather than creating a real tab or session.
CC may expect session isolation between agent teams; this does not provide it.

**Same-window targeting relies on `WEZTERM_PANE`.**
`display-message`, `list-panes -t @N`, and the session/window-name fallback in `split-window`/`respawn-pane` all resolve "the current pane" from the `WEZTERM_PANE` environment variable, and teammates are split within that pane's window.
If `WEZTERM_PANE` is missing or does not match a live pane, targeting falls back to the first pane found across all windows, which may not be the dispatching session's window.

**`respawn-pane` needs an idle shell and Git bash.**
WezTerm has no API to kill and restart a pane's process.
The shim writes a bash `.sh` launcher (the CC-supplied command is a POSIX/bash string, e.g. `cd '...' && env VAR=val '...claude.exe' ...`) plus a thin `.cmd` wrapper that invokes it via a discovered Git-for-Windows `bash.exe`, then sends the `.cmd` path to the target pane via `wezterm cli send-text --no-paste`.
This only works if the target pane is sitting at an idle shell prompt (if it is running another process, the keystrokes go to that process instead) and if Git for Windows is installed so `bash.exe` can be found.

**Teammate panes auto-close based on `remain-on-exit`.**
CC sets tmux's `remain-on-exit` option (via `set-option -p -t <pane> remain-on-exit <value>`) before each `respawn-pane`, and the shim now honors it: the generated launcher captures the teammate's exit code and runs `wezterm cli kill-pane` afterward according to the stored policy for that pane - `off` (the default when unset) closes the pane unconditionally, `failed` (what CC currently sets) closes it only on a clean exit and leaves it open on failure so the error is visible, and `on` never closes it automatically.

**Format token coverage is limited** to the tokens CC was observed to use.
Unknown `#{...}` tokens are left as-is in the output.

**Not code-signed.**
Enterprise endpoint security (CrowdStrike Falcon and similar) may block or quarantine the binary regardless of which toolchain built it.

## Debug Log

Every invocation appends to `%LOCALAPPDATA%\wezterm-tmux-shim\shim.log`.
If CC is not behaving as expected, this log is the first place to look.
It records the full argv, every `wezterm cli` call, stdout, and exit codes.

## State File

`%LOCALAPPDATA%\wezterm-tmux-shim\state.json` stores the tmux-to-WezTerm pane id mapping and any environment variables set via `set-environment`.
Delete this file to reset all id mappings.
