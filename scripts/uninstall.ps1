# uninstall.ps1 - Remove wezterm-tmux-shim and restore settings.json backup.
# Run from the repo root after install.ps1.
# Does NOT touch PATH - remove the bin dir from PATH manually if you added it.

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$InstallDir    = Join-Path $env:LOCALAPPDATA "wezterm-tmux-shim"
$CCSettings    = Join-Path $env:LOCALAPPDATA "claude-code" "settings.json"
$CCSettingsBak = "${CCSettings}.bak"

Write-Host "wezterm-tmux-shim uninstaller" -ForegroundColor Cyan

# ----- restore settings.json backup -----

if (Test-Path $CCSettingsBak) {
    Copy-Item -Path $CCSettingsBak -Destination $CCSettings -Force
    Remove-Item -Path $CCSettingsBak -Force
    Write-Host "Restored CC settings from backup." -ForegroundColor Green
} else {
    Write-Host "No settings.json backup found - skipping restore." -ForegroundColor DarkYellow
}

# ----- remove install directory (contains bin, state, log) -----

if (Test-Path $InstallDir) {
    Remove-Item -Path $InstallDir -Recurse -Force
    Write-Host "Removed: $InstallDir" -ForegroundColor Green
} else {
    Write-Host "Install directory not found - nothing to remove." -ForegroundColor DarkYellow
}

Write-Host ""
Write-Host "Uninstall complete."
Write-Host "Remove wezterm-tmux-shim bin from PATH manually if you added it."
