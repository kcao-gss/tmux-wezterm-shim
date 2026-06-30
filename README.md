# wezterm-tmux-shim

A native Windows `tmux.exe` shim that translates the tmux subcommands Claude Code's agent-teams feature emits into `wezterm cli` calls.

## Why This Exists

Claude Code (CC) agent-teams mode is hard-coded to use tmux as its multiplexer backend.
It spawns a bare `tmux` executable and issues subcommands to create panes, split windows, and pass environment variables between agents.
Windows has no native tmux.
iTerm2, the macOS-side alternative CC supports, also does not exist on Windows.

This shim sits in front of WezTerm - a native Windows terminal emulator with a rich CLI (`wezterm cli`) - and bridges CC's tmux API surface to WezTerm's actual capabilities.

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
| `respawn-pane -k -t <target> -- <cmd>` | Writes a `.cmd` launcher with env vars; sends to pane via send-text. |
| `set-environment -g <NAME> <VALUE>` | Stores in state file. |
| `show-environment -g <NAME>` | Reads from state file. |
| `break-pane ...` | No-op (exit 0). |
| Unknown subcommand | Logs `UNHANDLED: ...` and exits 0 (fail-soft). |

## Format Tokens

Supported `#{token}` values:

- `#{pane_id}` - tmux pane id of the context pane (e.g. `%3`)
- `#{window_id}` - window id (e.g. `@0`)
- `#{client_termtype}` - always `tmux-256color`
- `#{client_control_mode}` - always `0`

## Build

Requires Rust (MSVC toolchain) and VS Build Tools 2022 for the linker.

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

## Install / Uninstall

**Install (from repo root):**
```powershell
.\scripts\install.ps1
```

The script copies `tmux.exe` to `%LOCALAPPDATA%\wezterm-tmux-shim\bin\` and prints (does NOT apply) the steps to:

- Prepend the install dir to PATH
- Set `TMUX` and `TMUX_PANE` environment variables
- Set `settings.json` teammate mode to `tmux`

**Uninstall:**
```powershell
.\scripts\uninstall.ps1
```

Removes the install directory and restores the `settings.json` backup.

## Limitations / Version Drift

This is a Phase-0 spike, not a production-grade implementation.

- **new-session / new-window stubs:** These do not create real WezTerm tabs or sessions.
 They map the first live pane as a stub.
 CC may expect session isolation; this will not provide it.

- **respawn-pane via send-text:** WezTerm has no API to kill and restart a pane process.
 The shim writes a `.cmd` launcher with stored env vars and sends it to the pane via `wezterm cli send-text --no-paste`.
 The target pane must have an idle shell that accepts input.
 If the pane is occupied (running a process), the keystrokes will be delivered to that process instead.

- **Format token coverage:** Only the tokens CC was observed to use are implemented.
 Unknown tokens are left as-is in the output.

- **Version drift:** If CC emits new subcommands or changed argument forms after `claude 2.1.196`, the shim logs `UNHANDLED: ...` and exits 0.
 Check `shim.log` to detect drift.

- **Build toolchain:** The MSVC target is required on machines with endpoint security (CrowdStrike Falcon, etc.) that quarantine unsigned binaries from unknown compilers.
 The GNU (MinGW) toolchain build also works but may be blocked.

- **Code signing:** The shim is not code-signed.
 Enterprise policies may block it regardless of toolchain.

## Debug Log

Every invocation appends to `%LOCALAPPDATA%\wezterm-tmux-shim\shim.log`.
If CC is not behaving as expected, this log is the first place to look.
It records the full argv, every `wezterm cli` call, stdout, and exit codes.

## State File

`%LOCALAPPDATA%\wezterm-tmux-shim\state.json` stores the tmux-to-WezTerm pane id mapping and any environment variables set via `set-environment`.
Delete this file to reset all id mappings.
