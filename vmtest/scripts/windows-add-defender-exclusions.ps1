#Requires -Version 5
# One-time guest PROVISIONING (Windows): add the Windows Defender exclusions that
# let an intentionally UNSIGNED portzero MSI install and launch in the VM.
#
# Why this is needed. In the VM E2E harness the MSI is unsigned (signing is
# release-only) and is installed straight off the `\\Mac\Home` network share, so
# the installed binary inherits Mark-of-the-Web. Defender's heuristic engine then
# blocks it from even starting:
#     Start-Process : ... the file contains a virus or potentially unwanted software.
# A path/process exclusion tells Defender to leave the install dir + the launched
# binary alone. (This is a TEST-VM convenience for unsigned pre-release artifacts;
# real users install a signed MSI that Defender trusts — nothing here ships.)
#
# Why it must be PROVISIONED (baked into the `built` checkpoint), not run inline
# in a flavor script: every `vmkit test` reset reverts the guest to `built`
# first, discarding anything a per-run script did. So this is applied ONCE via
# `vmkit provision ... --checkpoint built` and re-baked into `built`; every
# subsequent reset inherits the exclusions. See vmkit docs/PROVISIONING.md.
#
# Idempotent: an exclusion that is already present is left as-is (re-provisioning
# is safe). Emits greppable KEY=value / PHASE=<name> ok=<true|false|SKIP> lines
# and a final RESULT=PASS|FAIL|SKIP, like the other vmtest scripts. Runs as
# SYSTEM under `prlctl exec` (Add-MpPreference needs elevation, which SYSTEM has).
#
# Params / env (all optional — the defaults cover the reported failure):
#   -InstallDir <path>          install dir to exclude (default: "%ProgramFiles%\Port Zero")
#   -Path <path[]>              extra paths to exclude
#   -Process <name[]>           extra process names to exclude (default adds portzero.exe)
#   $env:PORTZERO_DEFENDER_PATHS ';'-separated extra paths (for env-driven callers)
[CmdletBinding()]
param(
    [string]   $InstallDir = (Join-Path ${env:ProgramFiles} 'Port Zero'),
    [string[]] $Path = @(),
    [string[]] $Process = @()
)
$ErrorActionPreference = 'Continue'

$script:fails = 0
function Phase {
    param([string] $Name, [bool] $Ok)
    if ($Ok) { "PHASE=$Name ok=true" } else { "PHASE=$Name ok=false"; $script:fails++ }
}

"whoami=" + (whoami)

# --- Defender availability --------------------------------------------------
# If the Defender PowerShell module isn't present (Defender removed/replaced by
# another AV), there is nothing to exclude — SKIP cleanly rather than fail.
if (-not (Get-Command Add-MpPreference -ErrorAction SilentlyContinue)) {
    "defender_cmdlets=MISSING"
    "RESULT=SKIP reason=`"Add-MpPreference unavailable (Windows Defender module not present)`""
    exit 0
}

$mp = Get-MpPreference -ErrorAction SilentlyContinue
if (-not $mp) {
    "defender_prefs=UNAVAILABLE"
    "RESULT=SKIP reason=`"Get-MpPreference returned nothing (Defender service disabled?)`""
    exit 0
}

# --- assemble the exclusion sets --------------------------------------------
# Paths: the install dir where the launched binary lands, the repo's artifact
# drop dir on the share (the MSI/exe are read from there), plus any extras.
# $PSScriptRoot is the guest-visible UNC path to vmtest\scripts (set because
# vmkit runs us via `powershell -File <unc>`), so the artifact dir resolves
# without knowing the host's $HOME. Guarded in case it is ever empty.
$artifactDir = $null
if ($PSScriptRoot) {
    $artifactDir = Join-Path (Split-Path $PSScriptRoot -Parent) '.downloaded-artifacts'
}
$envPaths = @()
if ($env:PORTZERO_DEFENDER_PATHS) {
    $envPaths = $env:PORTZERO_DEFENDER_PATHS -split ';' | Where-Object { $_ -ne '' }
}
$wantPaths = @($InstallDir, $artifactDir) + $envPaths + $Path |
    Where-Object { $_ -and $_.Trim() -ne '' } |
    Select-Object -Unique
$wantProcs = @('portzero.exe') + $Process |
    Where-Object { $_ -and $_.Trim() -ne '' } |
    Select-Object -Unique

foreach ($p in $wantPaths)  { "want_path=$p" }
foreach ($n in $wantProcs)  { "want_process=$n" }

# Case-insensitive membership against what Defender already has.
function Has-Item {
    param([string[]] $Have, [string] $Want)
    if (-not $Have) { return $false }
    foreach ($h in $Have) { if ($h -and ($h.TrimEnd('\') -ieq $Want.TrimEnd('\'))) { return $true } }
    return $false
}

# --- apply path exclusions --------------------------------------------------
$pathOk = $true
foreach ($p in $wantPaths) {
    if (Has-Item -Have $mp.ExclusionPath -Want $p) {
        "path_add=SKIP-already-present path=`"$p`""
        continue
    }
    try {
        Add-MpPreference -ExclusionPath $p -ErrorAction Stop
        "path_add=OK path=`"$p`""
    } catch {
        "path_add=FAIL path=`"$p`" error=`"$($_.Exception.Message)`""
        $pathOk = $false
    }
}
Phase 'exclusion-paths' $pathOk

# --- apply process exclusions -----------------------------------------------
$procOk = $true
foreach ($n in $wantProcs) {
    if (Has-Item -Have $mp.ExclusionProcess -Want $n) {
        "process_add=SKIP-already-present process=`"$n`""
        continue
    }
    try {
        Add-MpPreference -ExclusionProcess $n -ErrorAction Stop
        "process_add=OK process=`"$n`""
    } catch {
        "process_add=FAIL process=`"$n`" error=`"$($_.Exception.Message)`""
        $procOk = $false
    }
}
Phase 'exclusion-processes' $procOk

# --- verify (re-read; catches policy/tamper-protection silent no-ops) --------
$after = Get-MpPreference -ErrorAction SilentlyContinue
$verifyOk = $true
foreach ($p in $wantPaths) {
    if (Has-Item -Have $after.ExclusionPath -Want $p) { "verify_path=PRESENT path=`"$p`"" }
    else { "verify_path=ABSENT path=`"$p`""; $verifyOk = $false }
}
foreach ($n in $wantProcs) {
    if (Has-Item -Have $after.ExclusionProcess -Want $n) { "verify_process=PRESENT process=`"$n`"" }
    else { "verify_process=ABSENT process=`"$n`""; $verifyOk = $false }
}
Phase 'verify' $verifyOk
if (-not $verifyOk) {
    "hint=`"An exclusion did not stick. Common cause: Tamper Protection is ON, or Defender is managed by policy/GPO. Turn Tamper Protection off in the golden image (Windows Security > Virus & threat protection settings) and re-provision.`""
}

# --- result -----------------------------------------------------------------
if ($script:fails -eq 0) {
    "RESULT=PASS defender exclusions in place"
    exit 0
} else {
    "RESULT=FAIL assertions failed=$($script:fails)"
    exit 1
}
