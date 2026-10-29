# Real-window e2e of the desktop app (ADR-0036): builds the release Forgebox.exe and forgeboxd,
# launches the app on a temp portable root (DNS off, fake Mailpit, private GUI data dir) with
# WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<free port> (test environment only,
# never in code or release config) and attaches Playwright over CDP (apps/desktop/e2e/real).
#
#   powershell -NoProfile -File scripts/e2e-desktop.ps1 [-SkipBuild]
#
# Asserts: the daemon auto-starts and the connection reports connected; mailpit start/stop
# reach the UI through events; killing forgeboxd shows `reconnecting` and reconnects within
# 10 s; a second instance exits and focuses the first. Teardown kills every process it started
# (GUI, WebView2 children, daemon, fake services) and removes the temp folder; the script fails
# if anything is left behind.
param(
    [switch]$SkipBuild,
    [switch]$KeepTemp
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$desktop = Join-Path $repo 'apps\desktop'
$exe = Join-Path $repo 'target\release\Forgebox.exe'
$daemon = Join-Path $repo 'target\release\forgeboxd.exe'
$probe = Join-Path $repo 'scripts\qa\window-probe.ps1'
$capture = Join-Path $repo 'scripts\qa\capture-window.ps1'

# Node 24 for this process only (never the global PATH).
if (-not (Get-Command node -ErrorAction SilentlyContinue) -or ((node -v) -notmatch '^v(2[4-9]|[3-9]\d)')) {
    $laragonNode = 'C:\laragon\bin\nodejs\node-v24'
    if (Test-Path $laragonNode) { $env:Path = "$laragonNode;$env:Path" }
}
if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = '4' }
$env:PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD = '1'

function Invoke-Step([string]$Label, [scriptblock]$Block) {
    Write-Host "==> $Label"
    & $Block
    if ($LASTEXITCODE -ne 0) { throw "step failed: $Label (exit $LASTEXITCODE)" }
}

if (-not $SkipBuild) {
    Invoke-Step 'pnpm build (dist/)' { pnpm -C $desktop build }
    Invoke-Step 'cargo build Forgebox.exe (release, custom-protocol)' {
        Push-Location $repo
        try { cargo build --release -p forgebox-desktop --features custom-protocol --bin Forgebox } finally { Pop-Location }
    }
    Invoke-Step 'cargo build forgeboxd (release)' {
        Push-Location $repo
        try { cargo build --release -p forge-daemon --bin forgeboxd } finally { Pop-Location }
    }
}
foreach ($f in @($exe, $daemon)) { if (-not (Test-Path $f)) { throw "missing $f (run without -SkipBuild)" } }

$ctx = (node (Join-Path $desktop 'scripts\qa-root.mjs') create --repo $repo) | ConvertFrom-Json
if (-not $ctx.base) { throw 'qa-root.mjs create printed no root' }
$base = $ctx.base
Write-Host "==> temp root $base"

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$listener.Start(); $cdpPort = $listener.LocalEndpoint.Port; $listener.Stop()

# taskkill writes to stderr when the process is already gone, which -Stop would turn into an error.
function Stop-Tree([int]$ProcessId) { cmd /c "taskkill /PID $ProcessId /T /F >nul 2>&1" }

function Get-DaemonPid {
    $lock = Join-Path $ctx.root 'state\locks\forgeboxd.lock'
    if (Test-Path $lock) { try { return [int]((Get-Content $lock -Raw | ConvertFrom-Json).pid) } catch { } }
    return 0
}

# Processes that belong to this run: command line or image path under the temp base (WebView2
# children carry --user-data-dir=<gui>\webview), plus the GUI and daemon pids.
function Get-RunProcesses([int]$GuiPid) {
    $mine = @()
    foreach ($p in Get-CimInstance Win32_Process) {
        if ($p.ProcessId -eq $PID) { continue }
        $text = "$($p.CommandLine) $($p.ExecutablePath)"
        if ($text.Contains($base) -or $p.ProcessId -eq $GuiPid) { $mine += $p }
    }
    $dp = Get-DaemonPid
    if ($dp -gt 0) { $mine += Get-CimInstance Win32_Process -Filter "ProcessId = $dp" -ErrorAction SilentlyContinue }
    return $mine | Where-Object { $_ } | Sort-Object ProcessId -Unique
}

