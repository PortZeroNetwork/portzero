#Requires -Version 5
# AUTO-UPDATE E2E (Windows): prove the real `portzero update` self-updater works
# end to end against a throwaway local release mirror — the thing that, if it
# ever breaks, silently strands every installed user's ability to update.
#
# `portzero update` resolves its download base from
# portzero_domain::endpoints::update_download_base, overridable with
# PZ_TUNNEL_UPDATE_BASE_URL. We point it at a local directory holding a
# hand-built version.json (the exact {"version","commit"} shape release.yml
# publishes) and a portzero-windows-<arch>.zip archive, then drive the real
# command and assert it detects, downloads, Expand-Archives, and swaps the
# running .exe in place (rename-aside + copy) — no network, no second build.
#
# Mirrors autoupdate-unix.sh. Because a running .exe can't be overwritten, the
# updater renames it aside as `portzero.old`; the swap assertions key off that
# and off byte-provenance instead of the inode change the unix side uses.
#
# Phases (greppable: `PHASE=<name> ... ok=<true|false>`, then RESULT=PASS|FAIL):
#   detect    `update --check` reports an available update against a bumped manifest
#   apply     `update --force` downloads+unpacks+swaps; result runs and matches the archive
#   noop      `update` against a same-version manifest reports up-to-date and swaps nothing
#   crossver  (gated) a PRIOR binary self-updates to the new build and its --version advances
#
# Env:
#   PORTZERO_EXE      new (under-test) portzero.exe. If unset, searches
#                     vmtest\.downloaded-artifacts\windows\portzero.exe.
#   PORTZERO_EXE_OLD  optional prior-version portzero.exe. crossver runs only if
#                     set AND it already understands `update` (releases predating
#                     this feature can't self-update — the forward-compat contract
#                     this test guards); SKIPs cleanly otherwise.
$ErrorActionPreference = 'Continue'
$script:fails = 0

function Phase {
    param([string] $Name, [bool] $Ok)
    if ($Ok) { "PHASE=$Name ok=true" } else { "PHASE=$Name ok=false"; $script:fails++ }
}

function Get-Version {
    param([string] $Exe)
    try { $o = (& $Exe --version 2>$null | Out-String).Trim(); return ($o -split '\s+')[-1] }
    catch { return '' }
}
function Get-Sha { param([string] $Path) return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash }

function Get-Target {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        'AMD64' { return 'windows-amd64' }
        'ARM64' { return 'windows-arm64' }
        default { return "windows-$($env:PROCESSOR_ARCHITECTURE.ToLower())" }
    }
}

# Build a release mirror dir: version.json (given version) + a .zip whose single
# binary is $Bin. Returns the mirror path.
function Build-Mirror {
    param([string] $Bin, [string] $Version, [string] $Target)
    $mirror = Join-Path $env:TEMP ("pz-mirror-" + [guid]::NewGuid().ToString('N'))
    $stage = Join-Path $mirror "stage\portzero-$Target"
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item -LiteralPath $Bin -Destination (Join-Path $stage 'portzero.exe')
    Compress-Archive -Path (Join-Path $mirror "stage\portzero-$Target") `
        -DestinationPath (Join-Path $mirror "portzero-$Target.zip") -Force
    Remove-Item -Recurse -Force (Join-Path $mirror 'stage')
    # Same shape release.yml emits — binds this test to the manifest format.
    Set-Content -LiteralPath (Join-Path $mirror 'version.json') -NoNewline `
        -Value ('{"version":"' + $Version + '","commit":"autoupdate-test"}')
    return $mirror
}

