Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$WintunVersion = "0.14.1"
$WintunUrl = "https://www.wintun.net/builds/wintun-$WintunVersion.zip"

function Write-Step {
    param([string] $Message)
    Write-Host "-> $Message"
}

function Fail {
    param([string] $Message)
    Write-Error $Message
    exit 1
}

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-CargoBinDir {
    if ($env:CARGO_HOME) {
        return (Join-Path $env:CARGO_HOME "bin")
    }

    $profileDir = $env:USERPROFILE
    if (-not $profileDir) {
        $profileDir = [Environment]::GetFolderPath("UserProfile")
    }
    if (-not $profileDir) {
        Fail "Could not determine the current user's profile directory."
    }

    return (Join-Path $profileDir ".cargo\bin")
}

function Add-UserPath {
    param([string] $Directory)

    $resolved = [IO.Path]::GetFullPath($Directory).TrimEnd('\')
    $currentUserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @()
    if ($currentUserPath) {
        $parts = $currentUserPath -split ';' | Where-Object { $_ }
    }

    $alreadyPresent = $false
    foreach ($part in $parts) {
        if ([string]::Equals($part.TrimEnd('\'), $resolved, [StringComparison]::OrdinalIgnoreCase)) {
            $alreadyPresent = $true
            break
        }
    }

    if (-not $alreadyPresent) {
        $newPath = @($parts + $resolved) -join ';'
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Step "Added $resolved to the user PATH"
    }

    $processParts = @()
    if ($env:Path) {
        $processParts = $env:Path -split ';' | Where-Object { $_ }
    }
    $processHasPath = $false
    foreach ($part in $processParts) {
        if ([string]::Equals($part.TrimEnd('\'), $resolved, [StringComparison]::OrdinalIgnoreCase)) {
            $processHasPath = $true
            break
        }
    }
    if (-not $processHasPath) {
        $env:Path = "$resolved;$env:Path"
    }
}

function Stop-ExistingDaemon {
    param([string] $CargoExe)

    $candidates = @()
    if (Test-Path $CargoExe) {
        $candidates += $CargoExe
    }
    $cmd = Get-Command "portzero.exe" -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source) {
        $candidates += $cmd.Source
    }

    foreach ($candidate in ($candidates | Select-Object -Unique)) {
        Write-Step "Stopping existing daemon with $candidate"
        $exitCode = 1
        $previousErrorActionPreference = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        try {
            & $candidate stop 2>$null
            $exitCode = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
        if ($exitCode -eq 0) {
            return
        }
    }
}

function Get-ActivePortzeroPath {
    $cmd = Get-Command "portzero.exe" -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source) {
        return $cmd.Source
    }
    return $null
}

function Install-Wintun {
    param([string] $InstallDir)

    $destination = Join-Path $InstallDir "wintun.dll"
    if (Test-Path $destination) {
        Write-Step "wintun.dll already present at $destination"
        return
    }

    $arch = switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { "amd64" }
        "ARM64" { "arm64" }
        "x86" { "x86" }
        default { Fail "Unsupported Windows architecture: $env:PROCESSOR_ARCHITECTURE" }
    }

    $workDir = Join-Path ([IO.Path]::GetTempPath()) ("portzero-wintun-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $workDir | Out-Null
    try {
        $zip = Join-Path $workDir "wintun.zip"
        Write-Step "Downloading Wintun $WintunVersion"
        Invoke-WebRequest -Uri $WintunUrl -OutFile $zip

        Expand-Archive -Path $zip -DestinationPath $workDir -Force
        $dll = Get-ChildItem -Path $workDir -Recurse -Filter "wintun.dll" |
            Where-Object { $_.FullName -match "\\$arch\\" } |
            Select-Object -First 1
        if (-not $dll) {
            Fail "Downloaded Wintun archive did not contain a $arch wintun.dll."
        }

        $signature = Get-AuthenticodeSignature -LiteralPath $dll.FullName
        if ($signature.Status -ne "Valid") {
            Fail "Downloaded wintun.dll does not have a valid Authenticode signature. Status: $($signature.Status)"
        }
        if ($signature.SignerCertificate.Subject -notmatch "WireGuard") {
            Fail "Downloaded wintun.dll was signed by an unexpected publisher: $($signature.SignerCertificate.Subject)"
        }

        Copy-Item -LiteralPath $dll.FullName -Destination $destination -Force
        Write-Step "Installed Wintun to $destination"
    }
    finally {
        Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Sync-ActiveInstall {
    param(
        [string] $CargoExe,
        [string] $ActiveExe
    )

    if (-not $ActiveExe) {
        return
    }

    $cargoPath = [IO.Path]::GetFullPath($CargoExe)
    $activePath = [IO.Path]::GetFullPath($ActiveExe)
    if ([string]::Equals($cargoPath, $activePath, [StringComparison]::OrdinalIgnoreCase)) {
        return
    }

    $activeDir = Split-Path -Parent $activePath
    Write-Step "Updating active portzero on PATH at $activePath"
    Copy-Item -LiteralPath $cargoPath -Destination $activePath -Force

    $cargoWintun = Join-Path (Split-Path -Parent $cargoPath) "wintun.dll"
    if (Test-Path $cargoWintun) {
        Copy-Item -LiteralPath $cargoWintun -Destination (Join-Path $activeDir "wintun.dll") -Force
    }
}

function Install-Tray {
    param(
        [string] $Cargo,
        [string] $CargoBin
    )

    # System-tray companion: a small GUI showing daemon/tunnel health with
    # start/restart/stop controls. Best-effort — a tray build or autostart
    # failure never aborts the daemon install.
    Write-Step "Building and installing the system-tray companion (portzero-tray)"
    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        & $Cargo install --path "client/crates/tray"
        $trayBuilt = ($LASTEXITCODE -eq 0)
    }
    finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    if (-not $trayBuilt) {
        Write-Warning "portzero-tray build failed; continuing without the tray."
        return
    }

    $trayExe = Join-Path $CargoBin "portzero-tray.exe"
    if (-not (Test-Path $trayExe)) {
        Write-Warning "cargo completed but $trayExe was not created; skipping tray autostart."
        return
    }

    # Autostart at login via a shortcut in the user's Startup folder.
    try {
        $startup = [Environment]::GetFolderPath("Startup")
        if ($startup) {
            $shortcut = Join-Path $startup "PortZero Tray.lnk"
            $wshell = New-Object -ComObject WScript.Shell
            $link = $wshell.CreateShortcut($shortcut)
            $link.TargetPath = $trayExe
            $link.Description = "PortZero daemon status and controls"
            $link.Save()
            Write-Step "Installed tray autostart shortcut at $shortcut"
        }
    }
    catch {
        Write-Warning "Could not create the tray autostart shortcut: $($_.Exception.Message)"
    }

    try {
        Start-Process -FilePath $trayExe | Out-Null
        Write-Step "Launched portzero-tray"
    }
    catch {
        Write-Warning "Could not launch portzero-tray; it will start at your next login."
    }
}

function Open-Browser {
    param([string] $Url)

    try {
        Start-Process -FilePath $Url | Out-Null
        return $true
    }
    catch {
        Write-Warning "Could not open $Url automatically."
        return $false
    }
}

function Wait-Dashboard {
    # Poll the dashboard over its real DNS path so we only pop the browser once
    # it will actually load — opening early would just show an error page.
    Write-Host "Waiting for http://portzero.local..."
    for ($i = 0; $i -lt 30; $i++) {
        try {
            $resp = Invoke-WebRequest -Uri "http://portzero.local/status.json" `
                -UseBasicParsing -TimeoutSec 2 -ErrorAction Stop
            if ($resp.StatusCode -eq 200) { return $true }
        }
        catch { }
        Start-Sleep -Seconds 1
    }
    return $false
}

if ($env:OS -ne "Windows_NT") {
    Fail "scripts/windows-install.ps1 can only be run on Windows."
}

if (-not (Test-Administrator)) {
    Fail "Run 'just install' from an Administrator PowerShell or Windows Terminal. Port Zero needs elevation to create the scheduled task, configure NRPT DNS, and create the Wintun adapter."
}

$cargo = Get-Command "cargo.exe" -ErrorAction SilentlyContinue
if (-not $cargo) {
    Fail "cargo.exe was not found on PATH. Install Rust from https://rustup.rs/, open a new terminal, and run 'just install' again."
}

$cargoBin = Get-CargoBinDir
New-Item -ItemType Directory -Path $cargoBin -Force | Out-Null
$portzeroExe = Join-Path $cargoBin "portzero.exe"
$activePortzeroExe = Get-ActivePortzeroPath

Stop-ExistingDaemon -CargoExe $portzeroExe

Write-Step "Building and installing portzero with cargo"
& $cargo.Source install --path "client/crates/cli"
if ($LASTEXITCODE -ne 0) {
    Fail "cargo install failed."
}

if (-not (Test-Path $portzeroExe)) {
    Fail "cargo completed but $portzeroExe was not created."
}

Add-UserPath -Directory $cargoBin
Install-Wintun -InstallDir $cargoBin
Sync-ActiveInstall -CargoExe $portzeroExe -ActiveExe $activePortzeroExe

Write-Step "Generating local CA certificate"
& $portzeroExe trust generate
if ($LASTEXITCODE -ne 0) {
    Fail "portzero trust generate failed."
}

Write-Step "Installing local CA certificate into Windows trust stores"
& $portzeroExe trust install
if ($LASTEXITCODE -ne 0) {
    Fail "portzero trust install failed."
}

Write-Step "Installing scheduled task for autostart"
& $portzeroExe autostart enable
if ($LASTEXITCODE -ne 0) {
    Fail "portzero autostart enable failed."
}

Write-Step "Starting daemon"
& $portzeroExe start --no-browser
if ($LASTEXITCODE -ne 0) {
    Fail "portzero start failed."
}

Install-Tray -Cargo $cargo.Source -CargoBin $cargoBin

Write-Host ""
if (Wait-Dashboard) {
    if (Open-Browser -Url "http://portzero.local") {
        Write-Host "portzero installed and should now be open in your browser."
        Write-Host "Run an example from the Getting Started section on the dashboard."
    } else {
        Write-Host "portzero installed. Open http://portzero.local in your browser and run an example from Getting Started."
    }
} else {
    Write-Host "portzero installed. Open http://portzero.local once it is reachable (see 'portzero status') and run an example from Getting Started."
}
Write-Host "If this terminal was already open, PATH has been updated for this process; new terminals will also find portzero.exe."

Write-Host ""
Write-Host "Local tunnels (*.portzero.local) governed by the GNU General Public License v3.0:"
Write-Host "  https://github.com/PortZeroNetwork/portzero-local/blob/staging/LICENSE"
Write-Host "Cloud features governed by https://portzero.net/terms"
