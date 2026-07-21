#!/usr/bin/env pwsh
# Client interop test (Windows): installs the just-built, signed portzero.exe
# via the real MSI (msiexec) and drives a real tunnel against the real,
# already-running staging environment in portzero-cloud. Does not stand up
# any infrastructure itself — if staging isn't already up, this fails fast
# with a clear error instead of hanging or trying to provision anything.
#
# Does not attempt to redirect the CLI's config directory (dirs::home_dir()
# resolves via the Win32 known-folder API on Windows, not %USERPROFILE%/%HOME%,
# so overriding them has no effect). Fine here since this only ever runs on a
# fresh, single-job GitHub-hosted windows-latest runner.

$ErrorActionPreference = "Stop"

function Get-RequiredEnv {
    param([string]$Name)
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrEmpty($value)) {
        throw "Set $Name"
    }
    return $value
}

$Domain = Get-RequiredEnv "STAGING_DOMAIN"
$SeedToken = Get-RequiredEnv "TEST_LOGIN_SEED_TOKEN"
$MsiPath = Get-RequiredEnv "PORTZERO_MSI_PATH"

$Email = if ($env:STAGING_CLIENT_E2E_EMAIL) { $env:STAGING_CLIENT_E2E_EMAIL } else { "staging-client-e2e-windows@example.com" }
# Unique per platform/repo so concurrent E2E runs against shared staging
# don't register the same tunnel domain (see client-interop-e2e.sh).
$Username = if ($env:STAGING_CLIENT_E2E_USERNAME) { $env:STAGING_CLIENT_E2E_USERNAME } else { "tunnel-e2e-windows" }
$AccountId = if ($env:STAGING_CLIENT_E2E_ACCOUNT_ID) { $env:STAGING_CLIENT_E2E_ACCOUNT_ID } else { "staging-client-e2e-windows-account" }
$VerifyCode = if ($env:STAGING_CLIENT_E2E_CODE) { $env:STAGING_CLIENT_E2E_CODE } else { "424242" }

$ApiUrl = "https://app.$Domain/api"
$EdgeUrl = "wss://edge.$Domain/tunnel"
$AuthUrl = "$ApiUrl/auth/verify"
$SeedUrl = "$ApiUrl/auth/test-seed-login"
# Cloud tunnels live on the shared `.tunnel.<domain>` apex, and the host before
# it is ONE DNS label — `<name>--<cloud-username>` joined with `--`, never a dot
# — so the single `*.tunnel.<domain>` staging wildcard cert covers it (a dotted
# multi-label host has no cert and fails TLS). Scope is the seeded cloud username.
$TunnelDomain = "web--$Username.tunnel.$Domain"
$ExpectedBody = "portzero-client-e2e-ok"
$PortzeroExe = "C:\Program Files\Port Zero\portzero.exe"
$AuthDir = Join-Path $env:USERPROFILE ".portzero"
$DaemonLog = Join-Path $AuthDir "daemon\daemon.log"

$WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) ("portzero-e2e-" + [Guid]::NewGuid().ToString("N"))
$HttpDir = Join-Path $WorkDir "http"
New-Item -ItemType Directory -Force -Path $WorkDir, $HttpDir | Out-Null

$script:HttpProcess = $null

function Test-Preflight {
    try {
        $resp = Invoke-WebRequest -Uri "https://app.$Domain/" -TimeoutSec 10 -UseBasicParsing
        $status = $resp.StatusCode
    } catch {
        $status = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
    }
    if ($status -ne 200) {
        Write-Host "::error::Staging is not up (HTTP $status) at https://app.$Domain/ — deploy staging in portzero-cloud before running the client interop test."
        exit 1
    }
    Write-Host "Staging is up (HTTP $status)."
}

function Install-Client {
    $installLog = Join-Path $WorkDir "msi-install.log"
    $proc = Start-Process -FilePath "msiexec.exe" `
        -ArgumentList @("/i", $MsiPath, "/quiet", "/qn", "/norestart", "/log", $installLog) `
        -Wait -PassThru
    if ($proc.ExitCode -ne 0) {
        Write-Host "::group::msiexec install log"
        if (Test-Path $installLog) { Get-Content $installLog | Write-Host }
        Write-Host "::endgroup::"
        throw "msiexec install failed with exit code $($proc.ExitCode)"
    }
    if (-not (Test-Path $PortzeroExe)) {
        throw "Expected portzero.exe not found at $PortzeroExe after install"
    }
}

function Add-SeedAuthCode {
    $body = @{
        email      = $Email
        username   = $Username
        account_id = $AccountId
        code       = $VerifyCode
    } | ConvertTo-Json -Compress
    Invoke-RestMethod -Method Post -Uri $SeedUrl `
        -Headers @{ Authorization = "Bearer $SeedToken" } `
        -ContentType "application/json" -Body $body | Out-Null
}

function Write-CliAuth {
    $body = @{ email = $Email; code = $VerifyCode } | ConvertTo-Json -Compress
    $resp = Invoke-RestMethod -Method Post -Uri $AuthUrl -ContentType "application/json" -Body $body

    New-Item -ItemType Directory -Force -Path $AuthDir | Out-Null
    $authJson = @{
        email      = $resp.email
        token      = $resp.token
        account_id = $resp.account_id
        username   = $resp.username
    } | ConvertTo-Json -Compress
    Set-Content -Path (Join-Path $AuthDir "auth.json") -Value $authJson -NoNewline
}