function Find-NewExe {
    if ($env:PORTZERO_EXE -and (Test-Path $env:PORTZERO_EXE)) { return $env:PORTZERO_EXE }
    $repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $c = Get-ChildItem -Path (Join-Path $repo 'vmtest\.downloaded-artifacts\windows') `
        -Filter 'portzero.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($c) { return $c.FullName }
    return $null
}

# --- preflight -------------------------------------------------------------
$exe = Find-NewExe
$target = Get-Target
"PHASE=preflight exe=$(if ($exe) { $exe } else { 'none' }) target=$target"
if (-not $exe -or -not (Test-Path $exe)) {
    "RESULT=FAIL no portzero.exe found (set PORTZERO_EXE or drop one under vmtest\.downloaded-artifacts\windows\)"
    exit 1
}
# Copy to a local, unblocked path and run EVERYTHING from there. Never execute
# the binary straight off the \\Mac\Home share: a UNC path plus Mark-of-the-Web
# make direct execution unreliable (it can produce no output and a non-zero
# exit), which would look like a missing subcommand rather than what it is.
# Reading/copying the share file is fine — only executing it is the problem.
$installDir = Join-Path $env:TEMP ("pz-autoupdate-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$P = Join-Path $installDir 'portzero.exe'
Copy-Item -LiteralPath $exe -Destination $P
Unblock-File -LiteralPath $P

# Detect `update` support by EXIT CODE (clap exits 0 for a known subcommand's
# --help, non-zero for an unrecognized one), not by captured output.
& $P update --help *> $null
if ($LASTEXITCODE -ne 0) {
    "RESULT=FAIL the under-test binary has no ``update`` subcommand (``update --help`` exit $LASTEXITCODE)"
    exit 1
}

# Hermetic: no privileged setup side effects; silence the background notifier.
$env:PZ_TUNNEL_NO_UPDATE_CHECK = '1'
$env:PZ_TUNNEL_UPDATE_SKIP_SETUP = '1'

# --- detect ----------------------------------------------------------------
# Build the mirror from the local copy so the archive binary and the swapped-in
# binary are byte-identical (the provenance assert), and no share exec occurs.
$bumpMirror = Build-Mirror -Bin $P -Version '99.0.0' -Target $target
$archiveSha = Get-Sha -Path $P
$env:PZ_TUNNEL_UPDATE_BASE_URL = $bumpMirror
$checkOut = (& $P update --check 2>&1 | Out-String)
Phase 'detect-reports-available' ($checkOut -match '(?i)update is available')

# --- apply -----------------------------------------------------------------
$backup = [System.IO.Path]::ChangeExtension($P, 'old')
if (Test-Path $backup) { Remove-Item -Force $backup -ErrorAction SilentlyContinue }
& $P update --force *> (Join-Path $env:TEMP 'pz-autoupdate-apply.log')
Phase 'apply-exit-zero' ($LASTEXITCODE -eq 0)
Phase 'apply-binary-runs' ((Get-Version -Exe $P) -ne '')
# The running .exe is renamed aside as portzero.old before the new one lands.
Phase 'apply-swapped-binary' (Test-Path $backup)
Phase 'apply-provenance' ((Get-Sha -Path $P) -eq $archiveSha)

# --- noop (up-to-date) -----------------------------------------------------
$curVer = (Get-Version -Exe $P) -replace '^v', ''
$sameMirror = Build-Mirror -Bin $P -Version $curVer -Target $target
$env:PZ_TUNNEL_UPDATE_BASE_URL = $sameMirror
$writeBefore = (Get-Item -LiteralPath $P).LastWriteTimeUtc
$noopOut = (& $P update 2>&1 | Out-String)
Phase 'noop-reports-up-to-date' ($noopOut -match '(?i)already up to date')
Phase 'noop-no-swap' ((Get-Item -LiteralPath $P).LastWriteTimeUtc -eq $writeBefore)

# --- crossver (gated real version advance) ---------------------------------
$old = $env:PORTZERO_EXE_OLD
if (-not $old -or -not (Test-Path $old)) {
    "PHASE=crossver skipped=no-prior-binary (set PORTZERO_EXE_OLD to exercise a real version advance)"
} else {
    # Copy + unblock the prior binary too, then probe support by exit code —
    # same reason as the new binary above (no direct share execution).
    $oldDir = Join-Path $env:TEMP ("pz-autoupdate-old-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $oldDir | Out-Null
    $pOld = Join-Path $oldDir 'portzero.exe'
    Copy-Item -LiteralPath $old -Destination $pOld
    Unblock-File -LiteralPath $pOld
    & $pOld update --help *> $null
    if ($LASTEXITCODE -ne 0) {
        "PHASE=crossver skipped=prior-has-no-update (predates the self-updater; forward-compat contract not yet exercisable)"
    } else {
        $newVer = (Get-Version -Exe $P) -replace '^v', ''
        $env:PZ_TUNNEL_UPDATE_BASE_URL = (Build-Mirror -Bin $P -Version $newVer -Target $target)
        & $pOld update --force *> (Join-Path $env:TEMP 'pz-autoupdate-crossver.log')
        Phase 'crossver-advanced' (((Get-Version -Exe $pOld) -replace '^v', '') -eq $newVer)
    }
}

if ($script:fails -eq 0) {
    "RESULT=PASS autoupdate (detect -> apply -> up-to-date no-op$(if ($old) { ' -> crossver' }))"
    exit 0
}
"RESULT=FAIL autoupdate assertions failed=$($script:fails)"
exit 1
