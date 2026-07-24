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
# updater renames it aside as `portzero.old` and copies the new build into
# place. The swap assertion keys off the running binary's NTFS file identity
# changing (volume serial + file index — the Windows analog of the inode the
# unix side compares), which a rename-aside-then-copy always changes. It does
# NOT key off the moved-aside `portzero.old` lingering: that backup is a
# best-effort rollback net the OS only unlocks after the updater process exits,
# and asserting its post-exit survival flaked (see git history of this file).
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

# NTFS file identity (volume serial + 64-bit file index) — the Windows analog of
# a Unix inode. Distinct files have distinct identities, so it detects that the
# running .exe was replaced by a freshly-created file even when the bytes match.
# Returns $null if the file can't be opened.
if (-not ([System.Management.Automation.PSTypeName]'PZ.Fs').Type) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
namespace PZ {
  [StructLayout(LayoutKind.Sequential)]
  public struct ByHandleFileInformation {
    public uint FileAttributes;
    public long CreationTime;
    public long LastAccessTime;
    public long LastWriteTime;
    public uint VolumeSerialNumber;
    public uint FileSizeHigh;
    public uint FileSizeLow;
    public uint NumberOfLinks;
    public uint FileIndexHigh;
    public uint FileIndexLow;
  }
  public static class Fs {
    [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share,
      IntPtr sec, uint disp, uint flags, IntPtr templ);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool GetFileInformationByHandle(SafeFileHandle h, out ByHandleFileInformation info);
    public static string FileId(string path) {
      // FILE_READ_ATTRIBUTES=0x80, FILE_SHARE_READ|WRITE|DELETE=7,
      // OPEN_EXISTING=3, FILE_FLAG_BACKUP_SEMANTICS=0x02000000.
      using (var h = CreateFileW(path, 0x80, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero)) {
        if (h.IsInvalid) return null;
        ByHandleFileInformation info;
        if (!GetFileInformationByHandle(h, out info)) return null;
        return info.VolumeSerialNumber.ToString("x8") + ":" +
               info.FileIndexHigh.ToString("x8") + info.FileIndexLow.ToString("x8");
      }
    }
  }
}
"@
}
function Get-FileId { param([string] $Path) return [PZ.Fs]::FileId($Path) }

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

# Point %TEMP%/%TMP% at a fresh Defender-EXCLUDED root for this run, so every
# unsigned portzero.exe we write — our staged copies, the mirror archives, AND
# the Rust updater's own extraction dir (std::env::temp_dir() honors TMP/TEMP) —
# lands in excluded space. Defender otherwise blocks a freshly-written unsigned
# exe from *starting*, or quarantines the unpacked exe mid-update; the baked
# `portzero.exe` process-name exclusion is not enough, and combined only works
# because it runs from the excluded Program Files install dir. SYSTEM (the prlctl
# exec context) can set the exclusion; it is discarded on the next reset.
function Enable-ExcludedTemp {
    $root = Join-Path $env:TEMP ("pz-autoupdate-root-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    if (Get-Command Add-MpPreference -ErrorAction SilentlyContinue) {
        try { Add-MpPreference -ExclusionPath $root -ErrorAction SilentlyContinue } catch { }
    }
    $env:TEMP = $root
    $env:TMP = $root
}

# Stage <src> as portzero.exe in a fresh unblocked subdir of the excluded temp
# root; return the local path. Only reads/copies <src>, never executes it, so a
# \\Mac\Home share source (UNC + Mark-of-the-Web) is fine here.
function Stage-LocalExe {
    param([string] $Src, [string] $Prefix)
    $dir = Join-Path $env:TEMP ($Prefix + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $dst = Join-Path $dir 'portzero.exe'
    Copy-Item -LiteralPath $Src -Destination $dst
    Unblock-File -LiteralPath $dst
    return $dst
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
# Route all temp work into a Defender-excluded root (see Enable-ExcludedTemp),
# then stage + run the binary from there. Never execute it straight off the
# \\Mac\Home share (UNC + Mark-of-the-Web) or from a non-excluded dir (Defender
# blocks an unsigned exe from starting) — either way the process never launches.
Enable-ExcludedTemp
$P = Stage-LocalExe -Src $exe -Prefix 'pz-autoupdate-'

# Probe `update` support. Capture output so a launch failure (empty $LASTEXITCODE
# = the process never ran, e.g. Defender still blocking) is diagnosable rather
# than masquerading as a missing subcommand. clap exits 0 for a known
# subcommand's --help, non-zero for an unrecognized one.
$probe = (& $P update --help 2>&1 | Out-String)
if ("$LASTEXITCODE" -ne '0') {
    "RESULT=FAIL ``portzero update --help`` did not run cleanly (exit '$LASTEXITCODE'): $($probe.Trim())"
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
$idBefore = Get-FileId -Path $P
& $P update --force *> (Join-Path $env:TEMP 'pz-autoupdate-apply.log')
Phase 'apply-exit-zero' ($LASTEXITCODE -eq 0)
Phase 'apply-binary-runs' ((Get-Version -Exe $P) -ne '')
# Proof the running .exe was actually replaced: its NTFS file identity changes
# when the updater renames it aside and copies the new build into place (the
# inode-change analog the unix side asserts). Identity-based, so it holds even
# though the hermetic mirror's bytes match the original — and it does not depend
# on the moved-aside portzero.old surviving process exit, which flaked before.
$idAfter = Get-FileId -Path $P
Phase 'apply-swapped-binary' ($idBefore -and $idAfter -and ($idAfter -ne $idBefore))
Phase 'apply-provenance' ((Get-Sha -Path $P) -eq $archiveSha)
# Informational only: the rollback backup the Windows updater leaves behind.
# Its post-exit survival is environment-dependent, so this is observed, not
# asserted (identity above is the hard swap proof). Greppable for diagnosis.
"NOTE=apply-backup-present old=$([bool](Test-Path $backup)) idBefore=$idBefore idAfter=$idAfter"

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
    # Stage the prior binary the same way (excluded+unblocked local dir), then
    # probe support by exit code.
    $pOld = Stage-LocalExe -Src $old -Prefix 'pz-autoupdate-old-'
    & $pOld update --help *> $null
    if ("$LASTEXITCODE" -ne '0') {
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
