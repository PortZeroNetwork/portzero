#Requires -Version 5
# Build portzero.exe inside the Windows VM, fully OFFLINE, from vendored crates.
# Copies the repo source and the vendor dir onto C: (fast local disk; also avoids
# building over SMB and the host's root-owned target/). Idempotent-ish: re-syncs
# source each run so edits on the host are picked up. Prints KEY=value lines.
$ErrorActionPreference = 'Stop'
$repoUnc   = '\\Mac\Home\Documents\src\PortZeroNetwork\portzero'
$vendorUnc = '\\Mac\MBP-Sidecar\loumtech\vm-toolchain-cache\common\vendor'
$src       = 'C:\src\portzero'
$vendor    = 'C:\vendor'
$cargoHome = 'C:\cargo-home'

# Resolve cargo (PATH may be stale in this process).
$cargo = (Get-Command cargo -EA SilentlyContinue).Source
if (-not $cargo) { $cargo = (Get-ChildItem 'C:\Program Files\Rust*\bin\cargo.exe' -EA SilentlyContinue | Select-Object -First 1).FullName }
if (-not $cargo) { throw "cargo not found; run win-install-toolchain.ps1 first" }
"cargo=$cargo"

# --- sync source (exclude build/vendor/vcs junk) via robocopy ---
"sync=source"
$null = New-Item -ItemType Directory -Force -Path $src
robocopy $repoUnc $src /MIR /XD target .git vendor .ticketry node_modules /XF *.log /NFL /NDL /NJH /NJS /NP /R:1 /W:1 | Out-Null
# robocopy exit codes 0-7 are success; >=8 is failure.
if ($LASTEXITCODE -ge 8) { throw "robocopy source failed ($LASTEXITCODE)" }

# --- one-time vendor copy to local disk (841 MB; skip if already present) ---
if (-not (Test-Path (Join-Path $vendor 'anstyle'))) {
    "sync=vendor(first-time)"
    $null = New-Item -ItemType Directory -Force -Path $vendor
    robocopy $vendorUnc $vendor /MIR /NFL /NDL /NJH /NJS /NP /R:1 /W:1 | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy vendor failed ($LASTEXITCODE)" }
} else {
    "sync=vendor(cached)"
}

# --- offline cargo config: replace crates.io with the local vendor dir ---
$null = New-Item -ItemType Directory -Force -Path $cargoHome
@"
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "$($vendor -replace '\\','\\')"

[net]
offline = true
"@ | Set-Content -Path (Join-Path $cargoHome 'config.toml') -Encoding ASCII

$env:CARGO_HOME = $cargoHome
$env:CARGO_TARGET_DIR = 'C:\pz-target'
$env:RUSTUP_TOOLCHAIN = ''  # ignore any rustup indirection; MSI cargo is direct

"build=start $(Get-Date -Format o)"
Push-Location $src
try {
    & $cargo build --release --offline --target x86_64-pc-windows-msvc --bin portzero
    $code = $LASTEXITCODE
} finally { Pop-Location }
"build_exit=$code"
if ($code -ne 0) { throw "cargo build failed ($code)" }

$exe = 'C:\pz-target\x86_64-pc-windows-msvc\release\portzero.exe'
"exe=$exe exists=$(Test-Path $exe)"

# The overlay needs wintun.dll next to the exe. It ships inside the vendored
# wintun crate, so no separate download — copy the amd64 build in.
$wintun = Join-Path $vendor 'wintun-0.3.2\wintun\bin\amd64\wintun.dll'
if (Test-Path $wintun) {
    Copy-Item $wintun (Split-Path $exe) -Force
    "wintun=placed"
} else {
    "wintun=NOT-FOUND ($wintun)"
}
if (Test-Path $exe) { "portzero_version=$(& $exe --version 2>&1)" }
