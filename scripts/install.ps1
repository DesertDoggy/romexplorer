<#
.SYNOPSIS
  Install the romexplorer server on Windows as a service that comes back after a reboot.

.DESCRIPTION
  The Windows counterpart of scripts/install.sh. It differs from that script in one way that
  is worth understanding rather than copying blindly:

  install.sh cannot run first-run setup itself, because the bundled mariadbd refuses to start
  as root and takes no --user, so setup has to happen as the unprivileged service account --
  which is why Unix gets the romexplorer-server-start launcher that adds
  --init-from-config on a first start. Windows has no such restriction, so this script runs
  setup directly, in the foreground, where a failure is reported to the person who typed the
  command instead of landing in a service log. That is also why package.sh does not ship the
  launcher on Windows.

  The service itself goes through WinSW. Not decoration: src/lifecycle.rs notes that Windows
  allows roughly five seconds for console close/logoff/shutdown events, which is not enough to
  close InnoDB's tablespaces, so the server "wants a real Service Control Manager handler".
  WinSW is that handler -- it owns the SCM conversation and stops the child with Ctrl+C, which
  lifecycle::stop_requested's Windows arm already listens for.

.EXAMPLE
  .\install.ps1
  Installs the package this script sits in.

.EXAMPLE
  .\install.ps1 -From C:\dl\romexplorer-server-0.1.0-windows-x64.zip -Port 7337

.EXAMPLE
  .\install.ps1 -Uninstall -Purge
