# install.ps1 - Install wezterm-tmux-shim into a scoped directory.
# This script copies tmux.exe and PRINTS (does not auto-apply) the steps
# to integrate it with Claude Code agent-teams. Run it from the repo root.
# Safe to re-run: overwrites an existing install.

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ----- paths -----

$RepoRoot   = $PSScriptRoot | Split-Path
$InstallDir = Join-Path (Join-Path $env:LOCALAPPDATA "wezterm-tmux-shim") "bin"
$StateDat   = Join-Path $env:LOCALAPPDATA "wezterm-tmux-shim"
$ExeSrc     = Join-Path (Join-Path $RepoRoot "target") (Join-Path "release" "tmux.exe")
$ExeDst     = Join-Path $InstallDir "tmux.exe"
$CCSettings = Join-Path (Join-Path $env:LOCALAPPDATA "claude-code") "settings.json"

Write-Host "wezterm-tmux-shim installer" -ForegroundColor Cyan
Write-Host "Repo root : $RepoRoot"
Write-Host "Install   : $InstallDir"
Write-Host ""

# ----- verify the built exe exists -----

if (-not (Test-Path $ExeSrc)) {
    Write-Error "tmux.exe not found at $ExeSrc. Build with: cargo build --release"
    exit 1
}

# ----- copy exe -----

if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir | Out-Null
    Write-Host "Created directory: $InstallDir"
}

Copy-Item -Path $ExeSrc -Destination $ExeDst -Force
Write-Host "Installed: $ExeDst" -ForegroundColor Green

# ----- PRINT (do not apply) the manual integration steps -----

Write-Host ""
Write-Host "-------------------------------------------------------------"
Write-Host "MANUAL STEPS TO ACTIVATE (not applied automatically):"
Write-Host "-------------------------------------------------------------"
Write-Host ""
Write-Host "1. Prepend the install dir to PATH in your CC launch context."
Write-Host "   For the current PowerShell session only:"
Write-Host "   `$env:PATH = `"$InstallDir`" + [IO.Path]::PathSeparator + `$env:PATH"
Write-Host ""
Write-Host "2. Set TMUX and TMUX_PANE so CC TmuxBackend sees a live session."
Write-Host "   Run: wezterm cli list  to find your pane id (integer), then:"
Write-Host "   `$env:TMUX     = `"wezterm-tmux-shim,0,0`""
Write-Host "   `$env:TMUX_PANE = `"%0`""
Write-Host ""
Write-Host "3. Set CC teammate mode to tmux in CC settings.json:"
Write-Host "   File: $CCSettings"
Write-Host "   Add/update: { teammateMode: tmux }"
Write-Host "   Backup at: ${CCSettings}.bak"
Write-Host ""

# ----- back up settings.json if it exists -----

if (Test-Path $CCSettings) {
    $Backup = "${CCSettings}.bak"
    Copy-Item -Path $CCSettings -Destination $Backup -Force
    Write-Host "Backed up settings to: $Backup" -ForegroundColor Yellow
} else {
    Write-Host "CC settings.json not found - skipping backup." -ForegroundColor DarkYellow
}

Write-Host ""
Write-Host "State and log files will be in: $StateDat"
Write-Host "Install complete. Follow MANUAL STEPS above to activate." -ForegroundColor Cyan