$exit = 1
$gui = $null
$failure = $null
try {
    $env:FORGEBOX_ROOT = $ctx.root
    $env:FORGE_GUI_DATA_DIR = $ctx.gui
    $env:FORGEBOX_OFFICIAL_REGISTRY = $ctx.registry
    $env:FORGE_DAEMON_EXE = $daemon
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$cdpPort"
    $gui = Start-Process -FilePath $exe -PassThru
    # The debugging port belongs to the first instance only.
    Remove-Item Env:\WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS

    $up = $false
    foreach ($i in 1..300) {
        try { Invoke-RestMethod "http://127.0.0.1:$cdpPort/json/version" -TimeoutSec 1 | Out-Null; $up = $true; break } catch { Start-Sleep -Milliseconds 100 }
    }
    if (-not $up) { throw "the CDP port $cdpPort never answered" }

    $env:FORGE_E2E_CDP_PORT = "$cdpPort"
    $env:FORGE_E2E_EXE = $exe
    $env:FORGE_E2E_ROOT = $ctx.root
    $env:FORGE_E2E_GUI_PID = "$($gui.Id)"
    $env:FORGE_E2E_PROBE = $probe

    Write-Host '==> playwright (real app over CDP)'
    pnpm -C $desktop e2e:real
    $exit = $LASTEXITCODE

    # Native frame, informative: a locked session is reported, not a failure of the e2e.
    $shot = Join-Path $desktop 'qa\out\e2e-desktop\native-frame.png'
    $cap = & powershell -NoProfile -File $capture -ProcessId $gui.Id -Png $shot -TimeoutMs 5000
    Write-Host "==> native frame: $cap"
}
catch {
    $failure = $_
    Write-Host "e2e-desktop failed: $_" -ForegroundColor Red
    $exit = 1
}
finally {
    foreach ($v in 'FORGE_E2E_CDP_PORT', 'FORGE_E2E_EXE', 'FORGE_E2E_ROOT', 'FORGE_E2E_GUI_PID', 'FORGE_E2E_PROBE', 'FORGEBOX_ROOT',
        'FORGE_GUI_DATA_DIR', 'FORGEBOX_OFFICIAL_REGISTRY', 'FORGE_DAEMON_EXE', 'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS') {
        Remove-Item "Env:\$v" -ErrorAction SilentlyContinue
    }
    $guiId = if ($gui) { $gui.Id } else { 0 }
    # Kill trees first (taskkill /T also takes the daemon's services), then sweep stragglers.
    foreach ($p in Get-RunProcesses $guiId) { Stop-Tree $p.ProcessId }
    $left = @()
    foreach ($i in 1..50) {
        $left = @(Get-RunProcesses $guiId)
        if ($left.Count -eq 0) { break }
        foreach ($p in $left) { Stop-Tree $p.ProcessId }
        Start-Sleep -Milliseconds 200
    }
    if ($left.Count -gt 0) {
        Write-Host "processes left behind: $(($left | ForEach-Object { "$($_.Name)#$($_.ProcessId)" }) -join ', ')" -ForegroundColor Red
        $exit = 1
    }
    if (-not $KeepTemp) {
        foreach ($i in 1..20) {
            Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
            if (-not (Test-Path $base)) { break }
            Start-Sleep -Milliseconds 250
        }
        if (Test-Path $base) { Write-Host "could not remove $base" -ForegroundColor Red; $exit = 1 }
    }
}
if ($exit -eq 0) { Write-Host 'E2E DESKTOP OK' } else { Write-Host 'E2E DESKTOP FAILED' -ForegroundColor Red }
exit $exit
