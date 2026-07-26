#Requires -Version 5
# COMBINED E2E (Windows): one VM session, one reset, covering everything the
# four old separate flavors (local-overlay, staging-tunnel, lifecycle, upgrade)
# used to check across four separate resets. Flow:
#
#   install -> local-tunnel test -> upgrade -> cloud-tunnel test -> uninstall
#
# Concretely: install the PRIOR released MSI if one is available (else install
# the new MSI directly and skip the upgrade step), prove the real overlay
# (.portzero.local via wintun) works against whatever just got installed, install
# the NEW MSI over the top and assert the WiX <MajorUpgrade> replaced it in place
# (no side-by-side, nothing duplicated), prove the real cloud tunnel works
# against the now-installed new binary, then uninstall and assert every artifact
# (binary, scheduled task, trust-store CA, NRPT rule, Wintun adapter) is GONE.
#
# Prints greppable `PHASE=<name> ... ok=<true|false>` lines, then RESULT=PASS|FAIL.
# Run as admin / SYSTEM. Every step that can wedge is time-boxed with a child job.
#
# Env:
#   PORTZERO_MSI      new (under-test) .msi. If unset, searches
#                     vmtest\.downloaded-artifacts\windows\*.msi.
#   PORTZERO_MSI_OLD  prior-version .msi to install FIRST. If unset, fetches the
#                     latest published release .msi via `gh`. When neither is
#                     available (or the two MSIs share a ProductVersion) the
#                     upgrade step SKIPs cleanly and the local/cloud tunnel tests
#                     run against the new MSI installed directly.
#   STAGING_SECRETS_FILE  path to the staging-e2e.env seed-token file for the
#                     cloud-tunnel phase. Defaults to the MBP-Sidecar share path.
#   COMBINED_SKIP_CLOUD=1  skip the cloud-tunnel phase outright. Off by default
#                      — and the only supported way to skip it. With a seed
#                      token present, a staging that does not answer 200 FAILS
#                      the run.
$ErrorActionPreference = 'Continue'

$script:fails = 0
$UpgradeCode = '{8D64B464-96F7-4CB9-A452-372F0AC067AF}'
$CertSubjectMatch = 'PortZero Local CA'
$TaskName = 'cloud.portzero.daemon'
$NrptMatch = 'portzero.local'
$AdapterName = 'deven0'
$InstalledExe = Join-Path ${env:ProgramFiles} 'Port Zero\portzero.exe'
$InstalledTrayExe = Join-Path ${env:ProgramFiles} 'Port Zero\portzero-tray.exe'
$TrayRunKey = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run'
$StartMenuShortcut = Join-Path ${env:ProgramData} 'Microsoft\Windows\Start Menu\Programs\PortZero\PortZero.lnk'

