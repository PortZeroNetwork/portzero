#Requires -Version 5
# FULL E2E: local overlay tunnel (.portzero.local). No cloud, no secrets.
# Exercises the privileged overlay path end to end: wintun TUN + scoped DNS +
# local proxy. Serves a tagged process, starts the daemon, then proves the name
# resolves through the overlay and HTTP returns the served body.
#
# Two tagged services are served, not one: a normal IPv4-loopback service and a
# second bound to `::1` ALONE. The IPv6 one is the regression this covers — such
# a service is discovered correctly (right domain, right port, right PID) and
# was then proxied to an empty 127.0.0.1, so the tunnel connected and returned
# zero bytes. Only the daemon's backend dial differs; the client side of the
# overlay is IPv4 either way.
#
# Prints PHASE/RESULT lines; exits non-zero on failure. Run as admin (SYSTEM).
$ErrorActionPreference = 'Stop'
$exe = 'C:\pz-target\x86_64-pc-windows-msvc\release\portzero.exe'
$name = "vmtestlocal"
$domain = "$name.portzero.local"
$body = "portzero-local-overlay-ok"
$name6 = "vmtestlocal6"
$domain6 = "$name6.portzero.local"
$body6 = "portzero-local-overlay-ipv6-ok"
$lib = Join-Path (Split-Path $PSCommandPath) 'lib\http-echo.ps1'
$work = Join-Path $env:TEMP ("pz-local-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $work | Out-Null
$svcOut = Join-Path $work 'svc.out'

"PHASE=preflight exe=$(Test-Path $exe) wintun=$(Test-Path (Join-Path (Split-Path $exe) 'wintun.dll'))"
# Local-only: make sure no auth.json biases the daemon toward cloud mode.
$authJson = Join-Path $env:USERPROFILE '.portzero\auth.json'
if (Test-Path $authJson) { Remove-Item $authJson -Force }

$svc = $null
$svc6 = $null
try {
    # --- tagged service on a fixed port (its env carries PZ_TUNNEL so the daemon
    # discovers it). PS 5.1 has no Start-Process -Environment: set PZ_TUNNEL so
    # the child inherits it, then CLEAR it before starting the daemon (else the
    # daemon inherits PZ_TUNNEL and discovers its own management port).
    $port = 18080
    $env:PZ_TUNNEL = $domain
    $svc = Start-Process powershell -PassThru -WindowStyle Hidden `
        -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',$lib,'-Body',$body,'-Port',"$port") `
        -RedirectStandardOutput $svcOut -RedirectStandardError (Join-Path $work 'svc.err')
    Remove-Item Env:\PZ_TUNNEL -ErrorAction SilentlyContinue
    # Prove the service actually serves locally before involving the daemon.
    $up = $false
    for ($i=0; $i -lt 30; $i++) {
        try { $null = Invoke-WebRequest "http://127.0.0.1:$port/" -TimeoutSec 2 -UseBasicParsing; $up = $true; break } catch {}
        Start-Sleep -Milliseconds 500
    }
    if (-not $up) { throw "service not listening on $port; err=$(Get-Content (Join-Path $work 'svc.err') -Raw -EA SilentlyContinue)" }
    "PHASE=service pid=$($svc.Id) port=$port tunnel=$domain"

    # --- second tagged service, bound to ::1 ONLY. If the guest cannot serve it
    # at all (IPv6 disabled), the IPv6 leg is skipped rather than failed — but a
    # service that IS reachable on [::1] directly and NOT through its tunnel is a
    # real failure, asserted below.
    $port6 = 18081
    $env:PZ_TUNNEL = $domain6
    $svc6 = Start-Process powershell -PassThru -WindowStyle Hidden `
        -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',$lib,'-Body',$body6,'-Port',"$port6",'-BindAddress','::1') `
        -RedirectStandardOutput (Join-Path $work 'svc6.out') -RedirectStandardError (Join-Path $work 'svc6.err')
    Remove-Item Env:\PZ_TUNNEL -ErrorAction SilentlyContinue
    $up6 = $false
    for ($i=0; $i -lt 20; $i++) {
        try { $null = Invoke-WebRequest "http://[::1]:$port6/" -TimeoutSec 2 -UseBasicParsing; $up6 = $true; break } catch {}
        Start-Sleep -Milliseconds 500
    }
    if ($up6) {
        "PHASE=service-ipv6 pid=$($svc6.Id) port=$port6 tunnel=$domain6"
    } else {
        "PHASE=service-ipv6 ok=SKIP reason=`"no IPv6 loopback service on this guest; $(Get-Content (Join-Path $work 'svc6.err') -Raw -EA SilentlyContinue)`""
        Stop-Process -Id $svc6.Id -Force -ErrorAction SilentlyContinue
        $svc6 = $null
    }

    # --- start the daemon (creates wintun TUN, scoped DNS, overlay) ---
    # Fire-and-forget: `portzero start` can block on this build, so launch it
    # detached (own hidden process, output to a file so no pipe/console
    # inheritance) and prove readiness via `status` polling below.
    $startLog = Join-Path $work 'start.log'
    Start-Process -FilePath $exe -ArgumentList 'start','--no-browser' -WindowStyle Hidden `
        -RedirectStandardOutput $startLog -RedirectStandardError (Join-Path $work 'start.err')
    "PHASE=daemon launched (verifying via the overlay URL directly)"

    # --- prove the overlay resolves + serves. This IS the readiness gate: no
    # dependence on `portzero status` (which can block during overlay startup).
    # ~120s covers overlay bring-up (~25s on SSD, ~85s on slow disk).
    $ok = $false
    for ($i=1; $i -le 60; $i++) {
        try {
            $resp = Invoke-WebRequest -Uri "http://$domain/" -TimeoutSec 5 -UseBasicParsing
            if ($resp.Content.Trim() -eq $body) { $ok = $true; break }
        } catch { }
        Start-Sleep -Seconds 2
    }
    if (-not $ok) {
        $dlog = Join-Path $env:USERPROFILE '.portzero\daemon\daemon.log'
        if (Test-Path $dlog) { "PHASE=diag daemon-log-tail:"; Get-Content $dlog -Tail 8 }
        "RESULT=FAIL domain=$domain (overlay did not serve within timeout)"
        exit 1
    }
    try { $ip = (Resolve-DnsName $domain -ErrorAction SilentlyContinue | Where-Object {$_.IPAddress} | Select-Object -First 1).IPAddress } catch {}
    "PHASE=tunnel ok=true domain=$domain overlay_ip=$ip body_ok=true"

    # --- the IPv6-only backend must be reachable through its tunnel too. The
    # overlay is already up by now, so this needs far less patience than above.
    if ($svc6) {
        $ok6 = $false
        for ($i=1; $i -le 30; $i++) {
            try {
                $resp = Invoke-WebRequest -Uri "http://$domain6/" -TimeoutSec 5 -UseBasicParsing
                if ($resp.Content.Trim() -eq $body6) { $ok6 = $true; break }
            } catch { }
            Start-Sleep -Seconds 2
        }
        if (-not $ok6) {
            $dlog = Join-Path $env:USERPROFILE '.portzero\daemon\daemon.log'
            if (Test-Path $dlog) { "PHASE=diag daemon-log-tail:"; Get-Content $dlog -Tail 8 }
            "RESULT=FAIL domain=$domain6 (IPv6-only backend not reachable through its tunnel)"
            exit 1
        }
        "PHASE=tunnel-ipv6 ok=true domain=$domain6 body_ok=true"
    }

    "RESULT=PASS domain=$domain overlay_ip=$ip body_ok=true ipv6_domain=$(if ($svc6) { $domain6 } else { 'skipped' })"
}
finally {
    # Bounded stop: `portzero stop` has been seen to wedge when the overlay/TUN
    # is in a bad state, which would hang this whole script. Give graceful stop
    # 10s, then force-kill the process so cleanup ALWAYS completes.
    $j = Start-Job { & $using:exe stop 2>&1 | Out-Null }
    if (-not (Wait-Job $j -Timeout 10)) { Stop-Job $j -ErrorAction SilentlyContinue; "WARN=stop-wedged-forced-kill" }
    Remove-Job $j -Force -ErrorAction SilentlyContinue
    Get-Process portzero -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    if ($svc) { Stop-Process -Id $svc.Id -Force -ErrorAction SilentlyContinue }
    if ($svc6) { Stop-Process -Id $svc6.Id -Force -ErrorAction SilentlyContinue }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
