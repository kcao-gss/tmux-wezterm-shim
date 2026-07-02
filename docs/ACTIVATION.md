# Transparent Activation

This guide shows how to make Claude Code (CC) spawn teammates into WezTerm panes automatically, without repeating the manual per-session setup described in the top-level README's Activation section.

## 1. Goal

Today, getting a teammate into a real WezTerm pane requires three manual steps every session: exporting PATH/TMUX/TMUX_PANE by hand, passing `--teammate-mode tmux` on the command line, and making sure no globally loaded skill hijacks the dispatch before it reaches the tmux backend.
The goal of this guide is to fold the first two into ordinary WezTerm and shell configuration, so that launching `claude` from inside a WezTerm pane behaves this way by default, with no extra flags or exports typed each time.
This is config you apply once to your own environment; the shim itself does not change.

## 2. Approach A (recommended): WezTerm-scoped environment

WezTerm's `set_environment_variables` config option lets you inject environment variables into every pane WezTerm spawns, without touching your shell profile or your system PATH.
Because it is scoped to WezTerm itself, it has no effect on shells opened outside WezTerm (a plain Windows Terminal tab, an SSH session, a CI runner, and so on).

Add the following to your `~/.wezterm.lua`.
Merge it into your existing config rather than overwriting the file; if you already call `config.set_environment_variables`, combine the tables instead of adding a second assignment.

```lua
local wezterm = require 'wezterm'
local config = wezterm.config_builder()

-- ... your existing config ...

config.set_environment_variables = {
  PATH = os.getenv("LOCALAPPDATA") .. "\wezterm-tmux-shim\bin;" .. os.getenv("PATH"),
  TMUX = "wezterm-tmux-shim,0,0",
  TMUX_PANE = "%0",
}

return config
```

With this in place, every pane WezTerm opens already has the shim on PATH and `TMUX` set, so CC's `insideTmux` detector reports true as soon as you run `claude` - no per-session export step needed.

## 3. Approach B (alternative): shell-profile activation gated on WezTerm

If you would rather keep WezTerm's own config untouched and manage activation from your PowerShell profile instead, gate the setup on `$env:WEZTERM_PANE` so it only fires inside a WezTerm pane and is a no-op everywhere else.

Add this to your PowerShell `$PROFILE`:

```powershell
if ($env:WEZTERM_PANE) {
    $shimBin = "$env:LOCALAPPDATA\wezterm-tmux-shim\bin"
    if ($env:PATH -notlike "*$shimBin*") {
        $env:PATH = $shimBin + [IO.Path]::PathSeparator + $env:PATH
    }
    $env:TMUX = "wezterm-tmux-shim,0,0"
    $env:TMUX_PANE = "%0"
}
```

The `-notlike` check keeps this idempotent: sourcing your profile twice in the same pane (for example by dot-sourcing it manually) will not prepend the shim's bin directory to PATH a second time.
Because the whole block is gated on `$env:WEZTERM_PANE`, opening a non-WezTerm shell (a plain `powershell.exe` window, a scheduled task, a remote session) leaves PATH and the environment untouched.

Approach A and Approach B are alternatives, not additions; pick one.
If you apply both, the effect is the same, just redundant.

## 4. The `--teammate-mode tmux` flag: open question

It is not yet confirmed whether CC auto-selects the tmux teammate backend purely from `TMUX` being set (its `BackendRegistry` "running inside tmux session" path), with no `--teammate-mode` flag at all.
If auto-detection works, Approaches A and B above are sufficient on their own and the flag becomes unnecessary.

### One-step test

With the environment from Approach A or B active in a WezTerm pane, run:

```powershell
claude --debug-file "$env:LOCALAPPDATA\wezterm-tmux-shim\cc-debug.log"
```

Deliberately omit `--teammate-mode tmux`.
Dispatch a teammate (see docs/INTEGRATION_TESTING.md Step 5 for how to prompt CC into calling `launchSwarm`), then check the debug log:

```powershell
Select-String -Path "$env:LOCALAPPDATA\wezterm-tmux-shim\cc-debug.log" -Pattern "\[BackendRegistry\]|\[TeammateModeSnapshot\]"
```

If the log shows `Selected: tmux (running inside tmux session)`, auto-detection works and no flag or alias is needed.

### Fallback if auto-detection does not fire

If the backend selection comes back as something other than tmux, add a WezTerm-gated PowerShell function that appends the flag automatically, so you still never have to type it by hand.
Add this to `$PROFILE`, after the block from Approach B (or standalone if you used Approach A):

```powershell
if ($env:WEZTERM_PANE) {
    function claude {
        & claude.exe --teammate-mode tmux @args
    }
}
```

This only shadows `claude` inside a WezTerm pane; a plain shell outside WezTerm still calls the real `claude.exe` with no flag added.
Do not set `teammateMode` globally in `settings.json` as an alternative to this - that is a documented hazard in the top-level README's Activation section, not a viable shortcut.

## 5. The superpowers skill tension

If the globally loaded `superpowers:dispatching-parallel-agents` skill is active, CC tends to satisfy a "run these in parallel" request with in-process Task subagents rather than native tmux teammates, even once the backend is correctly configured for panes.
This means pane-based teammates are not the default outcome of a parallel-work request as long as that skill is loaded; it is a skill-level routing decision, not a shim or backend problem.

Two ways to resolve this, presented neutrally - this is a user preference call, not a shim change:

- **Disable or rescope the skill** so native agent-teams wins by default for parallel requests, and pane-based teammates become the ordinary outcome of asking for parallel work.
- **Keep the skill loaded** and explicitly ask for "teammates in panes" (or equivalent phrasing) whenever you specifically want WezTerm panes, accepting that unqualified "run in parallel" requests will keep using in-process subagents.

Neither option is a shim configuration change; it is decided by which skills are loaded in your CC setup and how you phrase dispatch requests.

## 6. Verification

Once activation is in place, follow `docs/INTEGRATION_TESTING.md` end to end to confirm a real WezTerm pane appears and `shim.log` shows a successful `split-window` call.
That guide's Steps 1-4 double as a second confirmation that backend selection is working under whichever activation approach you chose here.
