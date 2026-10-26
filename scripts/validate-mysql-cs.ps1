<#
.SYNOPSIS
  Empirical validation for docs/15 section 5 (M1-E1-T2): do MySQL 8.4 and MariaDB 11 start
  with lower_case_table_names=0 on a case-sensitive NTFS data directory?

.DESCRIPTION
  MANUAL VALIDATION SCRIPT, not part of the product. It marks the data dir through
  NtSetInformationFile (like forge-core; `fsutil` needs elevation). For each server it:
    1. downloads the official zip into the cache (sha256 checked against the curated values),
    2. extracts it under <TmpDir>\validate-<id>-<stamp>\,
    3. creates an empty data dir and marks it case-sensitive (NtSetInformationFile, no elevation),
    4. renders a my.ini with the same keys as crates/forge-render/templates/<id>/my.ini.j2,
    5. initializes the data dir, starts the server on a free port,
    6. creates tables `Users` and `users` and checks that both exist,
    7. stops the server.
  It prints a PASS/FAIL summary per server. On FAIL with lower_case_table_names=0 it repeats
  with =2 and reports that too (fallback of docs/15 section 5).

  Keep the my.ini keys below in sync with the templates.

.EXAMPLE
  pwsh -File scripts/validate-mysql-cs.ps1
  pwsh -File scripts/validate-mysql-cs.ps1 -Only mysql
#>
[CmdletBinding()]
param(
    [string]$CacheDir = 'C:\apps_src\Forgebox-wt\cache',
    [string]$TmpDir = 'C:\apps_src\Forgebox-wt\tmp',
    [ValidateSet('all', 'mysql', 'mariadb')][string]$Only = 'all',
    # 0 (default): the validation of docs/15 section 5 (with the =2 fallback on FAIL).
    # 2: diagnostic, runs only lower_case_table_names=2 on the case-sensitive data dir.
    [ValidateSet(0, 2)][int]$Setting = 0,
    [int]$PortFrom = 33070,  # 33060 is the default MySQL X protocol port, often taken (Laragon)
    [int]$PortTo = 33199,
    # Negative control: leave the data dir case-insensitive (no NTFS flag). PASS then only means
    # "the server started and both tables could not coexist / effective value is reported".
    [switch]$NoCaseSensitiveFlag,
    [switch]$KeepFiles
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Servers = @(
    @{
        Id = 'mysql'; Name = 'MySQL'; Version = '8.4.13'
        File = 'mysql-8.4.13-winx64.zip'
        Url = 'https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-8.4.13-winx64.zip'
        Sha256 = 'e9cd98b1d23f122987b1afca26ad942efec5891481103d48ae3cde237fa3ea54'
        Server = 'mysqld.exe'; Client = 'mysql.exe'; Admin = 'mysqladmin.exe'
    },
    @{
        Id = 'mariadb'; Name = 'MariaDB'; Version = '11.8.9'
        File = 'mariadb-11.8.9-winx64.zip'
        Url = 'https://archive.mariadb.org/mariadb-11.8.9/winx64-packages/mariadb-11.8.9-winx64.zip'
        Sha256 = '830c46727d9278eae212ae3eca44eeb9e71b2a68704e95f344a64fba7b1963f5'
        Server = 'mariadbd.exe'; Client = 'mariadb.exe'; Admin = 'mariadb-admin.exe'
    }
)


# `fsutil file setCaseSensitiveInfo` needs elevation on this machine ("Acesso negado"), while
# NtSetInformationFile(FileCaseSensitiveInformation = class 71) works unelevated, exactly like
# the product does (forge-core WindowsCaseSensitiveFs, docs/15 section 3). Same call here.
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class ForgeCs {
    [StructLayout(LayoutKind.Sequential)] struct IoStatus { public IntPtr Status; public IntPtr Info; }
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr sa, uint disp, uint flags, IntPtr tmpl);
    [DllImport("ntdll.dll")] static extern int NtSetInformationFile(SafeFileHandle h, out IoStatus io, ref uint info, uint len, int cls);
    [DllImport("ntdll.dll")] static extern int NtQueryInformationFile(SafeFileHandle h, out IoStatus io, out uint info, uint len, int cls);
    const int FileCaseSensitiveInformation = 71;
    static SafeFileHandle Open(string dir, uint access) {
        // share read|write|delete, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS
        var h = CreateFileW(dir, access, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
        if (h.IsInvalid) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        return h;
    }
    public static void Set(string dir, bool on) {
        using (var h = Open(dir, 0x180)) { // FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES
            IoStatus io; uint flags = on ? 1u : 0u;
            int st = NtSetInformationFile(h, out io, ref flags, 4, FileCaseSensitiveInformation);
            if (st != 0) throw new Exception("NtSetInformationFile failed: 0x" + st.ToString("X8"));
        }
    }
    public static bool Get(string dir) {
        using (var h = Open(dir, 0x80)) { // FILE_READ_ATTRIBUTES
            IoStatus io; uint flags;
            int st = NtQueryInformationFile(h, out io, out flags, 4, FileCaseSensitiveInformation);
            if (st != 0) throw new Exception("NtQueryInformationFile failed: 0x" + st.ToString("X8"));
            return (flags & 1u) != 0;
        }
    }
}
"@

function Write-Step([string]$Message) { Write-Host "    $Message" }

function Get-FreePort {
    for ($port = $PortFrom; $port -le $PortTo; $port++) {
        $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Any, $port)
        try { $listener.Start(); $listener.Stop(); return $port } catch { }
    }
    throw "no free TCP port in $PortFrom-$PortTo"
}

