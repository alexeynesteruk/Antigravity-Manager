# Antigravity Tools - logon keep-alive script.
# Registered as a Scheduled Task ("Antigravity Tools - Logon Keepalive").
# At logon (and on every repetition) it starts Antigravity Tools if it is not
# already running. If the app is not installed yet, it exits silently, so the
# task starts working as soon as the app appears in one of the known locations.

$ErrorActionPreference = 'SilentlyContinue'

# Known install locations, in priority order.
# - NSIS currentUser install:  %LOCALAPPDATA%\Antigravity Tools
# - NSIS perMachine install:  %ProgramFiles%\Antigravity Tools
# - NSIS legacy path:         %LOCALAPPDATA%\Programs\Antigravity Tools
# - Portable / dev build:     repo src-tauri target
$candidates = @(
    (Join-Path $env:LOCALAPPDATA 'Antigravity Tools\antigravity-tools.exe'),
    (Join-Path $env:ProgramFiles 'Antigravity Tools\antigravity-tools.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Antigravity Tools\antigravity-tools.exe'),
    'D:\development\Antigravity-Manager\src-tauri\target\release\antigravity-tools.exe',
    'D:\development\Antigravity-Manager\src-tauri\target\debug\antigravity-tools.exe'
)

$exe = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $exe) { exit 0 }

# The process image name is the exe file name without the directory.
$procName = [System.IO.Path]::GetFileNameWithoutExtension($exe)

$running = Get-Process -Name $procName -ErrorAction SilentlyContinue
if ($running) { exit 0 }

Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) | Out-Null
exit 0
