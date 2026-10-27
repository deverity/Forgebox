#Requires -Version 7.2
<#
.SYNOPSIS
    Forgebox M1 gate smoke test (docs/10 "Gate M1", docs/16-smoke-m1.md).

.DESCRIPTION
    From an empty, temporary installation root and using only the workspace binaries
    (forgeboxd + forge CLI):
      1. forge tool install php@8.4 nginx mysql@8.4 mailpit composer (sha256-verified)
      2. forge start all
      3. Laravel project "loja" (composer through `forge exec`), site loja.test added with
         `forge site add --driver laravel --secure`, database created with the Forgebox
         mysql client, .env pointed at the Forgebox MySQL and Mailpit
      4. php artisan migrate (Forgebox PHP, through `forge exec`)
      5. HTTPS: GET https://loja.test[:port] answers 200 with the Laravel page, with a
         certificate issued by the Forgebox CA
      6. an e-mail sent by Laravel is captured by Mailpit (checked through its HTTP API,
         unique subject per run)
      7. teardown: services and daemon stopped, leftovers of this root killed, root removed

    The script never writes Forgebox configuration files and never talks to the daemon
    other than through `forge`. curl.exe and .NET are used only to VERIFY (HTTPS, Mailpit
    API, the DNS server). Ports are chosen through `forge config set ports.ranges.*`.

    Interactive mode (default): `forge dns enable` (UAC for the NRPT rule) and
    `forge tls ca install` (Windows confirmation dialog), then https://loja.test is fetched
    through the OS resolver and trust store. The root is a regular (non-portable) root,
    because portable mode never touches the OS trust store (docs/08 section 2.2); the
    script never runs `forge path add`, so the user PATH is untouched.

    -NonInteractive: nothing elevates or opens a dialog. The root is portable
    (portable.flag). loja.test is resolved with curl --resolve and the chain is checked
    with --cacert <root>\certs\ca\forgebox-ca.crt (never -k); the Forgebox DNS server is
    queried directly (Resolve-DnsName -Server 127.0.0.1).

.EXAMPLE
    pwsh -NoProfile -File scripts/smoke.ps1 -NonInteractive
#>
[CmdletBinding()]
param(
    # Installation root (must not exist or be empty). Default: %TEMP%\fb-smoke-<timestamp>.
    [string]$Root,
    # Keep the root (and its logs) at the end.
    [switch]$Keep,
    # No UAC, no dialogs (see the description).
    [switch]$NonInteractive,
    [int]$HttpPort = 80,
    [int]$HttpsPort = 443,
    [int]$MysqlPort = 3306,
    [int]$MailpitSmtpPort = 1025,
    [int]$MailpitHttpPort = 8025,
    # Full log. Default: <root>\logs\smoke.log (copied next to the root when it is removed).
    [string]$LogPath,
    # Use the binaries already built in target\debug (no cargo build).
    [switch]$NoBuild,
    # Laravel project skeleton for composer create-project.
    [string]$LaravelPackage = 'laravel/laravel',
    # Self-test of the teardown: break forge.exe right before it (step 7 must FAIL but
    # still kill every process of the root).
    [switch]$BreakForgeBeforeTeardown
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
if (-not $Root) { $Root = Join-Path ([System.IO.Path]::GetTempPath()) "fb-smoke-$Stamp" }
$Root = [System.IO.Path]::GetFullPath($Root)
$Site = 'loja.test'
$Project = Join-Path $Root 'projects\loja'
$Database = 'loja'
$Subject = "forgebox-smoke-$Stamp-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
$Results = [System.Collections.Generic.List[object]]::new()
$Failed = $false
$Daemon = $false
$Forge = Join-Path $Root 'forge.exe'
$script:LogFile = $null
$EarlyLog = [System.Collections.Generic.List[string]]::new()

# ---------------------------------------------------------------- output and logging

function Write-Log([string]$Text, [string]$Color = '') {
    $line = "[{0}] {1}" -f (Get-Date -Format 'HH:mm:ss.fff'), $Text
    if ($Color) { Write-Host $line -ForegroundColor $Color } else { Write-Host $line }
    if ($script:LogFile) { Add-Content -LiteralPath $script:LogFile -Value $line -Encoding utf8 }
    else { $EarlyLog.Add($line) }
}

function Open-Log([string]$Path) {
    $dir = Split-Path -Parent $Path
    if ($dir) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $script:LogFile = $Path
    if ($EarlyLog.Count) { Add-Content -LiteralPath $Path -Value $EarlyLog -Encoding utf8 }
}

# Runs a native command, logs its full output, returns it; throws on a non-zero exit code
# unless -AllowFail.
function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments, [switch]$AllowFail, [string]$WorkDir)
    Write-Log ("> {0} {1}" -f (Split-Path -Leaf $Exe), ($Arguments -join ' '))
    if ($WorkDir) { Push-Location -LiteralPath $WorkDir }
    try {
        $out = & $Exe @Arguments 2>&1 | ForEach-Object { "$_" }
        $code = $LASTEXITCODE
    } finally {
        if ($WorkDir) { Pop-Location }
    }
    foreach ($l in @($out)) { if ($null -ne $l) { Write-Log "  | $l" } }
    if ($code -ne 0 -and -not $AllowFail) {
        throw ("{0} exited with {1}" -f (Split-Path -Leaf $Exe), $code)
    }
    [pscustomobject]@{ Code = $code; Output = (@($out) -join "`n") }
}