function Get-Archive($Spec) {
    New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null
    $path = Join-Path $CacheDir $Spec.File
    if (-not (Test-Path $path)) {
        Write-Step "downloading $($Spec.Url)"
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $Spec.Url -OutFile "$path.part" -UseBasicParsing
        Move-Item "$path.part" $path
    }
    $hash = (Get-FileHash -Algorithm SHA256 $path).Hash.ToLowerInvariant()
    if ($hash -ne $Spec.Sha256) { throw "sha256 mismatch for $($Spec.File): $hash" }
    Write-Step "sha256 ok ($hash)"
    return $path
}

function Expand-Server($Archive, [string]$Dest) {
    New-Item -ItemType Directory -Force -Path $Dest | Out-Null
    if (Get-Command 7z -ErrorAction SilentlyContinue) {
        & 7z x -y "-o$Dest" $Archive | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "7z failed ($LASTEXITCODE)" }
    } else {
        Expand-Archive -Path $Archive -DestinationPath $Dest -Force
    }
    $root = Get-ChildItem $Dest -Directory | Select-Object -First 1
    return $root.FullName
}

function Get-MyIni($Spec, [int]$Port, [string]$DataDir, [string]$LogDir, [int]$LowerCase) {
    $dataPosix = $DataDir -replace '\\', '/'
    $logPosix = $LogDir -replace '\\', '/'
    $common = @(
        '[client]', 'host=127.0.0.1', "port=$Port", 'default-character-set=utf8mb4', '',
        '[mysqld]', 'bind-address=127.0.0.1', "port=$Port",
        "datadir=$dataPosix", "log-error=$logPosix/error.log", '',
        "lower_case_table_names=$LowerCase", '',
        'character-set-server=utf8mb4', 'max_connections=151', 'max_allowed_packet=64M',
        'innodb_buffer_pool_size=256M', 'innodb_flush_log_at_trx_commit=2'
    )
    if ($Spec.Id -eq 'mysql') {
        $common += @('mysqlx=OFF', 'named-pipe=OFF', 'shared-memory=OFF',
            'collation-server=utf8mb4_0900_ai_ci', 'explicit_defaults_for_timestamp=ON')
    } else {
        $common += @('skip-name-resolve', 'collation-server=utf8mb4_unicode_ci')
    }
    return ($common -join "`n") + "`n"
}

function Wait-Port([int]$Port, [System.Diagnostics.Process]$Proc, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if ($Proc.HasExited) { return $false }
        try {
            $client = [System.Net.Sockets.TcpClient]::new()
            $client.Connect('127.0.0.1', $Port)
            $client.Close()
            return $true
        } catch { Start-Sleep -Milliseconds 500 }
    }
    return $false
}

