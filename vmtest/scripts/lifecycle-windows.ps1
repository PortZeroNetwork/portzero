#Requires -Version 5
# LIFECYCLE E2E (Windows): real signed MSI install -> verify artifacts landed ->
# real uninstall -> ASSERT every artifact is GONE. Closes the audit gap where
# uninstall was completely untested on Windows: nothing proved that removing
# portzero pulls its trusted CA out of LocalMachine\Root, drops the .portzero.local
# NRPT resolver rule, deletes the autostart scheduled task, tears down the Wintun
# adapter, and removes the binary. Leftover trusted-CA / NRPT residue after
# uninstall is the loud-complaint bug class, so every removal is asserted.
#
# This installs the REAL MSI (the same artifact release.yml signs and ships),
# driven exactly as a user would (`msiexec /i`). The MSI's deferred custom
# actions run `autostart enable` + `start --no-browser` on install (as SYSTEM,
# which is how prlctl exec runs it), so the daemon brings up the overlay: CA into
# the trust store, NRPT rule, Wintun adapter. On uninstall the MSI runs
# `autostart disable` + `stop`; we additionally run `trust uninstall` while the
# binary still exists so the CA is removed too (a thorough uninstall).
#
# Prints greppable `PHASE=<name> ... ok=<true|false>` lines, then RESULT=PASS|FAIL.
# Run as admin / SYSTEM. Every step that can wedge is time-boxed with a child job.
#
# Env:
#   PORTZERO_MSI  path to the .msi. If unset, searches
#                 vmtest\.downloaded-artifacts\windows\*.msi.
$ErrorActionPreference = 'Continue'

$script:fails = 0
$CertSubjectMatch = 'PortZero Local CA'
$TaskName = 'cloud.portzero.daemon'
$NrptMatch = 'portzero.local'
$AdapterName = 'deven0'
$InstalledExe = Join-Path ${env:ProgramFiles} 'Port Zero\portzero.exe'

function Phase {
    param([string] $Name, [bool] $Ok)
    if ($Ok) {
        "PHASE=$Name ok=true"
    } else {
        "PHASE=$Name ok=false"
        $script:fails++
    }
}

# Run a scriptblock under a hard timeout so a wedged msiexec / stop never hangs
# the whole run (same discipline as repro-trust-install.ps1).
function Invoke-Guarded {
    param([scriptblock] $Script, [object[]] $ArgList = @(), [int] $TimeoutSec = 120, [string] $Label = 'step')
    $job = Start-Job -ScriptBlock $Script -ArgumentList $ArgList
    if (Wait-Job $job -Timeout $TimeoutSec) {
        Receive-Job $job 2>&1 | Out-Null
        Remove-Job $job -Force -ErrorAction SilentlyContinue
        return $true
    }
    Stop-Job $job -ErrorAction SilentlyContinue
    Remove-Job $job -Force -ErrorAction SilentlyContinue
    "WARN=$Label-timed-out-after-${TimeoutSec}s"
    return $false
}

# --- predicates (each returns [bool]) --------------------------------------
function Test-CertPresent {
    $c = Get-ChildItem Cert:\LocalMachine\Root -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -match $CertSubjectMatch }
    return [bool] $c
}
function Test-TaskPresent {
    $t = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    return [bool] $t
}
function Test-NrptPresent {
    $r = Get-DnsClientNrptRule -ErrorAction SilentlyContinue |
        Where-Object { $_.Namespace -match $NrptMatch }
    return [bool] $r
}
function Test-AdapterPresent {
    $a = Get-NetAdapter -Name $AdapterName -ErrorAction SilentlyContinue
    return [bool] $a
}

function Find-Msi {
    if ($env:PORTZERO_MSI -and (Test-Path $env:PORTZERO_MSI)) { return $env:PORTZERO_MSI }
    $repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $candidate = Get-ChildItem -Path (Join-Path $repo 'vmtest\.downloaded-artifacts\windows') `
        -Filter '*.msi' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($candidate) { return $candidate.FullName }
    return $null
}

$msi = Find-Msi
"PHASE=preflight msi=$(if ($msi) { $msi } else { 'none' })"
if (-not $msi) {
    "RESULT=FAIL no .msi found (set PORTZERO_MSI or drop one under vmtest\.downloaded-artifacts\windows\)"
    exit 1
}

# --- INSTALL (real signed MSI) ---------------------------------------------
">> installing $msi"
$log = Join-Path $env:TEMP 'pz-msi-install.log'
$p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$msi`"", '/qn', '/norestart', '/l*v', "`"$log`"") -Wait -PassThru
Phase 'install-exit' ($p.ExitCode -eq 0)
Phase 'install-binary' (Test-Path $InstalledExe)
Phase 'install-task' (Test-TaskPresent)

# The daemon (started by the MSI custom action) brings up the overlay
# asynchronously; poll for the trust store + NRPT + adapter to appear.
$deadline = (Get-Date).AddSeconds(120)
while ((Get-Date) -lt $deadline) {
    if ((Test-CertPresent) -and (Test-NrptPresent) -and (Test-AdapterPresent)) { break }
    Start-Sleep -Seconds 3
}
Phase 'install-trust-ca' (Test-CertPresent)
Phase 'install-nrpt-rule' (Test-NrptPresent)
Phase 'install-wintun-adapter' (Test-AdapterPresent)

# --- UNINSTALL (the way a user removes it) ---------------------------------
# Remove the CA while the binary still exists (the MSI's stop/disable custom
# actions don't touch the trust store), then run the MSI uninstaller which stops
# the daemon, disables autostart, and removes the files.
if (Test-Path $InstalledExe) {
    ">> trust uninstall"
    $null = Invoke-Guarded -Label 'trust-uninstall' -TimeoutSec 60 -ArgList @($InstalledExe) -Script {
        param($exe)
        & $exe trust uninstall 2>&1 | Out-Null
    }
}
">> msiexec /x"
$logx = Join-Path $env:TEMP 'pz-msi-uninstall.log'
$px = Start-Process msiexec.exe -ArgumentList @('/x', "`"$msi`"", '/qn', '/norestart', '/l*v', "`"$logx`"") -Wait -PassThru
Phase 'uninstall-exit' ($px.ExitCode -eq 0)

# --- ASSERT CLEAN: every install artifact must be GONE ---------------------
Start-Sleep -Seconds 3
Phase 'clean-binary' (-not (Test-Path $InstalledExe))
Phase 'clean-task' (-not (Test-TaskPresent))
Phase 'clean-trust-ca' (-not (Test-CertPresent))
Phase 'clean-nrpt-rule' (-not (Test-NrptPresent))
Phase 'clean-wintun-adapter' (-not (Test-AdapterPresent))

if ($script:fails -eq 0) {
    "RESULT=PASS lifecycle (install -> verify -> uninstall -> assert-clean)"
    exit 0
}
"RESULT=FAIL lifecycle assertions failed=$($script:fails)"
exit 1
