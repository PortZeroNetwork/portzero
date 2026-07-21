#Requires -Version 5
# Install the Windows build toolchain from the OFFLINE cache (no network):
# MSVC Build Tools from the vs_layout, and Rust from the standalone MSI.
# Idempotent — skips whatever is already present. Prints KEY=value lines.
$ErrorActionPreference = 'Stop'
$cache = '\\Mac\MBP-Sidecar\loumtech\vm-toolchain-cache\windows'

function Have-Cl {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $false }
    $p = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
    return [bool]$p
}

# --- MSVC Build Tools (VC++ x64 + Windows 11 SDK) from the offline layout ---
if (Have-Cl) {
    "msvc=already-installed"
} else {
    "msvc=installing-from-layout"
    $boot = Join-Path $cache 'vs_layout\vs_setup.exe'
    if (-not (Test-Path $boot)) { $boot = Join-Path $cache 'vs_layout\vs_BuildTools.exe' }
    $args = @(
        '--noWeb','--quiet','--wait','--norestart',
        '--add','Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
        '--add','Microsoft.VisualStudio.Component.Windows11SDK.22621'
    )
    $p = Start-Process -FilePath $boot -ArgumentList $args -Wait -PassThru
    "msvc_exit=$($p.ExitCode)"
    if (-not (Have-Cl)) { throw "MSVC install did not register VC.Tools" }
    "msvc=installed"
}

# --- Rust (standalone MSI, machine-wide) ---
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    # PATH may not be refreshed in this process; check the MSI's usual location.
    $guess = Get-ChildItem 'C:\Program Files\Rust*\bin\cargo.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($guess) { $cargo = $guess }
}
if ($cargo) {
    "rust=already-installed"
} else {
    "rust=installing-from-msi"
    $msi = Join-Path $cache 'rust-1.97.0-x86_64-pc-windows-msvc.msi'
    $log = 'C:\Windows\Temp\rust-msi.log'
    $p = Start-Process msiexec.exe -ArgumentList @('/i',"`"$msi`"",'/quiet','/norestart','/log',$log) -Wait -PassThru
    "rust_exit=$($p.ExitCode)"
    if ($p.ExitCode -ne 0) { throw "Rust MSI failed ($($p.ExitCode)); see $log" }
    "rust=installed"
}

# Resolve concrete tool paths for the caller (PATH may be stale in-process).
$cargoExe = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargoExe) { $cargoExe = (Get-ChildItem 'C:\Program Files\Rust*\bin\cargo.exe' -EA SilentlyContinue | Select-Object -First 1).FullName }
"cargo_path=$cargoExe"
if ($cargoExe) { "cargo_version=$(& $cargoExe --version 2>&1)" }