function Invoke-Forge {
    param([string[]]$Arguments, [switch]$AllowFail, [string]$WorkDir)
    Invoke-Native -Exe $Forge -Arguments (@('--root', $Root, '--yes') + $Arguments) -AllowFail:$AllowFail -WorkDir $WorkDir
}

function Invoke-ForgeJson([string[]]$Arguments) {
    $r = Invoke-Forge -Arguments (@('--json') + $Arguments)
    $r.Output | ConvertFrom-Json -Depth 64
}

# One gate step: timed, PASS/FAIL/SKIP recorded. A failed step makes the following
# dependent steps SKIP (teardown always runs).
function Invoke-Step([string]$Name, [scriptblock]$Body, [switch]$Always) {
    if ($script:Failed -and -not $Always) {
        $Results.Add([pscustomobject]@{ Step = $Name; Result = 'SKIP'; Seconds = 0; Detail = 'a previous step failed' })
        Write-Log "---- SKIP  $Name (a previous step failed)" 'DarkYellow'
        return
    }
    Write-Log "==== $Name" 'Cyan'
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $detail = & $Body
        $sw.Stop()
        $Results.Add([pscustomobject]@{ Step = $Name; Result = 'PASS'; Seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1); Detail = "$detail" })
        Write-Log ("---- PASS  {0} ({1:N1} s) {2}" -f $Name, $sw.Elapsed.TotalSeconds, $detail) 'Green'
    } catch {
        $sw.Stop()
        $script:Failed = $true
        $msg = $_.Exception.Message
        $Results.Add([pscustomobject]@{ Step = $Name; Result = 'FAIL'; Seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1); Detail = $msg })
        Write-Log ("---- FAIL  {0} ({1:N1} s): {2}" -f $Name, $sw.Elapsed.TotalSeconds, $msg) 'Red'
        Write-Log ($_.ScriptStackTrace -replace "`r?`n", ' <- ')
    }
}

# A step that reports but does not fail the gate (diagnostics).
function Add-Note([string]$Name, [string]$Result, [string]$Detail) {
    $Results.Add([pscustomobject]@{ Step = $Name; Result = $Result; Seconds = 0; Detail = $Detail })
    Write-Log "---- $Result  $Name $Detail" 'DarkCyan'
}

# ---------------------------------------------------------------- ports

function Test-PortListening([int]$Port) {
    $listening = @(Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue)
    if ($listening.Count -gt 0) { return $true }
    try {
        $l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
        $l.Start(); $l.Stop()
        return $false
    } catch { return $true }
}

