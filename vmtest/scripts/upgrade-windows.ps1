#Requires -Version 5
# UPGRADE E2E (Windows): install a PRIOR released MSI, then install the NEW MSI
# over the top, and assert the WiX <MajorUpgrade> replaced it in place — NO
# side-by-side install. Exactly one installed product carries the UpgradeCode,
# one 'cloud.portzero.daemon' scheduled task, one '.portzero.local' NRPT rule,
# and one Wintun adapter; and the installed binary is the NEW version. A broken
# MajorUpgrade (wrong/duplicated UpgradeCode, or RemoveExistingProducts not
# sequenced) is exactly how you end up with two products, two tasks, or two NRPT
# rules — this proves that does not happen.
#
# Mirrors upgrade-linux.sh: the "prior" MSI is the latest published release
# (fetched via gh) or a pinned path; the "new" MSI is the artifact under test.
# SKIPs cleanly (never a false fail) when no prior MSI is available or when the
# two MSIs carry the same ProductVersion (MajorUpgrade needs a version bump).
#
# Prints greppable `PHASE=<name> ... ok=<true|false>` lines, then RESULT=PASS|FAIL.
# Run as admin / SYSTEM. msiexec steps are time-boxed via a child job.
#
# Env:
#   PORTZERO_MSI      new (under-test) .msi. If unset, searches
#                     vmtest\.downloaded-artifacts\windows\*.msi.
#   PORTZERO_MSI_OLD  prior-version .msi to upgrade FROM. If unset, fetches the
#                     latest published release .msi via gh (needs network + gh).
$ErrorActionPreference = 'Continue'

$script:fails = 0
$UpgradeCode = '{8D64B464-96F7-4CB9-A452-372F0AC067AF}'
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

# Run a scriptblock under a hard timeout so a wedged msiexec never hangs the run
# (same discipline as lifecycle-windows.ps1).
function Invoke-Guarded {
    param([scriptblock] $Script, [object[]] $ArgList = @(), [int] $TimeoutSec = 180, [string] $Label = 'step')
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

# --- counts (each returns [int]) -------------------------------------------
# Number of installed products carrying our UpgradeCode. Exactly one after a
# clean MajorUpgrade; two means the new MSI installed side-by-side.
function Get-RelatedProductCount {
    param([string] $Code)
    $count = 0
    try {
        $installer = New-Object -ComObject WindowsInstaller.Installer
        $related = $installer.RelatedProducts($Code)
        foreach ($p in $related) { $count++ }
    } catch { }
    return $count
}
function Get-TaskCount {
    return @(Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue).Count
}
function Get-NrptCount {
    return @(Get-DnsClientNrptRule -ErrorAction SilentlyContinue |
        Where-Object { $_.Namespace -match $NrptMatch }).Count
}
function Get-AdapterCount {
    return @(Get-NetAdapter -Name $AdapterName -ErrorAction SilentlyContinue).Count
}

# Read ProductVersion straight out of an MSI's Property table via the Windows
# Installer COM automation interface (no install required).
function Get-MsiProductVersion {
    param([string] $Path)
    try {
        $installer = New-Object -ComObject WindowsInstaller.Installer
        $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Path, 0))
        $q = @'
SELECT `Value` FROM `Property` WHERE `Property`='ProductVersion'
'@
        $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($q))
        $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
        $rec = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if ($rec) {
            return $rec.GetType().InvokeMember('StringData', 'GetProperty', $null, $rec, 1)
        }
    } catch { }
    return $null
}

