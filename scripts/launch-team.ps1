# launch-team.ps1 - Safe, version-controlled launcher for Claude Code (CC)
# with the wezterm-tmux-shim agent-teams backend active.
#
# What this does: sets PATH/TMUX/TMUX_PANE for THIS PROCESS ONLY, then
# launches claude with the per-session `--teammate-mode tmux` flag.
#
# Why per-session instead of settings.json: CC's `teammateMode` setting,
# if set globally in settings.json, is cached into every CC session's
# BackendRegistry for that session's lifetime, including sessions that
# are not running inside WezTerm or do not have this shim on PATH.
# Those sessions then fail hard ("To use agent swarms, you need tmux
# which requires WSL") and stay broken until a fresh session is started.
# See the top-level README's "Why not just flip settings.json globally"
# section for the full explanation.
#
# This script supersedes any pre-existing personal copy at
# %LOCALAPPDATA%\wezterm-tmux-shim\launch-team.ps1.
# It does not modify settings.json, PATH persistently, or any file
# outside this process's environment.
#
# Usage:
#   .\scripts\launch-team.ps1 [-- additional claude args]
#   .\scripts\launch-team.ps1 --debug-file C:\path\to\debug.log

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ----- configuration -----

# Optional fallback path to claude.exe, used only if `claude` is not
# found on PATH. Leave empty to require PATH resolution.
# Example: $ClaudeExe = "C:\Users\you\.local\bin\claude.exe"
$ClaudeExe = ""

# ----- guard: must run inside a WezTerm pane -----

if (-not $env:WEZTERM_PANE) {
    Write-Warning "This script must be run from inside a WezTerm pane (WEZTERM_PANE is not set)."
    Write-Warning "Open a pane in WezTerm and re-run this script from there."
    exit 1
}

# ----- locate claude.exe -----

$ResolvedClaude = $null
$ClaudeCmd = Get-Command claude -ErrorAction SilentlyContinue
if ($ClaudeCmd) {
    $ResolvedClaude = $ClaudeCmd.Source
} elseif ($ClaudeExe -and (Test-Path $ClaudeExe)) {
    $ResolvedClaude = $ClaudeExe
}

if (-not $ResolvedClaude) {
    Write-Error "Could not find claude.exe on PATH, and `$ClaudeExe fallback is not set or does not exist."
    Write-Error "Either add claude to PATH, or edit `$ClaudeExe near the top of this script."
    exit 1
}

# ----- set session-scoped environment (this process only) -----

$ShimBin = Join-Path (Join-Path $env:LOCALAPPDATA "wezterm-tmux-shim") "bin"
$env:PATH = $ShimBin + [IO.Path]::PathSeparator + $env:PATH
$env:TMUX = "wezterm-tmux-shim,0,$env:WEZTERM_PANE"
$env:TMUX_PANE = "%0"

if (-not $env:CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS) {
    $env:CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS = "1"
}

# ----- status -----

Write-Host "wezterm-tmux-shim launcher" -ForegroundColor Cyan
Write-Host "This process only. settings.json is NOT modified." -ForegroundColor Cyan
Write-Host "Safe to run from any WezTerm pane, in any project."
Write-Host ""
Write-Host "PATH prepended : $ShimBin"
Write-Host "TMUX           : $env:TMUX"
Write-Host "TMUX_PANE      : $env:TMUX_PANE"
Write-Host "AGENT_TEAMS    : $env:CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"
Write-Host "claude.exe     : $ResolvedClaude"
Write-Host "Launch flag    : --teammate-mode tmux (per-session, not settings.json)"
Write-Host ""

# ----- launch -----

& $ResolvedClaude --teammate-mode tmux @args
