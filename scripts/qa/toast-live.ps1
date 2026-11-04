# Live check of the T16 toast (M2-E1-T16): release Forgebox.exe + forgeboxd on a temporary portable
# root, one `tool install` (mailpit 1.32.0, network) and a read-back of the toast history of the
# PowerShell AUMID (a release exe under target\ shows toasts under it). Needs
# `cargo build --release -p forgebox-desktop --features custom-protocol -p forge-daemon` and
# target\debug\forge.exe. Windows must have notifications on (HKCU PushNotifications
# ToastEnabled != 0), or the history stays empty. Leaves no process behind.
$ErrorActionPreference = 'Continue'
$wt = (Resolve-Path "$PSScriptRoot\..\..").Path
$rel = "$wt\target\release"
$forge = "$wt\target\debug\forge.exe"
$tmp = Join-Path $env:TEMP ('fbt16-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
$root = "$tmp\root"
New-Item -ItemType Directory -Force "$root\etc", "$tmp\gui", "$tmp\cfg" | Out-Null
function FreePort { $l = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0); $l.Start(); $p = $l.LocalEndpoint.Port; $l.Stop(); $p }
Set-Content "$root\portable.flag" ''
Set-Content "$root\etc\forgebox.toml" ("[dns]`nenabled = false`n`n[ports.ranges]`ntls-crl = [$(FreePort)]`ninternal = [$(FreePort)]`n") -Encoding ascii
$env:FORGEBOX_ROOT = $root
$env:FORGE_GUI_DATA_DIR = "$tmp\gui"
$env:FORGE_CONFIG_DIR = "$tmp\cfg"
$env:FORGEBOX_OFFICIAL_REGISTRY = "$wt\registry"
$env:FORGE_DAEMON_EXE = "$rel\forgeboxd.exe"
$env:NO_COLOR = '1'
$aumid = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[void][Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
function History { [Windows.UI.Notifications.ToastNotificationManager]::History.GetHistory($aumid) }
[Windows.UI.Notifications.ToastNotificationManager]::History.Clear($aumid)
Write-Output "tmp=$tmp session=$env:SESSIONNAME"
& $forge --root $root daemon start 2>&1 | Out-String | Write-Output
$gui = Start-Process "$rel\Forgebox.exe" -PassThru
try {
    Start-Sleep 8
    Write-Output "gui alive: $(-not $gui.HasExited)"
    & $forge --root $root daemon status 2>&1 | Select-Object -First 5
    $out = & $forge --root $root tool install mailpit@1.32.0 --yes 2>&1
    $out | Select-Object -Last 5
    Start-Sleep 9
    $h = @(History)
    Write-Output "toasts in history: $($h.Count)"
    foreach ($t in $h) { Write-Output ($t.Content.GetXml()) }
}
finally {
    if (-not $gui.HasExited) { Stop-Process $gui.Id -Force }
    & $forge --root $root daemon stop 2>&1 | Out-Null
    Start-Sleep 2
    Get-Process forgeboxd, Forgebox -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$wt*" } | Stop-Process -Force
    Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -like "*fbt16*" } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
    Write-Output "tmp kept at $tmp"
}
