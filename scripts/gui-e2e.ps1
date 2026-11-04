# Live suite of the E2 screens (M2-E2-T20, docs/07 section 10/11): the debug Forgebox.exe plus a
# real forgeboxd on a temp portable root with the fixture registry, fake tools and a fake php-cgi
# (apps/desktop/e2e/live/fixture.mjs), driven over WebView2's CDP port by Playwright
# (apps/desktop/e2e/live/*.spec.ts).
#
#   powershell -NoProfile -File scripts/gui-e2e.ps1 [-Suite e2|e3] [-SkipBuild] [-KeepTemp] [-Release] [-Grep <text>]
#
# Scenarios (serial, one app session): onboarding with a fixture stack, services start/stop, site
# add, live PHP switch between two fake versions, a quick setting, an extension toggle, catalog
# install with cancel, daemon kill and restart (disconnected within 2 s, then resync without a
# reload); plus the performance mark of Start (< 50 ms) and the parity scan of every [data-action].
# WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port> is set for this test only,
# never in code or release config. Nothing touches HKCU Run, the certificate stores or NRPT: the
# onboarding scenario turns those options off. Teardown kills every process it started (GUI,
# WebView2 children, daemon, fake services, fixture servers) and removes the temp folder; the
# script fails if anything is left behind.
param(
    # e2: the E2 screens against the stock forgeboxd (M2-E2-T20). e3: the E3 screens against
    # examples/live_daemon (a forgeboxd with a memory trust store and a fake UAC, M2-E3-T21).
    [ValidateSet('e2', 'e3')][string]$Suite = 'e2',
    [switch]$SkipBuild,
    [switch]$KeepTemp,
    [switch]$Release,
    [string]$Grep = ''
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$desktop = Join-Path $repo 'apps\desktop'
$profileDir = if ($Release) { 'release' } else { 'debug' }
$exe = Join-Path $repo "target\$profileDir\Forgebox.exe"
$daemon = if ($Suite -eq 'e3') { Join-Path $repo "target\$profileDir\examples\live_daemon.exe" } else { Join-Path $repo "target\$profileDir\forgeboxd.exe" }
$forge = Join-Path $repo "target\$profileDir\forge.exe"
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

function Invoke-Cargo([string[]]$CargoArgs) {
    Push-Location $repo
    try {
        if ($Release) { cargo @CargoArgs --release } else { cargo @CargoArgs }
    } finally { Pop-Location }
}

if (-not $SkipBuild) {
    Invoke-Step 'pnpm build (dist/)' { pnpm -C $desktop build }
    Invoke-Step "cargo build Forgebox.exe ($profileDir, custom-protocol)" {
        Invoke-Cargo @('build', '-p', 'forgebox-desktop', '--features', 'custom-protocol', '--bin', 'Forgebox')
    }
    Invoke-Step "cargo build forgeboxd, forge, live_seed ($profileDir)" {
        Invoke-Cargo @('build', '-p', 'forge-daemon', '--bin', 'forgeboxd', '--example', 'live_seed', '--example', 'live_daemon')
        if ($LASTEXITCODE -eq 0) { Invoke-Cargo @('build', '-p', 'forge-cli', '--bin', 'forge') }
        if ($LASTEXITCODE -eq 0) { Invoke-Cargo @('build', '-p', 'xtask') }
    }
}
foreach ($f in @($exe, $daemon, $forge)) { if (-not (Test-Path $f)) { throw "missing $f (run without -SkipBuild)" } }

$fixtureArgs = @('create', '--repo', $repo)
if ($Release) { $fixtureArgs += '--release' }
$ctx = (node (Join-Path $desktop 'e2e\live\fixture.mjs') @fixtureArgs) | ConvertFrom-Json
if (-not $ctx.base) { throw 'fixture.mjs create printed no root' }
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
    $env:FORGE_E2E_FORGE = $forge
    $env:FORGE_E2E_PROJECT = $ctx.project
    $env:FORGE_E2E_STALL_PORT = "$($ctx.stallPort)"
    $env:FORGE_E2E_DAEMON_EXE = $daemon
    $env:FORGE_E2E_QUEUE = $ctx.queue
    $env:FORGE_E2E_SUITE = $Suite

    Write-Host '==> playwright (live suite over CDP)'
    if ($Grep -ne '') { pnpm -C $desktop e2e:live --grep $Grep } else { pnpm -C $desktop e2e:live }
    $exit = $LASTEXITCODE

    # Native frame: a black frame (exit 2) is reported only; no window / PrintWindow failure (exit 1) fails.
    $shot = Join-Path $desktop 'qa\out\gui-e2e\native-frame.png'
    $cap = & powershell -NoProfile -File $capture -ProcessId $gui.Id -Png $shot -TimeoutMs 5000
    $capExit = $LASTEXITCODE
    Write-Host "==> native frame (exit $capExit): $cap"
    # Exit 2 (black frame) stays informative; exit 1 (no window / PrintWindow failed) is a failure.
    if ($capExit -eq 1) { throw "native frame capture failed: $cap" }
}
catch {
    $failure = $_
    Write-Host "gui-e2e failed: $_" -ForegroundColor Red
    $exit = 1
}
finally {
    foreach ($v in 'FORGE_E2E_CDP_PORT', 'FORGE_E2E_EXE', 'FORGE_E2E_ROOT', 'FORGE_E2E_GUI_PID', 'FORGE_E2E_PROBE', 'FORGE_E2E_FORGE', 'FORGE_E2E_PROJECT', 'FORGE_E2E_STALL_PORT', 'FORGE_E2E_DAEMON_EXE', 'FORGE_E2E_QUEUE', 'FORGE_E2E_SUITE', 'FORGEBOX_ROOT',
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
if ($exit -eq 0) { Write-Host 'GUI E2E OK' } else { Write-Host 'GUI E2E FAILED' -ForegroundColor Red }
exit $exit
