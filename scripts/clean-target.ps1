# Keeps the shared `target/` dir from eating the disk (the M1 worktrees all build into it).
# Cargo never garbage-collects stale artifacts, so this script, run periodically:
#   1. skips everything if a build holds `target/<profile>/.cargo-lock`;
#   2. removes `incremental/` (disabled in .cargo/config.toml, only legacy sessions remain);
#   3. deletes artifacts in deps/build/.fingerprint not written for -MaxAgeDays (cargo rebuilds
#      them on demand if still needed);
#   4. if free space on the target's drive is still below -MinFreeGB, removes the profile dirs.
# Register as a scheduled task with -Register (current user, no elevation).
param(
    [string]$TargetDir = (Join-Path $PSScriptRoot '..\target'),
    [int]$MaxAgeDays = 5,
    [int]$MinFreeGB = 30,
    [switch]$Register,
    [int]$EveryHours = 4
)
$ErrorActionPreference = 'Continue'

if ($Register) {
    $script = (Resolve-Path $PSCommandPath).Path
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument "-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$script`""
    $trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(5) `
        -RepetitionInterval (New-TimeSpan -Hours $EveryHours)
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -DontStopIfGoingOnBatteries -AllowStartIfOnBatteries `
        -ExecutionTimeLimit (New-TimeSpan -Hours 1)
    Register-ScheduledTask -TaskName 'Forgebox target sweep' -Action $action -Trigger $trigger `
        -Settings $settings -Description 'Prunes stale cargo artifacts in the Forgebox target dir' -Force | Out-Null
    Write-Host "registered 'Forgebox target sweep' (every $EveryHours h)"
    return
}

if (-not (Test-Path $TargetDir)) { return }
$TargetDir = (Resolve-Path $TargetDir).Path
$logFile = Join-Path $TargetDir 'clean-target.log'
function Log([string]$Msg) { "$(Get-Date -Format s) $Msg" | Add-Content -Path $logFile -Encoding utf8 }

function Get-FreeGB {
    $drive = Get-PSDrive -Name ($TargetDir.Substring(0, 1))
    [math]::Round($drive.Free / 1GB, 1)
}

# Profile dirs: debug, release and per-triple dirs (e.g. x86_64-linux-android/debug).
$profileDirs = Get-ChildItem $TargetDir -Directory -Recurse -Depth 1 |
    Where-Object { Test-Path (Join-Path $_.FullName 'deps') }

# Cargo holds an exclusive lock on .cargo-lock while building; if we can't open it, a build is running.
foreach ($p in $profileDirs) {
    $lock = Join-Path $p.FullName '.cargo-lock'
    if (-not (Test-Path $lock)) { continue }
    try {
        $fs = [System.IO.File]::Open($lock, 'Open', 'ReadWrite', 'None')
        $fs.Close()
    } catch {
        Log "skip: build in progress ($lock)"
        return
    }
}

$before = Get-FreeGB
$cutoff = (Get-Date).AddDays(-$MaxAgeDays)
foreach ($p in $profileDirs) {
    $inc = Join-Path $p.FullName 'incremental'
    if (Test-Path $inc) { Remove-Item $inc -Recurse -Force -ErrorAction SilentlyContinue }
    foreach ($sub in 'deps', 'build', '.fingerprint') {
        $dir = Join-Path $p.FullName $sub
        if (-not (Test-Path $dir)) { continue }
        Get-ChildItem $dir -Force | Where-Object { $_.LastWriteTime -lt $cutoff } |
            Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ((Get-FreeGB) -lt $MinFreeGB) {
    Log "free space below $MinFreeGB GB, removing profile dirs"
    foreach ($p in $profileDirs) { Remove-Item $p.FullName -Recurse -Force -ErrorAction SilentlyContinue }
}
Log "done: free $before GB -> $(Get-FreeGB) GB"