# The requested port, or the first free one of port + 10000*k (warns when it moves).
function Select-Port([string]$Name, [int]$Port, [int[]]$Taken) {
    if (-not (Test-PortListening $Port) -and $Taken -notcontains $Port) { return $Port }
    for ($k = 1; $k -le 6; $k++) {
        $candidate = $Port + 10000 * $k
        if ($candidate -gt 65535) { break }
        if (-not (Test-PortListening $candidate) -and $Taken -notcontains $candidate) {
            Write-Log "WARNING: port $Port ($Name) is in use; using $candidate" 'Yellow'
            return $candidate
        }
    }
    throw "no free port found for $Name (asked for $Port)"
}

# ---------------------------------------------------------------- teardown

# Processes whose executable lives under this root (and only those: Laragon and anything
# else on the machine are never touched).
function Get-RootProcesses {
    $prefix = $Root.TrimEnd('\') + '\'
    @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) })
}

# Kills the processes of this root (a few passes: a dying daemon may still spawn or
# release children) and returns how many were killed.
function Stop-RootProcesses {
    $count = 0
    for ($pass = 0; $pass -lt 3; $pass++) {
        $procs = @(Get-RootProcesses)
        if (-not $procs.Count) { break }
        foreach ($p in $procs) {
            Write-Log "killing leftover $($p.Name) (PID $($p.ProcessId)) of this root"
            try { Stop-Process -Id $p.ProcessId -Force -ErrorAction Stop; $count++ }
            catch {
                # Already gone (the daemon's Job object takes its children with it).
                if (Get-Process -Id $p.ProcessId -ErrorAction SilentlyContinue) {
                    Write-Log "  could not kill PID $($p.ProcessId): $($_.Exception.Message)"
                } else {
                    Write-Log "  PID $($p.ProcessId) already exited"
                }
            }
        }
        Start-Sleep -Milliseconds 500
    }
    $count
}

# ---------------------------------------------------------------- .env

function Set-DotEnv([string]$Path, [hashtable]$Values) {
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.AddRange([string[]](Get-Content -LiteralPath $Path))
    foreach ($key in $Values.Keys) {
        $line = "$key=$($Values[$key])"
        $index = -1
        for ($i = 0; $i -lt $lines.Count; $i++) {
            if ($lines[$i] -match "^\s*#?\s*$([regex]::Escape($key))\s*=") { $index = $i; break }
        }
        if ($index -ge 0) { $lines[$index] = $line } else { $lines.Add($line) }
    }
    [System.IO.File]::WriteAllText($Path, (($lines -join "`n") + "`n"), [System.Text.UTF8Encoding]::new($false))
}

# ================================================================ main

$Total = [System.Diagnostics.Stopwatch]::StartNew()
Write-Log "Forgebox M1 smoke: root=$Root mode=$(if ($NonInteractive) { 'non-interactive' } else { 'interactive' })"