#>
[CmdletBinding()]
param(
    [string] $From,
    [string] $Prefix,
    [string] $Config,
    [int]    $Port,
    [string] $Bind,
    [string] $WinSwPath,
    [switch] $NoStart,
    [switch] $NoService,
    [switch] $Uninstall,
    [switch] $Purge,
    [switch] $Yes
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ServiceName = 'romexplorer'
# Pinned rather than "latest": an installer that silently changes the thing supervising the
# service between runs is not one you can support.
$WinSwVersion = 'v2.12.0'
$WinSwUrl     = "https://github.com/winsw/winsw/releases/download/$WinSwVersion/WinSW-x64.exe"

function Die([string] $Message) { Write-Error $Message; exit 1 }
function Note([string] $Message) { Write-Host "  $Message" }
function Step([string] $Message) { Write-Host "`n==> $Message" -ForegroundColor Cyan }

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

# `KEY=VALUE`, the same shape src/database/env_file.rs reads: comments, blank lines, optional
# quotes. Deliberately not ConvertFrom-StringData, which mangles backslashes in paths.
function Read-Conf([string] $Path) {
    $map = @{}
    if (-not (Test-Path -LiteralPath $Path)) { return $map }
    foreach ($line in Get-Content -LiteralPath $Path) {
        $t = $line.Trim()
        if (-not $t -or $t.StartsWith('#')) { continue }
        $t = $t -replace '^export\s+', ''
        $i = $t.IndexOf('=')
        if ($i -lt 1) { continue }
        $k = $t.Substring(0, $i).Trim().ToLowerInvariant()
        $v = $t.Substring($i + 1).Trim()
        if ($v.Length -ge 2 -and (($v.StartsWith('"') -and $v.EndsWith('"')) -or ($v.StartsWith("'") -and $v.EndsWith("'")))) {
            $v = $v.Substring(1, $v.Length - 2)
        }
        $map[$k] = $v
    }
    $map
}
function Conf([hashtable] $M, [string] $Key, [string] $Default = '') {
    if ($M.ContainsKey($Key) -and $M[$Key]) { $M[$Key] } else { $Default }
}

# 24 alphanumerics from the OS CSPRNG, not Get-Random, which is seeded and not for secrets.
function New-Password {
    $bytes = [byte[]]::new(64)
    [System.Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
    $alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'
    -join ($bytes | ForEach-Object { $alphabet[$_ % $alphabet.Length] } | Select-Object -First 24)
}

function Test-Port([string] $TargetHost, [int] $TargetPort) {
    try {
        $c = [System.Net.Sockets.TcpClient]::new()
        $ok = $c.ConnectAsync($TargetHost, $TargetPort).Wait(1000)
        $c.Close(); return $ok
    } catch { return $false }
}

# Owner-only, and owner only: setup.env and auth.env hold plaintext passwords until the
# server consumes and deletes them. Inheritance is broken first, or the parent's ACL would
# put Users back on the file.
# LocalSystem and BUILTIN\Administrators by SID: the names are localised (Administratoren,
# Administrateurs, ...) and an NTAccount that does not resolve fails the whole install.
$ProtectedPrincipals = @(
    [System.Security.Principal.SecurityIdentifier]::new('S-1-5-18'),
    [System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))

function Protect-File([string] $Path) {
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($r in @($acl.Access)) { [void]$acl.RemoveAccessRule($r) }
    foreach ($who in $ProtectedPrincipals) {
        $acl.AddAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new(
            $who, 'FullControl', 'None', 'None', 'Allow'))
    }
    Set-Acl -LiteralPath $Path -AclObject $acl
}

# Protect-File for a directory: the same two principals, inherited by everything below it.
function Protect-Directory([string] $Path) {
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($r in @($acl.Access)) { [void]$acl.RemoveAccessRule($r) }
    foreach ($who in $ProtectedPrincipals) {
        $acl.AddAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new(
            $who, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    }
    Set-Acl -LiteralPath $Path -AclObject $acl
}

if (-not $Prefix) { $Prefix = Join-Path $env:ProgramFiles 'romexplorer' }
# The install is the app root, exactly as the package is: executables beside lib\, assets\
# and data\.
$App = $Prefix

if (-not (Test-Admin)) {
    Die "This needs an elevated PowerShell: it writes under $env:ProgramFiles and registers a service. Re-run as Administrator."
}

# ---------------------------------------------------------------------------- uninstall ---
if ($Uninstall) {
    Step "removing the $ServiceName service"
    $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($svc) {
        if ($svc.Status -ne 'Stopped') { Stop-Service -Name $ServiceName -Force; Note 'stopped' }
        $winsw = Join-Path $Prefix 'winsw.exe'
        if (Test-Path -LiteralPath $winsw) {
            & $winsw uninstall (Join-Path $Prefix 'romexplorer-service.xml') | Out-Null
        } else {
            & sc.exe delete $ServiceName | Out-Null
        }
        Note "removed the service"
    } else { Note 'no such service' }

    if ($Purge) {
        if (Test-Path -LiteralPath $Prefix) {
            if (-not $Yes) {
                $a = Read-Host "delete $Prefix and everything in it, database included? [y/N]"
                if ($a -notmatch '^[Yy]') { Die "left $Prefix alone" }
            }
            Remove-Item -LiteralPath $Prefix -Recurse -Force
            Note "removed $Prefix"
        }
    } else {
        Note "kept $Prefix (pass -Purge to delete it, database included)"
    }
    Write-Host "`nuninstalled."
    exit 0
}

# ------------------------------------------------------------------------ find the package ---
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("romexplorer-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null
try {

Step 'locating the package'
$SelfDir = Split-Path -Parent $PSCommandPath
$Src = $null

function Test-Package([string] $Dir) {
    $Dir -and (Test-Path -LiteralPath (Join-Path $Dir 'romexplorer_server.exe')) -and
        (Test-Path -LiteralPath (Join-Path $Dir 'lib') -PathType Container)
}

if ($From) {
    if ($From -match '^https?://') {
        $name = Split-Path -Leaf ($From -split '\?')[0]
        Note "downloading $From"
        Invoke-WebRequest -Uri $From -OutFile (Join-Path $Tmp $name) -UseBasicParsing
        # Verify if the publisher put a .sha256 beside it; a mismatch is fatal, absence is not.
        try {
            Invoke-WebRequest -Uri "$From.sha256" -OutFile (Join-Path $Tmp "$name.sha256") -UseBasicParsing
            $want = ((Get-Content -Raw (Join-Path $Tmp "$name.sha256")).Trim() -split '\s+')[0]
            $got  = (Get-FileHash -LiteralPath (Join-Path $Tmp $name) -Algorithm SHA256).Hash
            if ($want -and $want.ToLower() -ne $got.ToLower()) { Die "sha256 mismatch on $name -- refusing to install it" }
            Note 'sha256 verified'
        } catch { Note 'no .sha256 published next to the archive; skipping the checksum' }
        Expand-Archive -LiteralPath (Join-Path $Tmp $name) -DestinationPath (Join-Path $Tmp 'unpacked') -Force
    } elseif (Test-Path -LiteralPath $From -PathType Container) {
        $Src = (Resolve-Path -LiteralPath $From).Path
    } elseif (Test-Path -LiteralPath $From) {
        Expand-Archive -LiteralPath $From -DestinationPath (Join-Path $Tmp 'unpacked') -Force
    } else { Die "no such file or directory: $From" }
} elseif (Test-Package $SelfDir) {
    $Src = $SelfDir
    Note 'installing the package this script sits in'
} else {
    Die 'no package found. Put this script inside an unpacked package, or pass -From <zip|dir|url>.'
}

if (-not $Src) {
    $unpacked = Join-Path $Tmp 'unpacked'
    if (Test-Package $unpacked) { $Src = $unpacked }
    else { $Src = (Get-ChildItem -LiteralPath $unpacked -Directory | Where-Object { Test-Package $_.FullName } | Select-Object -First 1).FullName }
}
if (-not (Test-Package $Src)) { Die 'that package has no romexplorer_server.exe and lib\ at its top level' }
Note "package: $Src"
# An install has the same shape as a package, so the copy of this script inside one would
# otherwise "upgrade" the install from itself -- deleting lib\ and then copying it from nowhere.
if ((Test-Path -LiteralPath $Prefix) -and ((Resolve-Path -LiteralPath $Src).Path.TrimEnd('\') -ieq (Resolve-Path -LiteralPath $Prefix).Path.TrimEnd('\'))) {
    Die "$Prefix is the install itself, not a package to install from; pass -From"
}

# PROCESSOR_ARCHITEW6432 is set when this runs in a 32-bit or emulated PowerShell; it names
# the machine's real architecture, which is what the package has to match.
$HostArch = if (($env:PROCESSOR_ARCHITEW6432, $env:PROCESSOR_ARCHITECTURE | Where-Object { $_ } | Select-Object -First 1) -eq 'ARM64') { 'arm64' } else { 'x64' }
$PkgLib = Join-Path $Src "lib\windows\$HostArch"
if (-not (Test-Path -LiteralPath $PkgLib)) {
    Die "that package carries no lib\windows\$HostArch -- it was built for another platform or architecture"
}

# A package with its MariaDB removed (to run against a database you already have) cannot do
# first-run setup: that creates the database by starting the bundled mariadbd. The database
# has to exist already and database.env has to say where it is.
$BundledDb = Test-Path -LiteralPath (Join-Path $PkgLib 'mariadb\bin\mariadbd.exe')
if (-not $BundledDb -and -not (Test-Path -LiteralPath (Join-Path $App 'data\database.env'))) {
    Die ("this package has no bundled MariaDB, so it cannot run first-run setup.`n" +
         "  Point it at a database you already have: create $App\data\database.env with`n" +
         "    db_host=`"...`"  db_port=`"3306`"  db_user=`"...`"  db_password=`"...`"  db_database=`"romexplorer`"`n" +
         "  one per line, then re-run this script. Or install the server package, which bundles one.")
}

# ------------------------------------------------------------------------------- settings ---
if (-not $Config) {
    foreach ($c in @((Join-Path $Src 'romexplorer.conf'), (Join-Path $SelfDir 'romexplorer.conf'), (Join-Path $Src 'romexplorer.conf.example'))) {
        if ($c -and (Test-Path -LiteralPath $c)) { $Config = $c; break }
    }
}
if ($Config) { Note "settings from $Config" } else { Note 'no settings file; using defaults for everything' }
$C = Read-Conf $Config

$AdminUser   = Conf $C 'admin_username' 'admin'
$AdminPass   = Conf $C 'admin_password'
$DbName      = Conf $C 'database' 'romexplorer'
$DbRootPass  = Conf $C 'db_root_password'
$DbAdminUser = Conf $C 'db_admin_user' 'rx_admin'
$DbAdminPass = Conf $C 'db_admin_password'
$DbAppUser   = Conf $C 'db_app_user'
$DbAppPass   = Conf $C 'db_app_password'
$DbPort      = [int](Conf $C 'db_port' '3307')
$DbDataDir   = Conf $C 'db_datadir'
if (-not $Port) { $Port = [int](Conf $C 'server_port' '7337') }
if (-not $Bind) { $Bind = Conf $C 'server_bind' '127.0.0.1' }

$GeneratedAdminPass = $false
if (-not $AdminPass)   { $AdminPass = New-Password; $GeneratedAdminPass = $true }
if (-not $DbRootPass)  { $DbRootPass = New-Password }
if (-not $DbAdminPass) { $DbAdminPass = New-Password }
if ($DbAppUser -and -not $DbAppPass) { $DbAppPass = New-Password }
elseif (-not $DbAppUser) { $DbAppPass = '' }

# ---------------------------------------------------------------------------- lay it down ---
$Upgrade = Test-Path -LiteralPath (Join-Path $App 'data')
if ($Upgrade) {
    Step "upgrading the install at $Prefix"
    $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($svc -and $svc.Status -ne 'Stopped') { Stop-Service -Name $ServiceName -Force; Note 'stopped the running service' }
    # Only what a release replaces. data\ and anything the user put in assets\ stay put.
    $target = Join-Path $App 'lib'
    if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }
    Copy-Item -LiteralPath (Join-Path $Src 'lib') -Destination $target -Recurse -Force
    Get-ChildItem -LiteralPath $Src -File | Where-Object { $_.Name -like 'romexplorer_*.exe' -or $_.Name -eq 'LICENSE' } |
        ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $Prefix $_.Name) -Force }
    foreach ($shared in @('icons', 'gui_web')) {
        $s = Join-Path $Src "assets\$shared"
        if (Test-Path -LiteralPath $s) {
            $t = Join-Path $App "assets\$shared"
            if (Test-Path -LiteralPath $t) { Remove-Item -LiteralPath $t -Recurse -Force }
            New-Item -ItemType Directory -Path (Join-Path $App 'assets') -Force | Out-Null
            Copy-Item -LiteralPath $s -Destination $t -Recurse -Force
        }
    }
    Note 'replaced the executables, lib\ and the shipped assets; kept data\ and everything else in assets\'
} else {
    Step "installing into $Prefix"
    if ((Test-Path -LiteralPath $Prefix) -and (Get-ChildItem -LiteralPath $Prefix -Force | Select-Object -First 1)) {
        if (-not $Yes) {
            $a = Read-Host "$Prefix is not empty and has no database in it; replace its contents? [y/N]"
            if ($a -notmatch '^[Yy]') { Die "left $Prefix alone" }
        }
        Remove-Item -LiteralPath $Prefix -Recurse -Force
    }
    New-Item -ItemType Directory -Path $Prefix -Force | Out-Null
    Copy-Item -Path (Join-Path $Src '*') -Destination $Prefix -Recurse -Force
    Note 'copied the package'
}

foreach ($d in @('data', 'assets\media', 'assets\cache', 'assets\download\dats')) {
    New-Item -ItemType Directory -Path (Join-Path $App $d) -Force | Out-Null
}
# data\ holds the MariaDB datadir (account hashes, session tokens) and the server's keys.
# Under Program Files it would otherwise inherit BUILTIN\Users read access; the service runs
# as LocalSystem and setup runs elevated, so SYSTEM and Administrators are all it needs.
# Inheritable, so everything mariadbd and the server create in it later gets the same.
Protect-Directory (Join-Path $App 'data')
Note 'data\ restricted to SYSTEM and Administrators'

# ------------------------------------------------------------ the files first start consumes ---
$SetupFile = Join-Path $App 'data\setup.env'
$AuthFile  = Join-Path $App 'data\auth.env'
$DbEnv     = Join-Path $App 'data\database.env'

if (($Upgrade -or -not $BundledDb) -and (Test-Path -LiteralPath $DbEnv)) {
    Note 'setup already ran here, or the database is external; leaving the existing accounts and database alone'
    $NeedSetup = $false
} else {
    Step 'writing the first-start setup files'
    $setup = @("# Written by install.ps1. The server applies this on its first start and then deletes it.",
               "database=$DbName", "root_password=$DbRootPass",
               "admin_user=$DbAdminUser", "admin_password=$DbAdminPass")
    if ($DbAppUser) { $setup += @("app_user=$DbAppUser", "app_password=$DbAppPass") }
    $setup += @("db_port=$DbPort", "server_port=$Port")
    Set-Content -LiteralPath $SetupFile -Value $setup -Encoding ASCII
    Protect-File $SetupFile
    Note 'data\setup.env (database accounts)'

    $auth = @("# Written by install.ps1. Applied once, while no admin can sign in yet, then deleted.",
              "admin_username=$AdminUser", "admin_password=$AdminPass")
    if ($Config) {
        $auth += (Get-Content -LiteralPath $Config | Where-Object { $_ -match '^\s*(user|upload)\.[A-Za-z0-9._@-]+\s*=' } | ForEach-Object { $_.Trim() })
    }
    Set-Content -LiteralPath $AuthFile -Value $auth -Encoding ASCII
    Protect-File $AuthFile
    Note 'data\auth.env (the console login)'

    # StoredConfig reads these and setup preserves whatever is already in database.env, so a
    # pre-seeded file is how a datadir override reaches the first start.
    if ($DbDataDir) {
        Set-Content -LiteralPath $DbEnv -Value @("db_datadir=`"$DbDataDir`"") -Encoding ASCII
        Protect-File $DbEnv
        New-Item -ItemType Directory -Path $DbDataDir -Force | Out-Null
        Note "data\database.env pre-seeded with db_datadir=$DbDataDir"
    }
    $NeedSetup = $true
}

# ---------------------------------------------------------------------------- first-run setup ---
# In the foreground, unlike Unix: there is no mariadbd-as-root restriction here, so setup can
# run under the same account that installs, and a failure lands in this window rather than in
# a service log nobody thinks to read.
$ServerExe = Join-Path $Prefix 'romexplorer_server.exe'
if ($NeedSetup) {
    Step 'running first-run setup'
    $args = @('--init-from-config', $SetupFile, '--bind', '127.0.0.1', '--port', "$Port", '--no-worker')
    $proc = Start-Process -FilePath $ServerExe -ArgumentList $args -PassThru -NoNewWindow
    $waited = 0
    while ($waited -lt 300) {
        if (Test-Port '127.0.0.1' $Port) { break }
        if ($proc.HasExited) { Die "setup exited with code $($proc.ExitCode) before it finished -- see the output above" }
        Start-Sleep -Seconds 2; $waited += 2
        if ($waited % 30 -eq 0) { Note "still setting up (${waited}s)..." }
    }
    if (-not (Test-Port '127.0.0.1' $Port)) {
        try { $proc.Kill() } catch {}
        Die "setup did not finish within ${waited}s"
    }
    Note "setup finished after ${waited}s; stopping the temporary instance"
    # CloseMainWindow is the console-event path lifecycle::stop_requested listens on. Give it
    # real time: this is the shutdown that closes InnoDB's tablespaces, and cutting it short is
    # what makes the next start crash-recover.
    [void]$proc.CloseMainWindow()
    if (-not $proc.WaitForExit(120000)) { try { $proc.Kill() } catch {}; Die 'the temporary instance would not stop; not installing the service over a running database' }
    Note 'stopped cleanly'
}

# ---------------------------------------------------------------------------------- service ---
if (-not $NoService) {
    Step 'installing the service'
    $winsw = if ($WinSwPath) { $WinSwPath } else { Join-Path $Prefix 'winsw.exe' }
    if (-not (Test-Path -LiteralPath $winsw)) {
        Note "fetching WinSW $WinSwVersion"
        try { Invoke-WebRequest -Uri $WinSwUrl -OutFile $winsw -UseBasicParsing }
        catch { Die "could not download WinSW ($WinSwUrl). Pass -WinSwPath with a local copy, or -NoService to skip the service." }
    }

    # onfailure restart mirrors the systemd unit's Restart=on-failure. The console's own restart
    # exits 75 (lifecycle::RESTART_EXIT_CODE), which is non-zero, so WinSW restarts on it too.
    $xml = @"
<service>
  <id>$ServiceName</id>
  <name>romexplorer server</name>
  <description>romexplorer REST server and background worker.</description>
  <executable>$ServerExe</executable>
  <arguments>--bind $Bind --port $Port</arguments>
  <workingdirectory>$Prefix</workingdirectory>
  <env name="ROMEXPLORER_SERVICE_MANAGED" value="1"/>
  <env name="ROMEXPLORER_APP_ROOT" value="$App"/>
  <onfailure action="restart" delay="5 sec"/>
  <resetfailure>1 hour</resetfailure>
  <startmode>Automatic</startmode>
  <!-- Ctrl+C rather than a kill, and long enough for InnoDB to close its tablespaces.
       src/lifecycle.rs: Windows gives console events about five seconds, which is why this
       goes through WinSW's own stop handling instead of relying on them. -->
  <stoptimeout>120 sec</stoptimeout>
  <stopparentprocessfirst>false</stopparentprocessfirst>
  <log mode="roll-by-size"><sizeThreshold>10240</sizeThreshold><keepFiles>4</keepFiles></log>
  <logpath>$App\data</logpath>
</service>
"@
    $xmlPath = Join-Path $Prefix 'romexplorer-service.xml'
    Set-Content -LiteralPath $xmlPath -Value $xml -Encoding UTF8
    Note "wrote $xmlPath"

    if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
        & $winsw uninstall $xmlPath | Out-Null
        Start-Sleep -Seconds 2
    }
    & $winsw install $xmlPath
    if ($LASTEXITCODE -ne 0) { Die "WinSW could not install the service (exit $LASTEXITCODE)" }
    Note 'service registered, start mode Automatic (so it comes back after a reboot)'

    if (-not $NoStart) {
        & $winsw start $xmlPath | Out-Null
        $waited = 0
        $probe = if ($Bind -eq '0.0.0.0') { '127.0.0.1' } else { $Bind }
        while ($waited -lt 300) {
            if (Test-Port $probe $Port) { break }
            Start-Sleep -Seconds 2; $waited += 2
        }
        if (Test-Port $probe $Port) {
            Note "listening on ${Bind}:$Port after ${waited}s"
        } else {
            Write-Warning "the service did not answer on ${Bind}:$Port within ${waited}s. Its log:"
            Get-ChildItem -LiteralPath (Join-Path $App 'data') -Filter "$ServiceName.*.log" -ErrorAction SilentlyContinue |
                Sort-Object LastWriteTime -Descending | Select-Object -First 1 |
                ForEach-Object { Get-Content -LiteralPath $_.FullName -Tail 40 }
            Die "install finished but the server is not up; fix what the log says and run: $winsw restart $xmlPath"
        }
    }
} else {
    Note "no service installed (-NoService); start it with: `"$ServerExe`" --bind $Bind --port $Port"
}

# ------------------------------------------------------------------------------- what to know ---
$scheme = if ($Bind -eq '127.0.0.1' -or $Bind -eq 'localhost') { 'http' } else { 'https' }
$shown  = if ($Bind -eq '0.0.0.0') { $env:COMPUTERNAME } else { $Bind }
Write-Host ''
Write-Host 'romexplorer is installed.' -ForegroundColor Green
Write-Host ''
Write-Host ("  console    {0}://{1}:{2}/" -f $scheme, $shown, $Port)
Write-Host ("  install    {0}" -f $Prefix)
Write-Host ("  service    Get-Service {0}  /  Restart-Service {0}" -f $ServiceName)
Write-Host ("  check      `"{0}\romexplorer_dbtool.exe`" check" -f $Prefix)

if (-not $Upgrade) {
    Write-Host ''
    if ($GeneratedAdminPass) {
        Write-Host 'The console login, generated just now and shown only here:' -ForegroundColor Yellow
        Write-Host ''
        Write-Host ("    username  {0}" -f $AdminUser)
        Write-Host ("    password  {0}" -f $AdminPass)
        Write-Host ''
        Write-Host 'Write it down before closing this window. To change it later:'
        Write-Host ("    `"{0}\romexplorer_server.exe`" reset-admin --username {1}" -f $Prefix, $AdminUser)
    } else {
        Write-Host ("Sign in as `"{0}`" with the password from {1}." -f $AdminUser, (Split-Path -Leaf $Config))
    }
    if ($Bind -eq '127.0.0.1') {
        Write-Host ''
        Write-Host 'This server answers only on this machine. To serve the network, re-run with'
        Write-Host '-Bind 0.0.0.0 (it serves TLS with a certificate made on first start).'
    }
}
Write-Host ''

} finally {
    Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