function Install-Msi {
    param([string] $Path, [string] $LogName)
    $log = Join-Path $env:TEMP $LogName
    $ok = Invoke-Guarded -Label "msiexec-$LogName" -TimeoutSec 180 -ArgList @($Path, $log) -Script {
        param($msiPath, $logPath)
        Start-Process msiexec.exe -ArgumentList @('/i', "`"$msiPath`"", '/qn', '/norestart', '/l*v', "`"$logPath`"") -Wait
    }
    return $ok
}

# Poll until the daemon (started by the MSI custom action) brings the overlay up.
function Wait-Overlay {
    $deadline = (Get-Date).AddSeconds(120)
    while ((Get-Date) -lt $deadline) {
        if ((Get-TaskCount) -ge 1 -and (Get-NrptCount) -ge 1 -and (Get-AdapterCount) -ge 1) { break }
        Start-Sleep -Seconds 3
    }
}

function Find-NewMsi {
    if ($env:PORTZERO_MSI -and (Test-Path $env:PORTZERO_MSI)) { return $env:PORTZERO_MSI }
    $repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $candidate = Get-ChildItem -Path (Join-Path $repo 'vmtest\.downloaded-artifacts\windows') `
        -Filter '*.msi' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($candidate) { return $candidate.FullName }
    return $null
}

# Best-effort fetch of the latest published release .msi as the "prior" version.
function Fetch-PriorMsi {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { return $null }
    $out = Join-Path $env:TEMP ("pz-prior-msi-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    & gh release download --repo PortZeroNetwork/portzero-local --pattern '*.msi' --dir $out 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { return $null }
    $msi = Get-ChildItem -Path $out -Filter '*.msi' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($msi) { return $msi.FullName }
    return $null
}

$new = Find-NewMsi
$old = if ($env:PORTZERO_MSI_OLD -and (Test-Path $env:PORTZERO_MSI_OLD)) { $env:PORTZERO_MSI_OLD } else { Fetch-PriorMsi }

"PHASE=preflight new=$(if ($new) { $new } else { 'none' }) old=$(if ($old) { $old } else { 'none' })"
if (-not $new -or -not (Test-Path $new)) {
    "RESULT=FAIL no new .msi found (set PORTZERO_MSI or drop one under vmtest\.downloaded-artifacts\windows\)"
    exit 1
}
if (-not $old -or -not (Test-Path $old)) {
    "PHASE=prior-version skipped=unavailable (set PORTZERO_MSI_OLD or provide network+gh)"
    "RESULT=SKIP upgrade (no prior-version .msi to upgrade FROM)"
    exit 0
}

$newVer = Get-MsiProductVersion -Path $new
$oldVer = Get-MsiProductVersion -Path $old
"PHASE=versions old=$oldVer new=$newVer"
if ($newVer -and $oldVer -and ($newVer -eq $oldVer)) {
    "PHASE=version-bump skipped=same-version (MajorUpgrade needs a version bump: old=$oldVer new=$newVer)"
    "RESULT=SKIP upgrade (prior and new MSI carry the same ProductVersion)"
    exit 0
}

# --- INSTALL PRIOR ---------------------------------------------------------
">> installing prior version ($old)"
Phase 'prior-install-exit' (Install-Msi -Path $old -LogName 'pz-msi-old.log')
Wait-Overlay
Phase 'prior-installed' (Test-Path $InstalledExe)
Phase 'prior-single-product' ((Get-RelatedProductCount -Code $UpgradeCode) -eq 1)
Phase 'prior-single-task' ((Get-TaskCount) -eq 1)
Phase 'prior-single-nrpt' ((Get-NrptCount) -eq 1)
Phase 'prior-single-adapter' ((Get-AdapterCount) -eq 1)

# --- UPGRADE (new MSI over the top; MajorUpgrade replaces in place) ---------
">> upgrading to new version ($new)"
Phase 'upgrade-install-exit' (Install-Msi -Path $new -LogName 'pz-msi-new.log')
Wait-Overlay
Phase 'upgrade-installed' (Test-Path $InstalledExe)
# The core upgrade invariants: MajorUpgrade replaced, nothing DUPLICATED.
Phase 'upgrade-single-product' ((Get-RelatedProductCount -Code $UpgradeCode) -eq 1)
Phase 'upgrade-single-task' ((Get-TaskCount) -eq 1)
Phase 'upgrade-single-nrpt' ((Get-NrptCount) -eq 1)
Phase 'upgrade-single-adapter' ((Get-AdapterCount) -eq 1)

# The installed binary is the NEW version.
$installedVer = ''
if (Test-Path $InstalledExe) {
    try { $installedVer = (& $InstalledExe --version 2>&1 | Out-String).Trim() } catch { }
}
"PHASE=installed-version value=$installedVer"
if ($newVer) {
    Phase 'upgrade-binary-is-new' ($installedVer -match [regex]::Escape($newVer))
} else {
    # Couldn't read the new MSI's ProductVersion; fall back to proving the
    # binary at least runs, and note the weaker assertion.
    "WARN=new-msi-productversion-unreadable-falling-back-to-runs-check"
    Phase 'upgrade-binary-runs' ($installedVer -ne '')
}

# --- CLEANUP (remove the upgraded product) ---------------------------------
if (Test-Path $InstalledExe) {
    ">> trust uninstall"
    $null = Invoke-Guarded -Label 'trust-uninstall' -TimeoutSec 60 -ArgList @($InstalledExe) -Script {
        param($exe)
        & $exe trust uninstall 2>&1 | Out-Null
    }
}
">> msiexec /x"
$logx = Join-Path $env:TEMP 'pz-msi-upgrade-uninstall.log'
$null = Invoke-Guarded -Label 'msiexec-uninstall' -TimeoutSec 180 -ArgList @($new, $logx) -Script {
    param($msiPath, $logPath)
    Start-Process msiexec.exe -ArgumentList @('/x', "`"$msiPath`"", '/qn', '/norestart', '/l*v', "`"$logPath`"") -Wait
}
Start-Sleep -Seconds 3
Phase 'clean-product-gone' ((Get-RelatedProductCount -Code $UpgradeCode) -eq 0)

if ($script:fails -eq 0) {
    "RESULT=PASS upgrade (prior -> new, MajorUpgrade replaced: single product/task/nrpt/adapter)"
    exit 0
}
"RESULT=FAIL upgrade assertions failed=$($script:fails)"
exit 1