function Invoke-Sql([string]$Client, [int]$Port, [string]$Sql) {
    $out = & $Client --protocol=tcp -h 127.0.0.1 -P $Port -u root --batch --skip-column-names -e $Sql 2>&1
    if ($LASTEXITCODE -ne 0) { throw "client failed ($LASTEXITCODE): $out" }
    $lines = $out | ForEach-Object { "$_" } | Where-Object { $_ -notmatch '^WARNING:' }
    return ($lines -join "`n").Trim()
}

# Runs one server with one lower_case_table_names value. Returns @{Pass; Notes; Lctn}.
function Test-Server($Spec, $Archive, [int]$LowerCase) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $work = Join-Path $TmpDir "validate-$($Spec.Id)-lctn$LowerCase-$stamp"
    $notes = New-Object System.Collections.Generic.List[string]
    $proc = $null
    try {
        Write-Step "extracting to $work"
        $root = Expand-Server $Archive (Join-Path $work 'pkg')
        $bin = Join-Path $root 'bin'
        $data = Join-Path $work 'data'
        $logs = Join-Path $work 'logs'
        New-Item -ItemType Directory -Force -Path $data, $logs | Out-Null

        if ($NoCaseSensitiveFlag) {
            $notes.Add('negative control: data dir left case-insensitive')
            Write-Step 'negative control: data dir left case-insensitive'
        } else {
            # Measured (M1-E1-T2): the flag needs FILE_DELETE_CHILD on the directory. Under a root
            # where the user only has Modify (inherited ACL) the call fails with ACCESS_DENIED.
            & icacls $data /grant "$($env:USERNAME):(OI)(CI)(DC)" | Out-Null
            [ForgeCs]::Set($data, $true)
            $state = "data dir case-sensitive flag: $([ForgeCs]::Get($data))"
            if (-not [ForgeCs]::Get($data)) { throw 'could not mark the data dir case-sensitive' }
            $notes.Add($state)
            Write-Step $state
        }

        $port = Get-FreePort
        $ini = Join-Path $work 'my.ini'
        [System.IO.File]::WriteAllText($ini, (Get-MyIni $Spec $port $data $logs $LowerCase),
            [System.Text.UTF8Encoding]::new($false))

        $server = Join-Path $bin $Spec.Server
        Write-Step "initializing data dir (lower_case_table_names=$LowerCase)"
        if ($Spec.Id -eq 'mysql') {
            $init = & $server "--defaults-file=$ini" --initialize-insecure --console 2>&1
        } else {
            # mariadb-install-db writes its own my.ini inside the data dir; the server is
            # started later with our --defaults-file, so only the system tables come from here.
            $init = & (Join-Path $bin 'mariadb-install-db.exe') "--datadir=$data" "--port=$port" --silent 2>&1
        }
        if ($LASTEXITCODE -ne 0) {
            $notes.Add("init failed ($LASTEXITCODE): " + (($init | Select-Object -Last 8) -join ' | '))
            $errLog = Join-Path $logs 'error.log'
            if (Test-Path $errLog) { $notes.Add('error.log tail: ' + ((Get-Content $errLog -Tail 6) -join ' | ')) }
            return @{ Pass = $false; Notes = $notes; Lctn = $LowerCase }
        }

        Write-Step "starting $($Spec.Server) on port $port"
        $stdout = Join-Path $logs 'stdout.log'
        $stderr = Join-Path $logs 'stderr.log'
        $proc = Start-Process -FilePath $server -ArgumentList "--defaults-file=`"$ini`"", '--console' `
            -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        if (-not (Wait-Port $port $proc 90)) {
            $notes.Add('server did not accept connections (exited or timeout)')
            foreach ($f in @((Join-Path $logs 'error.log'), $stderr)) {
                if (Test-Path $f) { $notes.Add("$(Split-Path $f -Leaf) tail: " + ((Get-Content $f -Tail 8) -join ' | ')) }
            }
            return @{ Pass = $false; Notes = $notes; Lctn = $LowerCase }
        }

        $client = Join-Path $bin $Spec.Client
        $effective = Invoke-Sql $client $port 'SELECT @@version, @@lower_case_table_names;'
        $notes.Add("version/effective lower_case_table_names: $effective")
        Write-Step $effective
        $effectiveLctn = [int](($effective -split '\s+')[-1])

        Invoke-Sql $client $port 'CREATE DATABASE cs_probe;' | Out-Null
        Invoke-Sql $client $port 'CREATE TABLE cs_probe.Users (id INT PRIMARY KEY);' | Out-Null
        $second = $null
        try { Invoke-Sql $client $port 'CREATE TABLE cs_probe.users (id INT PRIMARY KEY);' | Out-Null } catch { $second = $_.Exception.Message }
        $tables = (Invoke-Sql $client $port 'SHOW TABLES IN cs_probe;') -split '\s+' | Where-Object { $_ }
        $notes.Add("tables: $($tables -join ', ')")
        Write-Step "tables: $($tables -join ', ')"
        if ($second) { $notes.Add("second CREATE TABLE failed: $second") }
        $bothExist = ($tables -ccontains 'Users') -and ($tables -ccontains 'users')

        if ($NoCaseSensitiveFlag) {
            $pass = $true  # informational: the effective value and tables are in the notes
        } elseif ($LowerCase -eq 0) {
            $pass = ($effectiveLctn -eq 0) -and $bothExist
        } else {
            # With =2 names are stored as given but compared case-insensitively: one table only.
            $pass = ($effectiveLctn -eq 2) -and ($tables -contains 'Users') -and ($tables.Count -eq 1)
        }
        return @{ Pass = $pass; Notes = $notes; Lctn = $LowerCase; Effective = $effectiveLctn }
    } catch {
        $notes.Add("exception: $($_.Exception.Message)")
        return @{ Pass = $false; Notes = $notes; Lctn = $LowerCase }
    } finally {
        if ($proc -and -not $proc.HasExited) {
            try {
                & (Join-Path $bin $Spec.Admin) --protocol=tcp -h 127.0.0.1 -P $port -u root shutdown 2>&1 | Out-Null
                if (-not $proc.WaitForExit(30000)) { $proc.Kill() }
            } catch { try { $proc.Kill() } catch { } }
        }
        if (-not $KeepFiles -and (Test-Path $work)) {
            # Clearing the flag is not needed to delete; recursive delete handles CS directories.
            Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
        }
    }
}

