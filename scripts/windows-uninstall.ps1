Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Write-Step {
    param([string] $Message)
    Write-Host "-> $Message"
}

function Get-CargoBinDir {
    if ($env:CARGO_HOME) {
        return (Join-Path $env:CARGO_HOME "bin")
    }

    $profileDir = $env:USERPROFILE
    if (-not $profileDir) {
        $profileDir = [Environment]::GetFolderPath("UserProfile")
    }

    return (Join-Path $profileDir ".cargo\bin")
}

function Get-PortzeroCandidates {
    param([string] $CargoExe)

    $candidates = @()
    if (Test-Path $CargoExe) {
        $candidates += $CargoExe
    }

    $cmd = Get-Command "portzero.exe" -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source) {
        $candidates += $cmd.Source
    }

    return @($candidates | Select-Object -Unique)
}

function Invoke-PortzeroBestEffort {
    param(
        [string[]] $Candidates,
        [string[]] $Arguments
    )

    foreach ($candidate in $Candidates) {
        if (-not (Test-Path $candidate)) {
            continue
        }

        $previousErrorActionPreference = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        try {
            & $candidate @Arguments 2>$null
            if ($LASTEXITCODE -eq 0) {
                return $true
            }
        }
        finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
    }

    return $false
}

function Remove-FileBestEffort {
    param([string] $Path)

    if (Test-Path $Path) {
        Write-Step "Removing $Path"
        Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    }
}

if ($env:OS -ne "Windows_NT") {
    Write-Error "scripts/windows-uninstall.ps1 can only be run on Windows."
    exit 1
}

$cargoBin = Get-CargoBinDir
$cargoPortzeroExe = Join-Path $cargoBin "portzero.exe"
$candidates = Get-PortzeroCandidates -CargoExe $cargoPortzeroExe

Write-Step "Stopping the tray companion and removing its autostart shortcut"
Get-Process -Name "portzero-tray" -ErrorAction SilentlyContinue |
    Stop-Process -Force -ErrorAction SilentlyContinue
$startup = [Environment]::GetFolderPath("Startup")
if ($startup) {
    Remove-FileBestEffort -Path (Join-Path $startup "PortZero Tray.lnk")
}

Write-Step "Stopping daemon and removing autostart service"
$null = Invoke-PortzeroBestEffort -Candidates $candidates -Arguments @("autostart", "disable")
$null = Invoke-PortzeroBestEffort -Candidates $candidates -Arguments @("stop")

Write-Step "Removing local CA certificate from Windows trust stores"
$null = Invoke-PortzeroBestEffort -Candidates $candidates -Arguments @("trust", "uninstall")

$cargo = Get-Command "cargo.exe" -ErrorAction SilentlyContinue
if ($cargo) {
    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        Write-Step "Uninstalling portzero-cli with cargo"
        & $cargo.Source uninstall portzero-cli 2>$null
        Write-Step "Uninstalling portzero-tray with cargo"
        & $cargo.Source uninstall portzero-tray 2>$null
    }
    finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
}

foreach ($candidate in $candidates) {
    $candidateDir = Split-Path -Parent $candidate
    Remove-FileBestEffort -Path $candidate
    Remove-FileBestEffort -Path (Join-Path $candidateDir "wintun.dll")
    Remove-FileBestEffort -Path (Join-Path $candidateDir "portzero-tray.exe")
}

Write-Host "portzero uninstalled."