try {
    Invoke-Step 'prepare root and binaries' {
        if ((Test-Path -LiteralPath $Root) -and @(Get-ChildItem -LiteralPath $Root -Force).Count -gt 0) {
            throw "$Root exists and is not empty"
        }
        New-Item -ItemType Directory -Force -Path $Root | Out-Null
        Open-Log $(if ($LogPath) { $LogPath } else { Join-Path $Root 'logs\smoke.log' })
        if (-not $NoBuild) {
            Invoke-Native -Exe 'cargo' -Arguments @('build', '-p', 'forge-cli', '-p', 'forge-daemon', '-p', 'forge-supervisor', '-p', 'forge-helper') -WorkDir $Repo | Out-Null
        }
        # A release-shaped root: the executables next to their data (docs/08 section 1).
        foreach ($exe in 'forge.exe', 'forgeboxd.exe', 'forgebox-ctrl.exe', 'forgebox-helper.exe') {
            $src = Join-Path $Repo "target\debug\$exe"
            if (-not (Test-Path -LiteralPath $src)) { throw "$src is missing (build the workspace first)" }
            Copy-Item -LiteralPath $src -Destination $Root
        }
        if ($NonInteractive) { New-Item -ItemType File -Path (Join-Path $Root 'portable.flag') | Out-Null }
        # The workspace registry (signed with the development key) is the official one.
        $env:FORGEBOX_OFFICIAL_REGISTRY = Join-Path $Repo 'registry'
        $env:FORGE_LANG = 'en'
        $env:NO_COLOR = '1'
        Remove-Item Env:FORGEBOX_ROOT -ErrorAction SilentlyContinue
        "registry=$env:FORGEBOX_OFFICIAL_REGISTRY"
    } -Always

    Invoke-Step 'start the daemon' {
        Invoke-Forge @('daemon', 'start') | Out-Null
        $script:Daemon = $true
        $status = Invoke-ForgeJson @('daemon', 'status')
        "pid=$($status.pid) api=$($status.api_version.major).$($status.api_version.minor) portable=$($status.portable)"
    }

    Invoke-Step 'choose ports (forge config set ports.ranges.*)' {
        $taken = @()
        $script:HttpPort = Select-Port 'http' $HttpPort $taken; $taken += $script:HttpPort
        $script:HttpsPort = Select-Port 'https' $HttpsPort $taken; $taken += $script:HttpsPort
        $script:MysqlPort = Select-Port 'mysql' $MysqlPort $taken; $taken += $script:MysqlPort
        $script:MailpitSmtpPort = Select-Port 'mailpit smtp' $MailpitSmtpPort $taken; $taken += $script:MailpitSmtpPort
        $script:MailpitHttpPort = Select-Port 'mailpit http' $MailpitHttpPort $taken; $taken += $script:MailpitHttpPort
        # Ranges are read in order: nginx takes http then https, mailpit smtp then http.
        Invoke-Forge @('config', 'set', 'ports.ranges.web', "[$($script:HttpPort), $($script:HttpsPort)]") | Out-Null
        Invoke-Forge @('config', 'set', 'ports.ranges.databases', "[$($script:MysqlPort)]") | Out-Null
        Invoke-Forge @('config', 'set', 'ports.ranges.mail', "[$($script:MailpitSmtpPort), $($script:MailpitHttpPort)]") | Out-Null
        # The CRL responder and the internal endpoint are bound at boot (fixed ports of the
        # daemon, docs/03 section 4): only when another process holds them, move them and
        # restart the daemon.
        $daemonPorts = @()
        $status = Invoke-ForgeJson @('daemon', 'status')
        foreach ($range in @(@{ Name = 'tls-crl'; Port = 47480 }, @{ Name = 'internal'; Port = 47481 })) {
            $owners = @(Get-NetTCPConnection -State Listen -LocalPort $range.Port -ErrorAction SilentlyContinue |
                    Where-Object { $_.OwningProcess -ne $status.pid })
            if ($owners.Count) {
                $free = Select-Port $range.Name $range.Port $taken; $taken += $free
                Invoke-Forge @('config', 'set', "ports.ranges.$($range.Name)", "[$free]") | Out-Null
                $daemonPorts += "$($range.Name)=$free"
            }
        }
        if ($daemonPorts.Count) { Invoke-Forge @('daemon', 'restart') | Out-Null }
        "http=$($script:HttpPort) https=$($script:HttpsPort) mysql=$($script:MysqlPort) smtp=$($script:MailpitSmtpPort) mailpit-http=$($script:MailpitHttpPort) $($daemonPorts -join ' ')".Trim()
    }

    Invoke-Step '1. forge tool install php@8.4 nginx mysql@8.4 mailpit composer' {
        Invoke-Forge @('tool', 'install', 'php@8.4', 'nginx', 'mysql@8.4', 'mailpit', 'composer') | Out-Null
        $tools = Invoke-ForgeJson @('tool', 'list', '--installed')
        $names = @($tools.tools | ForEach-Object { "$($_.id)@$($_.current)" })
        foreach ($id in 'php', 'nginx', 'mysql', 'mailpit', 'composer') {
            if (-not ($tools.tools | Where-Object { $_.id -eq $id })) { throw "$id is not installed" }
        }
        $names -join ' '
    }

    Invoke-Step '2. forge start all' {
        Invoke-Forge @('start', 'all') | Out-Null
        $status = Invoke-ForgeJson @('status')
        $bad = @($status.instances | Where-Object { $_.state -ne 'running' })
        if ($bad.Count) { throw "not running: $(($bad | ForEach-Object { "$($_.tool)/$($_.name)=$($_.state)" }) -join ', ')" }
        $ports = @{}
        foreach ($i in $status.instances) { foreach ($p in $i.ports) { $ports["$($i.tool).$($p.name)"] = $p.port } }
        if ($ports['nginx.http'] -ne $script:HttpPort -or $ports['nginx.https'] -ne $script:HttpsPort -or
            $ports['mysql.main'] -ne $script:MysqlPort -or $ports['mailpit.smtp'] -ne $script:MailpitSmtpPort -or
            $ports['mailpit.http'] -ne $script:MailpitHttpPort) {
            throw "ports differ from the configured ranges: $(($ports.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' ')"
        }
        (@($status.instances | ForEach-Object { "$($_.tool)/$($_.name)" }) -join ' ') + ' running'
    }

    Invoke-Step '3a. Laravel project (forge exec -- composer create-project)' {
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Project) | Out-Null
        Invoke-Forge -WorkDir (Split-Path -Parent $Project) -Arguments @('exec', '--', 'composer', 'create-project', $LaravelPackage, $Project, '--no-interaction', '--prefer-dist', '--no-progress') | Out-Null
        if (-not (Test-Path -LiteralPath (Join-Path $Project 'artisan'))) { throw 'artisan not found after create-project' }
        $version = Invoke-Forge -WorkDir $Project -Arguments @('exec', '--', 'php', 'artisan', '--version')
        $version.Output.Trim()
    }

    Invoke-Step '3b. forge site add --driver laravel --secure' {
        Invoke-Forge @('site', 'add', $Project, '--host', $Site, '--php', '8.4', '--server', 'nginx', '--driver', 'laravel', '--secure') | Out-Null
        $sites = Invoke-ForgeJson @('site', 'list')
        $s = @($sites.sites) | Where-Object { $_.host -eq $Site } | Select-Object -First 1
        if (-not $s) { throw "$Site is not listed by forge site list" }
        if ($s.driver -ne 'laravel' -or -not $s.secure) { throw "unexpected site: driver=$($s.driver) secure=$($s.secure)" }
        "driver=$($s.driver) php=$($s.php.resolved) upstream=$($s.php.upstream) secure=$($s.secure)"
    }

    if (-not $NonInteractive) {
        Invoke-Step 'interactive: forge dns enable + forge tls ca install (UAC, CA dialog)' {
            Invoke-Forge @('dns', 'enable') | Out-Null
            Invoke-Forge @('tls', 'ca', 'install', '--scope', 'user') | Out-Null
            $ca = Invoke-ForgeJson @('tls', 'ca', 'status')
            "dns enabled; CA trusted for the user: $($ca | ConvertTo-Json -Compress -Depth 4)"
        }
    }

    Invoke-Step '3c. database and .env (Forgebox mysql client, MySQL + Mailpit)' {
        Invoke-Forge -WorkDir $Project -Arguments @('exec', '--', 'mysql', '--protocol=tcp', '-h', '127.0.0.1', '-P', "$($script:MysqlPort)", '-u', 'root',
            '-e', "CREATE DATABASE IF NOT EXISTS $Database CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci") | Out-Null
        $dbs = Invoke-Forge -WorkDir $Project -Arguments @('exec', '--', 'mysql', '--protocol=tcp', '-h', '127.0.0.1', '-P', "$($script:MysqlPort)", '-u', 'root', '-N', '-e', 'SHOW DATABASES')
        if ($dbs.Output -notmatch "(?m)^$Database\s*$") { throw "database $Database not created" }
        $url = if ($script:HttpsPort -eq 443) { "https://$Site" } else { "https://${Site}:$($script:HttpsPort)" }
        Set-DotEnv (Join-Path $Project '.env') ([ordered]@{
                APP_URL       = $url
                DB_CONNECTION = 'mysql'
                DB_HOST       = '127.0.0.1'
                DB_PORT       = "$($script:MysqlPort)"
                DB_DATABASE   = $Database
                DB_USERNAME   = 'root'
                DB_PASSWORD   = ''
                MAIL_MAILER   = 'smtp'
                MAIL_HOST     = '127.0.0.1'
                MAIL_PORT     = "$($script:MailpitSmtpPort)"
                MAIL_USERNAME = 'null'
                MAIL_PASSWORD = 'null'
            })
        Invoke-Forge -WorkDir $Project -Arguments @('exec', '--', 'php', 'artisan', 'config:clear') | Out-Null
        "database=$Database APP_URL=$url"
    }

    Invoke-Step '4. php artisan migrate (Forgebox PHP)' {
        $r = Invoke-Forge -WorkDir $Project -Arguments @('exec', '--php', '8.4', '--', 'php', 'artisan', 'migrate', '--force', '--no-interaction')
        $tables = Invoke-Forge -WorkDir $Project -Arguments @('exec', '--', 'mysql', '--protocol=tcp', '-h', '127.0.0.1', '-P', "$($script:MysqlPort)", '-u', 'root', '-N', '-e', "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '$Database' AND table_name = 'migrations'")
        if ($tables.Output.Trim() -ne '1') { throw "no migrations table in $Database" }
        $migrated = Invoke-Forge -WorkDir $Project -Arguments @('exec', '--', 'mysql', '--protocol=tcp', '-h', '127.0.0.1', '-P', "$($script:MysqlPort)", '-u', 'root', '-N', '-e', "SELECT COUNT(*) FROM $Database.migrations")
        "migrations applied: $($migrated.Output.Trim())"
    }

    Invoke-Step '5. HTTPS GET loja.test (Forgebox CA)' {
        $ca = Join-Path $Root 'certs\ca\forgebox-ca.crt'
        if (-not (Test-Path -LiteralPath $ca)) { throw "$ca does not exist (no CA was created)" }
        $url = if ($script:HttpsPort -eq 443) { "https://$Site/" } else { "https://${Site}:$($script:HttpsPort)/" }
        $body = Join-Path $Root 'tmp\smoke-body.html'
        if ($NonInteractive) {
            # Resolved without the OS resolver; the chain must validate against the Forgebox
            # CA only (Schannel, revocation checked against the Forgebox CRL). Never -k.
            $curlArgs = @('--silent', '--show-error', '--fail-with-body', '--resolve', "${Site}:$($script:HttpsPort):127.0.0.1", '--cacert', $ca, '--output', $body, '--write-out', '%{http_code}', $url)
        } else {
            $curlArgs = @('--silent', '--show-error', '--fail-with-body', '--output', $body, '--write-out', '%{http_code}', $url)
        }
        $r = Invoke-Native -Exe "$env:SystemRoot\System32\curl.exe" -Arguments $curlArgs -AllowFail
        if ($r.Code -ne 0 -or $r.Output.Trim() -ne '200') { throw "curl exit $($r.Code), HTTP $($r.Output.Trim())" }
        $html = Get-Content -LiteralPath $body -Raw
        if ($html -notmatch 'Laravel') { throw 'the page is not the Laravel welcome page' }

        # Who issued the certificate: the leaf served on the port, checked by .NET against
        # the Forgebox CA as the only trust anchor.
        $caCert = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($ca)
        $client = [System.Net.Sockets.TcpClient]::new('127.0.0.1', $script:HttpsPort)
        try {
            $ssl = [System.Net.Security.SslStream]::new($client.GetStream(), $false, { $true })
            $ssl.AuthenticateAsClient($Site)
            $leaf = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($ssl.RemoteCertificate)
        } finally { $client.Dispose() }
        $chain = [System.Security.Cryptography.X509Certificates.X509Chain]::new()
        $chain.ChainPolicy.TrustMode = [System.Security.Cryptography.X509Certificates.X509ChainTrustMode]::CustomRootTrust
        $chain.ChainPolicy.CustomTrustStore.Add($caCert) | Out-Null
        $chain.ChainPolicy.RevocationMode = [System.Security.Cryptography.X509Certificates.X509RevocationMode]::NoCheck
        if (-not $chain.Build($leaf)) {
            throw "the served certificate does not chain to the Forgebox CA: $(($chain.ChainStatus | ForEach-Object { $_.StatusInformation }) -join '; ')"
        }
        if ($leaf.Issuer -ne $caCert.Subject) { throw "issuer $($leaf.Issuer) is not $($caCert.Subject)" }
        "HTTP 200, Laravel page, issuer '$($leaf.Issuer)'"
    }

    Invoke-Step '6. e-mail sent by Laravel is captured by Mailpit' {
        # An artisan command in the project sends it (the app's own code, not Forgebox config).
        $console = Join-Path $Project 'routes\console.php'
        $command = @'

Illuminate\Support\Facades\Artisan::command('smoke:mail {subject}', function (string $subject) {
    Illuminate\Support\Facades\Mail::raw('Forgebox M1 smoke test.', function ($message) use ($subject) {
        $message->to('smoke@loja.test')->subject($subject);
    });
    $this->info('sent');
});
'@
        Add-Content -LiteralPath $console -Value $command -Encoding utf8NoBOM
        Invoke-Forge -WorkDir $Project -Arguments @('exec', '--', 'php', 'artisan', 'smoke:mail', $Subject) | Out-Null
        $found = $null
        for ($i = 0; $i -lt 20 -and -not $found; $i++) {
            $list = Invoke-RestMethod -Uri "http://127.0.0.1:$($script:MailpitHttpPort)/api/v1/messages?limit=50"
            $found = @($list.messages) | Where-Object { $_.Subject -eq $Subject } | Select-Object -First 1
            if (-not $found) { Start-Sleep -Milliseconds 500 }
        }
        if (-not $found) { throw "no message with subject $Subject in Mailpit" }
        "message $($found.ID) to $(@($found.To)[0].Address)"
    }

    Invoke-Step 'DNS: the Forgebox resolver answers loja.test' {
        $dns = Invoke-ForgeJson @('dns', 'status')
        if (-not $dns.server_running) { throw "the Forgebox DNS server is not running (listen: $($dns.listen))" }
        if ($NonInteractive) {
            if ($dns.listen -notmatch ':53$') { throw "the resolver listens on $($dns.listen), not port 53 (Resolve-DnsName only asks port 53)" }
            $answer = Resolve-DnsName -Name $Site -Server 127.0.0.1 -Type A -DnsOnly -QuickTimeout -ErrorAction Stop
            $ips = @($answer | Where-Object { $_.Type -eq 'A' } | ForEach-Object { $_.IPAddress })
            if ($ips -notcontains '127.0.0.1') { throw "answer: $($ips -join ', ')" }
            "Resolve-DnsName -Server 127.0.0.1 $Site -> $($ips -join ', ') (NRPT not applied: non-interactive)"
        } else {
            $test = Invoke-Forge @('dns', 'test', $Site)
            "forge dns test: $($test.Output.Trim())"
        }
    }
} finally {
    # ------------------------------------------------------------ teardown (always)
    if ($BreakForgeBeforeTeardown -and (Test-Path -LiteralPath $Forge)) {
        # Self-test of the teardown: forge.exe can no longer start (as when an antivirus
        # quarantines it); the teardown must still kill everything of this root.
        Write-Log 'SELF-TEST: replacing forge.exe with a non-executable file before teardown' 'Yellow'
        try { Set-Content -LiteralPath $Forge -Value 'not an executable' -Encoding ascii }
        catch { Write-Log "SELF-TEST: could not replace forge.exe: $($_.Exception.Message)" 'Yellow' }
    }
    Invoke-Step '7. teardown' {
        # Every action is attempted even when a previous one throws (forge.exe blocked,
        # missing, pwsh error): the leftovers of this root are ALWAYS killed before the
        # root is removed. Any failed action still makes the step FAIL.
        $errors = [System.Collections.Generic.List[string]]::new()
        $teardown = [ordered]@{}
        if ($script:Daemon) {
            if (-not $NonInteractive) { $teardown['dns disable'] = { Invoke-Forge @('dns', 'disable') | Out-Null } }
            $teardown['stop all'] = { Invoke-Forge @('stop', 'all') | Out-Null }
            $teardown['daemon stop'] = { Invoke-Forge @('daemon', 'stop') | Out-Null }
        }
        foreach ($action in $teardown.Keys) {
            try { & $teardown[$action] }
            catch {
                $first = ("$($_.Exception.Message)" -split "`r?`n")[0]
                $errors.Add("${action}: $first")
                Write-Log "WARNING: teardown action '$action' failed: $($_.Exception.Message)" 'Yellow'
            }
        }
        $killed = 0
        try { $killed = Stop-RootProcesses }
        catch {
            $errors.Add("kill leftovers: $($_.Exception.Message)")
            Write-Log "WARNING: killing the leftovers failed: $($_.Exception.Message)" 'Yellow'
        }
        $alive = @(try { Get-RootProcesses } catch { $errors.Add("listing processes: $($_.Exception.Message)") })
        if ($alive.Count) { $errors.Add("still alive: $(($alive | ForEach-Object { "$($_.Name)#$($_.ProcessId)" }) -join ', ')") }
        $detail = "leftovers killed: $killed"
        if ($errors.Count) { throw "$detail; $($errors -join '; ')" }
        $detail
    } -Always

    $Total.Stop()
    Write-Log ''
    Write-Log '================ SUMMARY ================'
    foreach ($r in $Results) {
        $color = switch ($r.Result) { 'PASS' { 'Green' } 'FAIL' { 'Red' } default { 'Yellow' } }
        Write-Log ('{0,-5} {1,7:N1} s  {2}  {3}' -f $r.Result, $r.Seconds, $r.Step, $r.Detail) $color
    }
    $failedSteps = @($Results | Where-Object { $_.Result -eq 'FAIL' }).Count
    Write-Log ("TOTAL {0:N1} s, {1} step(s), {2} failed. Root: {3}" -f $Total.Elapsed.TotalSeconds, $Results.Count, $failedSteps, $Root)

    if (-not $Keep -and (Test-Path -LiteralPath $Root)) {
        $saved = Join-Path (Split-Path -Parent $Root) "$(Split-Path -Leaf $Root).smoke.log"
        if ($script:LogFile -and (Test-Path -LiteralPath $script:LogFile) -and -not $LogPath) {
            Copy-Item -LiteralPath $script:LogFile -Destination $saved -Force
            $script:LogFile = $saved
        }
        for ($i = 0; $i -lt 5; $i++) {
            try { Remove-Item -LiteralPath $Root -Recurse -Force -ErrorAction Stop; break }
            catch { Start-Sleep -Seconds 2; if ($i -eq 4) { Write-Log "WARNING: could not remove ${Root}: $($_.Exception.Message)" 'Yellow' } }
        }
        Write-Log "root removed; log: $($script:LogFile)"
    } else {
        Write-Log "root kept; log: $($script:LogFile)"
    }
    $code = if ($failedSteps -gt 0) { 1 } else { 0 }
}
exit $code