$results = @()
foreach ($spec in $Servers) {
    if ($Only -ne 'all' -and $Only -ne $spec.Id) { continue }
    Write-Host "== $($spec.Name) $($spec.Version)"
    $archive = Get-Archive $spec
    $first = Test-Server $spec $archive $Setting
    $results += [pscustomobject]@{ Server = "$($spec.Name) $($spec.Version)"; Setting = "lower_case_table_names=$Setting"; Result = $(if ($first.Pass) { 'PASS' } else { 'FAIL' }); Notes = ($first.Notes -join ' ; ') }
    if (-not $first.Pass -and $Setting -eq 0) {
        Write-Host "   lower_case_table_names=0 FAILED, retrying with =2"
        $second = Test-Server $spec $archive 2
        $results += [pscustomobject]@{ Server = "$($spec.Name) $($spec.Version)"; Setting = 'lower_case_table_names=2 (fallback)'; Result = $(if ($second.Pass) { 'PASS' } else { 'FAIL' }); Notes = ($second.Notes -join ' ; ') }
    }
}

Write-Host ''
Write-Host '== Summary'
foreach ($r in $results) {
    Write-Host ("{0,-16} {1,-38} {2}" -f $r.Server, $r.Setting, $r.Result)
    Write-Host "    $($r.Notes)"
}
# Exit 1 when some server has no PASS at all (neither =0 nor the =2 fallback).
$noPass = $results | Group-Object Server | Where-Object { -not ($_.Group | Where-Object Result -eq 'PASS') }
if ($noPass) { exit 1 } else { exit 0 }
