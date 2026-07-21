#Requires -Version 5
# FULL E2E: staging cloud tunnel. Authenticates a test client against real
# staging (devenvtools.top) with the rotated seed token, registers a public
# tunnel through the in-VM-built daemon, approves it, and fetches the public URL.
# Adapted from scripts/client-interop-e2e.ps1 but uses the built exe, the
# python-free http-echo helper, and a unique per-run tunnel name.
$ErrorActionPreference = 'Stop'
$exe = 'C:\pz-target\x86_64-pc-windows-msvc\release\portzero.exe'
$domain = 'devenvtools.top'
$secretsFile = '\\Mac\MBP-Sidecar\loumtech\vm-toolchain-cache\common\staging-e2e.env'
$lib = Join-Path (Split-Path $PSCommandPath) 'lib\http-echo.ps1'

# Unique per-run identity so we never collide with leftover staging route state.
$suffix = (Get-Date -Format 'MMddHHmmss')
$user = "vmtestwin$suffix"           # tunnel label == account username
$email = "vmtest-win-$suffix@example.com"
$account = "vmtest-win-$suffix"
$code = "424242"
$body = "portzero-staging-tunnel-ok"

$apiUrl = "https://app.$domain/api"
$edgeUrl = "wss://edge.$domain/tunnel"
$tunnel = "$user.tunnel.$domain"

# --- seed token from the offline cache ---
$seed = (Get-Content $secretsFile | Where-Object { $_ -match '^TEST_LOGIN_SEED_TOKEN=' }) -replace '^TEST_LOGIN_SEED_TOKEN=',''
if (-not $seed) { throw "no TEST_LOGIN_SEED_TOKEN in $secretsFile" }
"PHASE=preflight exe=$(Test-Path $exe) tunnel=$tunnel token=$([bool]$seed)"

$work = Join-Path $env:TEMP ("pz-staging-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $work | Out-Null
$svcOut = Join-Path $work 'svc.out'
$authDir = Join-Path $env:USERPROFILE '.portzero'
$svc = $null

try {
    # --- staging up? ---
    $pf = try { (Invoke-WebRequest "https://app.$domain/" -TimeoutSec 10 -UseBasicParsing).StatusCode } catch { 0 }
    if ($pf -ne 200) { throw "staging not up (HTTP $pf)" }

    # --- seed an auth code, then verify to get a JWT ---
    $seedBody = @{ email=$email; username=$user; account_id=$account; code=$code } | ConvertTo-Json -Compress
    Invoke-RestMethod -Method Post -Uri "$apiUrl/auth/test-seed-login" -Headers @{ Authorization="Bearer $seed" } -ContentType 'application/json' -Body $seedBody | Out-Null
    $verify = Invoke-RestMethod -Method Post -Uri "$apiUrl/auth/verify" -ContentType 'application/json' -Body (@{ email=$email; code=$code } | ConvertTo-Json -Compress)
    New-Item -ItemType Directory -Force -Path $authDir | Out-Null
    (@{ email=$verify.email; token=$verify.token; account_id=$verify.account_id; username=$verify.username } | ConvertTo-Json -Compress) |
        Set-Content -Path (Join-Path $authDir 'auth.json') -NoNewline
    "PHASE=auth user=$($verify.username)"

    # --- tagged service on a fixed port; ":80" tells the daemon the canonical
    # external port, the daemon auto-discovers the real local port. PS 5.1:
    # inherit PZ_TUNNEL via env, then clear it before the daemon starts.
    $port = 18080
    $env:PZ_TUNNEL = "${tunnel}:80"
    $svc = Start-Process powershell -PassThru -WindowStyle Hidden `
        -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',$lib,'-Body',$body,'-Port',"$port") `
        -RedirectStandardOutput $svcOut -RedirectStandardError (Join-Path $work 'svc.err')
    Remove-Item Env:\PZ_TUNNEL -ErrorAction SilentlyContinue
    $up = $false
    for ($i=0; $i -lt 30; $i++) { try { $null = Invoke-WebRequest "http://127.0.0.1:$port/" -TimeoutSec 2 -UseBasicParsing; $up = $true; break } catch {}; Start-Sleep -Milliseconds 500 }
    if (-not $up) { throw "service not listening on $port" }
    "PHASE=service port=$port"

    # --- start daemon pointed at staging (fire-and-forget; `start` can block) ---
    $env:PZ_TUNNEL_API_URL = $apiUrl; $env:PZ_TUNNEL_EDGE_URL = $edgeUrl; $env:PZ_TUNNEL_BASE_DOMAIN = $domain
    Start-Process -FilePath $exe -ArgumentList 'start','--no-browser' -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $work 'start.log') -RedirectStandardError (Join-Path $work 'start.err')
    "PHASE=daemon-launched"

    # --- readiness gate: the route appearing in the cloud API (no `portzero
    # status` dependency, which can block during startup) ---
    $headers = @{ Authorization = "Bearer $($verify.token)" }
    $registered = $false
    for ($i=1; $i -le 45; $i++) {
        try { $routes = Invoke-RestMethod -Uri "$apiUrl/routes" -Headers $headers; if ($routes | Where-Object { $_.domain -eq $tunnel }) { $registered = $true; break } } catch {}
        Start-Sleep -Seconds 2
    }
    if (-not $registered) {
        $dlog = Join-Path $authDir 'daemon\daemon.log'
        if (Test-Path $dlog) { "PHASE=diag daemon-log-tail:"; Get-Content $dlog -Tail 8 }
        throw "route did not register: $tunnel"
    }
    "PHASE=route-registered"

    # --- approve + fetch the public tunnel ---
    Invoke-RestMethod -Method Post -Uri "$apiUrl/routes/$tunnel/approve" -Headers $headers -ContentType 'application/json' -Body '{}' | Out-Null
    $ok = $false
    for ($i=1; $i -le 30; $i++) { try { $r = Invoke-WebRequest "https://$tunnel/" -TimeoutSec 10 -UseBasicParsing; if ($r.Content.Trim() -eq $body) { $ok=$true; break } } catch {}; Start-Sleep -Seconds 2 }
    if ($ok) { "RESULT=PASS tunnel=https://$tunnel body_ok=true" }
    else { "RESULT=FAIL tunnel=https://$tunnel (public URL did not return expected body)"; exit 1 }
}
finally {
    # Bounded stop (see e2e-local-overlay): never let cleanup wedge the script.
    $env:PZ_TUNNEL_API_URL=$apiUrl; $env:PZ_TUNNEL_EDGE_URL=$edgeUrl; $env:PZ_TUNNEL_BASE_DOMAIN=$domain
    $j = Start-Job { & $using:exe stop 2>&1 | Out-Null }
    if (-not (Wait-Job $j -Timeout 10)) { Stop-Job $j -ErrorAction SilentlyContinue; "WARN=stop-wedged-forced-kill" }
    Remove-Job $j -Force -ErrorAction SilentlyContinue
    Get-Process portzero -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    if ($svc) { Stop-Process -Id $svc.Id -Force -ErrorAction SilentlyContinue }
    if (Test-Path (Join-Path $authDir 'auth.json')) { Remove-Item (Join-Path $authDir 'auth.json') -Force -ErrorAction SilentlyContinue }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