function Start-LocalService {
    Set-Content -Path (Join-Path $HttpDir "index.html") -Value $ExpectedBody -NoNewline
    $env:PZ_TUNNEL = "${TunnelDomain}:80"

    $stderrLog = Join-Path $WorkDir "http.log"
    $stdoutLog = Join-Path $WorkDir "http.stdout.log"
    # -u: unbuffered stdout/stderr. Without it, Python fully buffers stdout
    # when it isn't a TTY (as it isn't here, redirected to a file), so the
    # "Serving HTTP on ..." banner sits unflushed and this loop always times
    # out. The banner itself goes to stdout (a plain print(), no file=
    # argument) — stderr only gets per-request access logs — so it must be
    # checked in $stdoutLog, not $stderrLog.
    $script:HttpProcess = Start-Process -FilePath "python" `
        -ArgumentList @("-u", "-m", "http.server", "0", "--bind", "127.0.0.1") `
        -WorkingDirectory $HttpDir `
        -RedirectStandardOutput $stdoutLog `
        -RedirectStandardError $stderrLog `
        -PassThru -WindowStyle Hidden

    for ($i = 0; $i -lt 30; $i++) {
        if ((Test-Path $stdoutLog) -and (Select-String -Path $stdoutLog -Pattern "Serving HTTP" -Quiet)) {
            return
        }
        Start-Sleep -Seconds 1
    }

    Write-Host "::group::local http server log"
    if (Test-Path $stdoutLog) { Get-Content $stdoutLog | Write-Host }
    if (Test-Path $stderrLog) { Get-Content $stderrLog | Write-Host }
    Write-Host "::endgroup::"
    throw "Local HTTP server did not start"
}

function Start-Portzero {
    # PZ_TUNNEL was set session-wide so the python http.server child inherited
    # it. It must NOT leak into the portzero daemon's own environment: the
    # daemon has listening ports of its own (management API), so on Windows it
    # would discover itself as the service for the tunnel domain and register
    # the wrong local port. The bash sibling scopes PZ_TUNNEL to the python
    # subshell; do the equivalent here by clearing it before starting portzero.
    Remove-Item Env:\PZ_TUNNEL -ErrorAction SilentlyContinue

    $env:PZ_TUNNEL_API_URL = $ApiUrl
    $env:PZ_TUNNEL_EDGE_URL = $EdgeUrl
    $env:PZ_TUNNEL_BASE_DOMAIN = "tunnel.$Domain"
    & $PortzeroExe start --no-browser

    # $LASTEXITCODE after `portzero start` has proven unreliable on GitHub's
    # windows-latest runner: observed as $null even on runs where the daemon
    # printed its own success output and the daemon log showed a clean startup
    # and cloud edge connection. Verify functionally via `portzero status`
    # instead of trusting the native exit code.
    for ($i = 0; $i -lt 30; $i++) {
        $status = & $PortzeroExe status 2>&1 | Out-String
        if ($status -match "Daemon: running") {
            return
        }
        Start-Sleep -Seconds 1
    }

    throw "portzero daemon did not report running via 'portzero status'"
}

function Wait-ForRoute {
    $authJson = Get-Content (Join-Path $AuthDir "auth.json") -Raw | ConvertFrom-Json
    $headers = @{ Authorization = "Bearer $($authJson.token)" }

    for ($i = 1; $i -le 45; $i++) {
        try {
            $routes = Invoke-RestMethod -Uri "$ApiUrl/routes" -Headers $headers
            if ($routes | Where-Object { $_.domain -eq $TunnelDomain }) {
                Write-Host "Route registered: $TunnelDomain"
                return
            }
        } catch {
            # Route not registered yet, or transient network error — keep polling.
        }
        Write-Host "Waiting for route registration... ($i/45)"
        Start-Sleep -Seconds 2
    }

    throw "Route did not register: $TunnelDomain"
}

function Approve-Route {
    $authJson = Get-Content (Join-Path $AuthDir "auth.json") -Raw | ConvertFrom-Json
    $headers = @{ Authorization = "Bearer $($authJson.token)" }
    Invoke-RestMethod -Method Post -Uri "$ApiUrl/routes/$TunnelDomain/approve" -Headers $headers -ContentType "application/json" -Body "{}" | Out-Null
}

function Test-PublicTunnel {
    for ($i = 1; $i -le 30; $i++) {
        try {
            $resp = Invoke-WebRequest -Uri "https://$TunnelDomain/" -TimeoutSec 10 -UseBasicParsing
            if ($resp.Content.Trim() -eq $ExpectedBody) {
                Write-Host "Public tunnel returned expected response."
                return
            }
        } catch {
            # Not up yet — keep polling.
        }
        Write-Host "Waiting for public tunnel response... ($i/30)"
        Start-Sleep -Seconds 2
    }

    throw "Public tunnel did not return expected response from https://$TunnelDomain/"
}

try {
    Test-Preflight
    Install-Client
    & $PortzeroExe --version
    Add-SeedAuthCode
    Write-CliAuth

    $env:PZ_TUNNEL_API_URL = $ApiUrl
    & $PortzeroExe whoami

    Start-LocalService
    Start-Portzero
    Wait-ForRoute
    Approve-Route
    Test-PublicTunnel
}
finally {
    $env:PZ_TUNNEL_API_URL = $ApiUrl
    $env:PZ_TUNNEL_EDGE_URL = $EdgeUrl
    $env:PZ_TUNNEL_BASE_DOMAIN = "tunnel.$Domain"
    if (Test-Path $PortzeroExe) {
        try { & $PortzeroExe stop 2>$null | Out-Null } catch {}
    }
    if ($script:HttpProcess) {
        Stop-Process -Id $script:HttpProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path $DaemonLog) {
        Write-Host "::group::portzero daemon log"
        Get-Content $DaemonLog -Tail 200 | Write-Host
        Write-Host "::endgroup::"
    }
    Remove-Item -Recurse -Force $WorkDir -ErrorAction SilentlyContinue
}