function Phase {
    param([string] $Name, [bool] $Ok)
    if ($Ok) {
        "PHASE=$Name ok=true"
    } else {
        "PHASE=$Name ok=false"
        $script:fails++
    }
}
# Like Phase, but never counts as a failure: reports ok=true when the
# predicate holds, else ok=SKIP with a reason. Reserved for runtime states a
# headless `prlctl exec` install genuinely cannot guarantee — the HKLM Run
# value only autostarts the tray at an interactive logon, which msiexec
# running as SYSTEM here does not produce.
function Phase-Skip {
    param([string] $Name, [bool] $Ok, [string] $Reason)
    if ($Ok) { "PHASE=$Name ok=true" }
    else { "PHASE=$Name ok=SKIP reason=`"$Reason`"" }
}

# Run a scriptblock under a hard timeout so a wedged msiexec / daemon call never
# hangs the whole run (same discipline as repro-trust-install.ps1).
# Returns the scriptblock's own return value on success (so a caller can check
# a real exit code, not just "did it finish in time"); $false on timeout.
function Invoke-Guarded {
    param([scriptblock] $Script, [object[]] $ArgList = @(), [int] $TimeoutSec = 180, [string] $Label = 'step')
    $job = Start-Job -ScriptBlock $Script -ArgumentList $ArgList
    if (Wait-Job $job -Timeout $TimeoutSec) {
        $result = Receive-Job $job 2>&1
        Remove-Job $job -Force -ErrorAction SilentlyContinue
        return $result
    }
    Stop-Job $job -ErrorAction SilentlyContinue
    Remove-Job $job -Force -ErrorAction SilentlyContinue
    "WARN=$Label-timed-out-after-${TimeoutSec}s"
    return $false
}

# --- predicates ---------------------------------------------------------
function Test-CertPresent {
    $c = Get-ChildItem Cert:\LocalMachine\Root -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -match $CertSubjectMatch }
    return [bool] $c
}
function Test-TaskPresent { return [bool] (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) }
function Test-NrptPresent {
    $r = Get-DnsClientNrptRule -ErrorAction SilentlyContinue | Where-Object { $_.Namespace -match $NrptMatch }
    return [bool] $r
}
function Test-AdapterPresent { return [bool] (Get-NetAdapter -Name $AdapterName -ErrorAction SilentlyContinue) }
function Test-TrayAutostartRegistered {
    $v = Get-ItemProperty -Path $TrayRunKey -Name 'PortZeroTray' -ErrorAction SilentlyContinue
    return [bool] $v
}
function Test-StartMenuShortcutPresent { return (Test-Path $StartMenuShortcut) }
function Test-TrayProcessRunning { return [bool] (Get-Process -Name portzero-tray -ErrorAction SilentlyContinue) }
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
function Get-TaskCount { return @(Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue).Count }
function Get-NrptCount { return @(Get-DnsClientNrptRule -ErrorAction SilentlyContinue | Where-Object { $_.Namespace -match $NrptMatch }).Count }
function Get-AdapterCount { return @(Get-NetAdapter -Name $AdapterName -ErrorAction SilentlyContinue).Count }

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
        if ($rec) { return $rec.GetType().InvokeMember('StringData', 'GetProperty', $null, $rec, 1) }
    } catch { }
    return $null
}

function Install-Msi {
    param([string] $Path, [string] $LogName)
    $log = Join-Path $env:TEMP $LogName

    # Always install from a guest-local copy. The MSI normally arrives over the
    # Parallels shared folder (\\Mac\Home\...), and msiexec runs its install
    # service as LOCAL SYSTEM, which has no access to the share mapped for the
    # interactive user — so installing in place fails with a bare non-zero exit
    # code that looks exactly like a broken package. Copying first costs a
    # second and takes the host filesystem out of what this test measures.
    $local = Join-Path $env:TEMP ([System.IO.Path]::GetFileName($Path))
    try {
        Copy-Item -LiteralPath $Path -Destination $local -Force -ErrorAction Stop
    } catch {
        "WARN=msi-copy-failed src=$Path err=$($_.Exception.Message)"
        $local = $Path
    }

    $raw = Invoke-Guarded -Label "msiexec-$LogName" -TimeoutSec 180 -ArgList @($local, $log) -Script {
        param($msiPath, $logPath)
        $p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$msiPath`"", '/qn', '/norestart', '/l*v', "`"$logPath`"") -Wait -PassThru
        return $p.ExitCode
    }

    # Invoke-Guarded emits its WARN line into the pipeline before returning, so
    # on timeout the caller receives @('WARN=...', $false), not a bare $false —
    # and a 2-element array casts to $true, which would have reported a wedged
    # install as a passing one. Take the last emitted value, which is the
    # scriptblock's return in both paths.
    $code = if ($raw -is [array]) { $raw[-1] } else { $raw }

    if ($code -is [int] -and $code -eq 0) { return $true }

    # A bare `ok=false` gives whoever reads this run nothing to act on. Surface
    # the msiexec exit code and the last of its verbose log, which is where the
    # actual reason (blocked path, failed custom action, missing dependency)
    # always is.
    "WARN=msiexec-exit code=$code msi=$local"
    if (Test-Path $log) {
        "WARN=msiexec-log-tail ---"
        Get-Content $log -Tail 25 -ErrorAction SilentlyContinue
        "WARN=msiexec-log-end ---"
    } else {
        "WARN=msiexec-log-missing path=$log"
    }
    return $false
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
    & gh release download --repo PortZeroNetwork/portzero --pattern '*.msi' --dir $out 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { return $null }
    $msi = Get-ChildItem -Path $out -Filter '*.msi' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($msi) { return $msi.FullName }
    return $null
}

# --- local-tunnel helper: serve a tagged echo, start the daemon, assert the
# .portzero.local domain resolves+serves through the overlay directly, then
# stop the daemon. Local-only: a cloud tunnel's public domain isn't reachable
# via a same-host http:// poll the way the overlay intercepts .portzero.local
# (it needs route-registration + approval first, and only serves over the real
# public HTTPS edge) — see Invoke-CloudTunnelTest below for that flow.
function Invoke-TunnelTest {
    param([string] $Prefix, [string] $TunnelEnvValue, [string] $Domain, [string] $Body)
    $lib = Join-Path $PSScriptRoot 'lib\http-echo.ps1'
    $work = Join-Path $env:TEMP ("pz-$Prefix-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    $svcOut = Join-Path $work 'svc.out'
    $svc = $null
    try {
        $port = 18080
        $env:PZ_TUNNEL = $TunnelEnvValue
        $svc = Start-Process powershell -PassThru -WindowStyle Hidden `
            -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',$lib,'-Body',$Body,'-Port',"$port") `
            -RedirectStandardOutput $svcOut -RedirectStandardError (Join-Path $work 'svc.err')
        Remove-Item Env:\PZ_TUNNEL -ErrorAction SilentlyContinue
        $up = $false
        for ($i = 0; $i -lt 30; $i++) {
            try { $null = Invoke-WebRequest "http://127.0.0.1:$port/" -TimeoutSec 2 -UseBasicParsing; $up = $true; break } catch {}
            Start-Sleep -Milliseconds 500
        }
        if (-not $up) { Phase "$Prefix-service" $false; return }
        "PHASE=$Prefix-service ok=true port=$port tunnel=$Domain"

        Start-Process -FilePath $InstalledExe -ArgumentList 'start','--no-browser' -WindowStyle Hidden `
            -RedirectStandardOutput (Join-Path $work 'start.log') -RedirectStandardError (Join-Path $work 'start.err')
        "PHASE=$Prefix-daemon-launched ok=true"

        $ok = $false
        for ($i = 1; $i -le 60; $i++) {
            try {
                $resp = Invoke-WebRequest -Uri "http://$Domain/" -TimeoutSec 5 -UseBasicParsing
                if ($resp.Content.Trim() -eq $Body) { $ok = $true; break }
            } catch { }
            Start-Sleep -Seconds 2
        }
        if ($ok) {
            "PHASE=$Prefix-tunnel ok=true domain=$Domain"
        } else {
            $dlog = Join-Path $env:USERPROFILE '.portzero\daemon\daemon.log'
            if (Test-Path $dlog) { "PHASE=diag daemon-log-tail:"; Get-Content $dlog -Tail 8 }
            Phase "$Prefix-tunnel" $false
        }
    } finally {
        $j = Start-Job { & $using:InstalledExe stop 2>&1 | Out-Null }
        if (-not (Wait-Job $j -Timeout 10)) { Stop-Job $j -ErrorAction SilentlyContinue; "WARN=stop-wedged-forced-kill" }
        Remove-Job $j -Force -ErrorAction SilentlyContinue
        Get-Process portzero -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        if ($svc) { Stop-Process -Id $svc.Id -Force -ErrorAction SilentlyContinue }
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
}

# --- cloud-tunnel helper: serve a tagged echo, start the daemon pointed at
# staging, wait for the route to actually REGISTER via the cloud API (the
# real readiness gate — the daemon connects out to the edge asynchronously),
# approve it, then fetch the real public HTTPS URL. Keeps the daemon alive
# through approval + fetch and stops it exactly once at the end.
function Invoke-CloudTunnelTest {
    param([string] $Tunnel, [string] $Token, [string] $ApiUrl, [string] $EdgeUrl, [string] $BaseDomain, [string] $Body)
    $lib = Join-Path $PSScriptRoot 'lib\http-echo.ps1'
    $work = Join-Path $env:TEMP ("pz-cloud-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    $svcOut = Join-Path $work 'svc.out'
    $svc = $null
    try {
        $port = 18080
        $env:PZ_TUNNEL = "${Tunnel}:80"
        $svc = Start-Process powershell -PassThru -WindowStyle Hidden `
            -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',$lib,'-Body',$Body,'-Port',"$port") `
            -RedirectStandardOutput $svcOut -RedirectStandardError (Join-Path $work 'svc.err')
        Remove-Item Env:\PZ_TUNNEL -ErrorAction SilentlyContinue
        $up = $false
        for ($i = 0; $i -lt 30; $i++) {
            try { $null = Invoke-WebRequest "http://127.0.0.1:$port/" -TimeoutSec 2 -UseBasicParsing; $up = $true; break } catch {}
            Start-Sleep -Milliseconds 500
        }
        if (-not $up) { Phase 'cloud-service' $false; return }
        "PHASE=cloud-service ok=true port=$port tunnel=$Tunnel"

        $env:PZ_TUNNEL_API_URL = $ApiUrl; $env:PZ_TUNNEL_EDGE_URL = $EdgeUrl; $env:PZ_TUNNEL_BASE_DOMAIN = $BaseDomain
        Start-Process -FilePath $InstalledExe -ArgumentList 'start','--no-browser' -WindowStyle Hidden `
            -RedirectStandardOutput (Join-Path $work 'start.log') -RedirectStandardError (Join-Path $work 'start.err')
        "PHASE=cloud-daemon-launched ok=true"

        $headers = @{ Authorization = "Bearer $Token" }
        $registered = $false
        for ($i = 1; $i -le 45; $i++) {
            try { $routes = Invoke-RestMethod -Uri "$ApiUrl/routes" -Headers $headers; if ($routes | Where-Object { $_.domain -eq $Tunnel }) { $registered = $true; break } } catch {}
            Start-Sleep -Seconds 2
        }
        if (-not $registered) {
            Phase 'cloud-route-registered' $false
            $dlog = Join-Path $env:USERPROFILE '.portzero\daemon\daemon.log'
            if (Test-Path $dlog) { "PHASE=diag daemon-log-tail:"; Get-Content $dlog -Tail 8 }
        } else {
            "PHASE=cloud-route-registered ok=true"
            try { Invoke-RestMethod -Method Post -Uri "$ApiUrl/routes/$Tunnel/approve" -Headers $headers -ContentType 'application/json' -Body '{}' | Out-Null } catch {}
            $ok = $false
            for ($i = 1; $i -le 30; $i++) {
                try { $r = Invoke-WebRequest "https://$Tunnel/" -TimeoutSec 10 -UseBasicParsing; if ($r.Content.Trim() -eq $Body) { $ok = $true; break } } catch {}
                Start-Sleep -Seconds 2
            }
            Phase 'cloud-public-url' $ok
        }
    } finally {
        $env:PZ_TUNNEL_API_URL = $ApiUrl; $env:PZ_TUNNEL_EDGE_URL = $EdgeUrl; $env:PZ_TUNNEL_BASE_DOMAIN = $BaseDomain
        $j = Start-Job { & $using:InstalledExe stop 2>&1 | Out-Null }
        if (-not (Wait-Job $j -Timeout 10)) { Stop-Job $j -ErrorAction SilentlyContinue; "WARN=stop-wedged-forced-kill" }
        Remove-Job $j -Force -ErrorAction SilentlyContinue
        Remove-Item Env:\PZ_TUNNEL_API_URL,Env:\PZ_TUNNEL_EDGE_URL,Env:\PZ_TUNNEL_BASE_DOMAIN -ErrorAction SilentlyContinue
        Get-Process portzero -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        if ($svc) { Stop-Process -Id $svc.Id -Force -ErrorAction SilentlyContinue }
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
}

# =============================================================================
$new = Find-NewMsi
$old = if ($env:PORTZERO_MSI_OLD -and (Test-Path $env:PORTZERO_MSI_OLD)) { $env:PORTZERO_MSI_OLD } else { Fetch-PriorMsi }
"PHASE=preflight new=$(if ($new) { $new } else { 'none' }) old=$(if ($old) { $old } else { 'none' })"
if (-not $new -or -not (Test-Path $new)) {
    "RESULT=FAIL no new .msi found (set PORTZERO_MSI or drop one under vmtest\.downloaded-artifacts\windows\)"
    exit 1
}

$haveUpgrade = $false
if ($old -and (Test-Path $old)) {
    $newVer = Get-MsiProductVersion -Path $new
    $oldVer = Get-MsiProductVersion -Path $old
    "PHASE=versions old=$oldVer new=$newVer"
    if ($newVer -and $oldVer -and ($newVer -eq $oldVer)) {
        "PHASE=version-bump skipped=same-version (MajorUpgrade needs a version bump: old=$oldVer new=$newVer)"
    } else {
        $haveUpgrade = $true
    }
}

# --- 1. INSTALL (real signed MSI: prior version if we have one, else new) --
$first = if ($haveUpgrade) { $old } else { $new }
">> installing $first"
Phase 'install-exit' (Install-Msi -Path $first -LogName 'pz-msi-install.log')
Wait-Overlay
Phase 'install-binary' (Test-Path $InstalledExe)
Phase 'install-task' (Test-TaskPresent)
Phase 'install-trust-ca' (Test-CertPresent)
Phase 'install-nrpt-rule' (Test-NrptPresent)
Phase 'install-wintun-adapter' (Test-AdapterPresent)

# Tray icon + Start Menu shortcut: proves the MSI lands the tray binary,
# registers it to autostart at logon (HKLM Run), and gives the desktop app a
# Start Menu entry, all without any post-install step from the user.
Phase 'install-tray-binary' (Test-Path $InstalledTrayExe)
Phase 'install-tray-autostart-registered' (Test-TrayAutostartRegistered)
Phase 'install-start-menu-shortcut' (Test-StartMenuShortcutPresent)
# The Run key only autostarts the tray at an interactive logon; msiexec ran
# this install as SYSTEM with no logon event, so this can't be a hard assert.
Phase-Skip 'install-tray-running' (Test-TrayProcessRunning) `
    'HKLM Run only autostarts the tray at interactive logon; msiexec installed this as SYSTEM with no logon event, so this cannot be observed under headless prlctl exec (autostart is still registered regardless, via install-tray-autostart-registered)'

# --- 2. LOCAL TUNNEL TEST (real wintun TUN + scoped DNS + local proxy) ----
# Stop the daemon the MSI custom action auto-started so the local-tunnel test
# controls its own tagged service + a fresh `start`.
$j = Start-Job { & $using:InstalledExe stop 2>&1 | Out-Null }
if (-not (Wait-Job $j -Timeout 10)) { Stop-Job $j -ErrorAction SilentlyContinue }
Remove-Job $j -Force -ErrorAction SilentlyContinue
$authJson = Join-Path $env:USERPROFILE '.portzero\auth.json'
if (Test-Path $authJson) { Remove-Item $authJson -Force }
">> local-tunnel test against $first"
Invoke-TunnelTest -Prefix 'local' -TunnelEnvValue 'vmtestlocal.portzero.local' -Domain 'vmtestlocal.portzero.local' -Body 'portzero-local-overlay-ok'

# --- 3. UPGRADE (new MSI over the top; MajorUpgrade replaces in place) ----
if ($haveUpgrade) {
    Phase 'prior-single-product' ((Get-RelatedProductCount -Code $UpgradeCode) -eq 1)
    Phase 'prior-single-task' ((Get-TaskCount) -eq 1)
    Phase 'prior-single-nrpt' ((Get-NrptCount) -eq 1)
    Phase 'prior-single-adapter' ((Get-AdapterCount) -eq 1)

    ">> upgrading to new version ($new)"
    Phase 'upgrade-install-exit' (Install-Msi -Path $new -LogName 'pz-msi-upgrade.log')
    Wait-Overlay
    Phase 'upgrade-installed' (Test-Path $InstalledExe)
    Phase 'upgrade-single-product' ((Get-RelatedProductCount -Code $UpgradeCode) -eq 1)
    Phase 'upgrade-single-task' ((Get-TaskCount) -eq 1)
    Phase 'upgrade-single-nrpt' ((Get-NrptCount) -eq 1)
    Phase 'upgrade-single-adapter' ((Get-AdapterCount) -eq 1)

    $installedVer = ''
    if (Test-Path $InstalledExe) {
        try { $installedVer = (& $InstalledExe --version 2>&1 | Out-String).Trim() } catch { }
    }
    "PHASE=installed-version value=$installedVer"
    if ($newVer) {
        Phase 'upgrade-binary-is-new' ($installedVer -match [regex]::Escape($newVer))
    } else {
        "WARN=new-msi-productversion-unreadable-falling-back-to-runs-check"
        Phase 'upgrade-binary-runs' ($installedVer -ne '')
    }

    # Stop whatever the upgrade's custom action auto-started, same reason as step 2.
    $j = Start-Job { & $using:InstalledExe stop 2>&1 | Out-Null }
    if (-not (Wait-Job $j -Timeout 10)) { Stop-Job $j -ErrorAction SilentlyContinue }
    Remove-Job $j -Force -ErrorAction SilentlyContinue
} else {
    "PHASE=upgrade skipped=no-prior-artifact (set PORTZERO_MSI_OLD or provide network+gh)"
}

# --- 4. CLOUD TUNNEL TEST (real staging tunnel, against the new binary) ---
if ($env:COMBINED_SKIP_CLOUD -eq '1') {
    'PHASE=cloud-tunnel ok=SKIP reason="COMBINED_SKIP_CLOUD=1"'
} else {
    $stagingDomain = 'devenvtools.top'
    $secretsFile = if ($env:STAGING_SECRETS_FILE) { $env:STAGING_SECRETS_FILE } else { '\\Mac\MBP-Sidecar\loumtech\vm-toolchain-cache\common\staging-e2e.env' }
    $seed = if (Test-Path $secretsFile) {
        (Get-Content $secretsFile | Where-Object { $_ -match '^TEST_LOGIN_SEED_TOKEN=' }) -replace '^TEST_LOGIN_SEED_TOKEN=',''
    } else { $null }
    if (-not $seed) {
        "PHASE=cloud-tunnel ok=SKIP reason=`"no seed token at $secretsFile`""
    } else {
        # Probe staging up to three times before believing it is down: this
        # guest shares one physical host with the others (see vm-e2e.yml), so a
        # single 10s request can lose to a transient blip.
        $apiProbe = 0
        foreach ($probeAttempt in 1..3) {
            # Invoke-WebRequest throws on any non-2xx, so dig the real status out
            # of the exception: "staging answered 502" and "staging answered
            # nothing" need different fixes, and 0 for both hides that.
            $apiProbe = try { (Invoke-WebRequest "https://app.$stagingDomain/" -TimeoutSec 10 -UseBasicParsing).StatusCode }
                        catch { if ($_.Exception.Response) { [int] $_.Exception.Response.StatusCode } else { 0 } }
            if ($apiProbe -eq 200) { break }
            if ($probeAttempt -lt 3) { Start-Sleep -Seconds 5 }
        }
        if ($apiProbe -ne 200) {
            # This used to SKIP, which turned an unreachable staging into a
            # PASS with the whole cloud leg silently unrun — a green build that
            # proved nothing about the cloud path. Not reaching staging is a
            # failure; the deliberate opt-out is COMBINED_SKIP_CLOUD=1.
            Phase 'cloud-reachable' $false
            ">> staging did not answer 200 (got $apiProbe) at https://app.$stagingDomain/, so the cloud-tunnel test could not run."
            if ($apiProbe -eq 0) {
                '>>   0 means no response at all: this guest has no egress, DNS failed, or'
                '>>   the TLS handshake was rejected. Usually the guest network, not staging.'
            } else {
                ">>   staging answered $apiProbe, so it is up but unhealthy."
            }
            '>> next steps:'
            ">>   - curl https://app.$stagingDomain/ from the host; if that works, fix the guest's network"
            '>>   - if it fails there too, check the Deploy Staging workflow in portzero-cloud'
            '>>   - to run without the cloud leg on purpose, re-run with COMBINED_SKIP_CLOUD=1'
        } else {
            $suffix = (Get-Date -Format 'MMddHHmmss')
            $user = "vmtestwin$suffix"
            $email = "vmtest-win-$suffix@example.com"
            $account = "vmtest-win-$suffix"
            $code = "424242"
            $body = "portzero-staging-tunnel-ok"
            $apiUrl = "https://app.$stagingDomain/api"
            $edgeUrl = "wss://edge.$stagingDomain/tunnel"
            $tunnel = "$user.tunnel.$stagingDomain"

            try {
                $seedBody = @{ email=$email; username=$user; account_id=$account; code=$code } | ConvertTo-Json -Compress
                Invoke-RestMethod -Method Post -Uri "$apiUrl/auth/test-seed-login" -Headers @{ Authorization="Bearer $seed" } -ContentType 'application/json' -Body $seedBody | Out-Null
                $verify = Invoke-RestMethod -Method Post -Uri "$apiUrl/auth/verify" -ContentType 'application/json' -Body (@{ email=$email; code=$code } | ConvertTo-Json -Compress)
                $authDir = Join-Path $env:USERPROFILE '.portzero'
                New-Item -ItemType Directory -Force -Path $authDir | Out-Null
                (@{ email=$verify.email; token=$verify.token; account_id=$verify.account_id; username=$verify.username } | ConvertTo-Json -Compress) |
                    Set-Content -Path (Join-Path $authDir 'auth.json') -NoNewline
                "PHASE=cloud-auth ok=true user=$($verify.username)"

                Invoke-CloudTunnelTest -Tunnel $tunnel -Token $verify.token -ApiUrl $apiUrl -EdgeUrl $edgeUrl -BaseDomain $stagingDomain -Body $body
            } catch {
                Phase 'cloud-auth' $false
                "WARN=cloud-tunnel-exception: $($_.Exception.Message)"
            } finally {
                $authJson2 = Join-Path $env:USERPROFILE '.portzero\auth.json'
                if (Test-Path $authJson2) { Remove-Item $authJson2 -Force -ErrorAction SilentlyContinue }
            }
        }
    }
}

# --- 5. UNINSTALL (the way a user removes it) -------------------------------
if (Test-Path $InstalledExe) {
    ">> trust uninstall"
    $null = Invoke-Guarded -Label 'trust-uninstall' -TimeoutSec 60 -ArgList @($InstalledExe) -Script {
        param($exe)
        & $exe trust uninstall 2>&1 | Out-Null
    }
}
">> msiexec /x"
$logx = Join-Path $env:TEMP 'pz-msi-uninstall.log'
$null = Invoke-Guarded -Label 'msiexec-uninstall' -TimeoutSec 180 -ArgList @($new, $logx) -Script {
    param($msiPath, $logPath)
    Start-Process msiexec.exe -ArgumentList @('/x', "`"$msiPath`"", '/qn', '/norestart', '/l*v', "`"$logPath`"") -Wait
}

# --- ASSERT CLEAN: every install artifact must be GONE ---------------------
Start-Sleep -Seconds 3
Phase 'clean-binary' (-not (Test-Path $InstalledExe))
Phase 'clean-task' (-not (Test-TaskPresent))
Phase 'clean-trust-ca' (-not (Test-CertPresent))
Phase 'clean-nrpt-rule' (-not (Test-NrptPresent))
Phase 'clean-wintun-adapter' (-not (Test-AdapterPresent))
Phase 'clean-product-gone' ((Get-RelatedProductCount -Code $UpgradeCode) -eq 0)
Phase 'clean-tray-binary' (-not (Test-Path $InstalledTrayExe))
Phase 'clean-tray-autostart-registered' (-not (Test-TrayAutostartRegistered))
Phase 'clean-start-menu-shortcut' (-not (Test-StartMenuShortcutPresent))
Phase 'clean-tray-running' (-not (Test-TrayProcessRunning))

if ($script:fails -eq 0) {
    "RESULT=PASS combined (install -> local-tunnel -> upgrade -> cloud-tunnel -> uninstall -> assert-clean)"
    exit 0
}
"RESULT=FAIL combined assertions failed=$($script:fails)"
exit 1
