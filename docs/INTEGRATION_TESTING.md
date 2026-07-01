# Integration Testing: wezterm-tmux-shim with Claude Code

This is a copy-pastable recipe to confirm that Claude Code (CC) selects the tmux backend and drives real WezTerm panes through this shim.
Run it in an interactive WezTerm GUI session - not in a piped or `-p` session, since those always force CC's in-process backend.

Verified against `claude 2.1.196`.

## Prerequisites

- WezTerm is installed and running.
- The shim is built: `target\release\tmux.exe` exists (see the Build section in the top-level README).
- `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` is set in your environment (this repo assumes it is already set via `settings.json` env; if not, set it before continuing).
- CC's `settings.json` does NOT have `teammateMode` globally set to `tmux`.
  If it does, revert it first - this recipe uses the per-session `--teammate-mode` flag instead, which is the safe activation path (see the top-level README's Activation section for why).

## Step 1: Confirm the experimental flag

Check that agent teams are enabled for CC:

```powershell
echo $env:CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS
```

Expected output: `1`.
If this is not set, agent-teams dispatch will not be offered regardless of the backend.

## Step 2: Set up the shim in the current PowerShell session

Open a pane inside WezTerm (this recipe must run inside an actual WezTerm window, since the shim shells out to `wezterm cli`).

```powershell
$env:PATH      = "$env:LOCALAPPDATA\wezterm-tmux-shim\bin" + [IO.Path]::PathSeparator + $env:PATH
$env:TMUX      = "wezterm-tmux-shim,0,0"
$env:TMUX_PANE = "%0"
```

These three lines must be set in the exact shell process that will launch `claude` in Step 3 - not in a subshell, and not in a different pane.

## Step 3: Launch CC with the tmux backend flag and a debug file

```powershell
claude --teammate-mode tmux --debug-file "$env:LOCALAPPDATA\wezterm-tmux-shim\cc-debug.log"
```

Do not use `-p` here; non-interactive sessions always force the in-process backend.

## Step 4: Verify backend selection in the debug log

In a separate pane (or after exiting CC), search the debug file:

```powershell
Select-String -Path "$env:LOCALAPPDATA\wezterm-tmux-shim\cc-debug.log" -Pattern "\[BackendRegistry\]"
```

Expected line:

```
[BackendRegistry] Selected: tmux (running inside tmux session)
```

If instead you see `isInProcessEnabled: true (non-interactive session)`, CC treated the session as non-interactive - confirm you did not launch with `-p` and that stdin/stdout are an actual TTY.
If you see a different backend selected, confirm `TMUX` was set in the exact process that launched `claude`.

## Step 5: Give CC a task that dispatches teammates

With CC running, ask it to do something that requires parallel teammates - for example, a multi-file investigation or a request that explicitly asks for parallel agents.
CC's internal tool for this is `launchSwarm` (with a `teammateCount` argument), invoked when CC's own planning decides the task benefits from parallelism.

**Open question (unconfirmed):** the exact interactive phrasing that reliably makes CC call `launchSwarm` has not been confirmed.
Backend selection (Steps 1-4 above) is fully understood and mechanical; getting CC to actually decide to dispatch a swarm is a model-judgment call CC makes on its own, and no specific prompt has been verified to trigger it consistently.
This is the remaining manual validation step for this integration - a human should experiment with task phrasing in a live session and record what works.

## Step 6: Confirm a real pane was created

If CC does dispatch a teammate, expect:

- A new WezTerm pane appears in the same window.
- `%LOCALAPPDATA%\wezterm-tmux-shim\shim.log` shows a `split-window` invocation followed by a logged `wezterm cli split-pane` call with a non-error exit code.

```powershell
Select-String -Path "$env:LOCALAPPDATA\wezterm-tmux-shim\shim.log" -Pattern "split-window|split-pane" -Context 0,2
```

## Rollback

None needed if you followed this recipe.
The environment variables in Step 2 and the `--teammate-mode` flag in Step 3 are scoped to the current shell and process; closing the shell reverts everything.
No file was written to `settings.json` and no global CC setting was changed.

## Troubleshooting

- **`[BackendRegistry] Using cached backend` appears instead of a fresh selection.**
  CC caches its backend choice per session.
  If a previous session in the same shell selected a different backend, start a brand-new `claude` invocation.
- **"To use agent swarms, you need tmux which requires WSL."**
  This means CC tried the tmux backend but `TMUX` was not visible, or the shim was not found on PATH by the CC process itself.
  Re-check Step 2 was run in the same process that launched `claude`.
- **Panes are created but CC does not seem to notice them.**
  Check `shim.log` for the exact `wezterm cli` commands and their exit codes; a non-zero exit or unexpected stdout format is the most common cause.

See the top-level README's Troubleshooting section for the general debugging workflow.
