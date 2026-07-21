#!/usr/bin/env bash
# Download VM provisioning installers into the Mac-side cache ONCE, on an
# unmetered connection. Idempotent: skips anything already present with the
# right size. The cache (vmtest/cache) survives VM snapshot reverts, so the
# guest installs from it offline and metered sessions never re-download.
#
# Tiers (set TIER=… ; default "artifact"):
#   artifact  ~0 MB of installers  — test the CI-built portzero.exe / MSI only.
#             The binary itself is fetched per-run via `gh run download`, not here.
#   toolchain ~2-3 GB — rustup + git + MSVC Build Tools offline layout, so the
#             daemon can be BUILT and iterated inside the VM fully offline.
set -euo pipefail

TIER="${TIER:-artifact}"
# Toolchain cache lives on the 4 TB drive (shared into all guests), NOT in the
# repo — it is cross-repo and too big to track. Override with CACHE=… if the
# drive is elsewhere. See that dir's README.md for layout.
CACHE="${CACHE:-/Volumes/MBP-Sidecar/loumtech/vm-toolchain-cache}"
mkdir -p "$CACHE"/{windows,linux,macos,common}
echo "Cache: $CACHE   Tier: $TIER"

RUSTVER="$(cat "$CACHE/common/rust-stable-version.txt" 2>/dev/null || echo 1.97.0)"

fetch() { # url  relpath (under CACHE, may include a subdir)
    local url="$1" name="$2" dest="$CACHE/$2"
    mkdir -p "$(dirname "$dest")"
    if [ -s "$dest" ]; then echo "  have $name ($(du -h "$dest" | cut -f1))"; return; fi
    echo "  get  $name"
    curl -fL --retry 3 --connect-timeout 20 -o "$dest.part" "$url"
    mv "$dest.part" "$dest"
    echo "       -> $(du -h "$dest" | cut -f1)"
}

if [ "$TIER" = "artifact" ]; then
    echo "artifact tier: no installers to pre-download."
    echo "Fetch the binary under test per-run with, e.g.:"
    echo "  gh run download <run-id> -n portzero-windows-amd64 -D vmtest/cache/bin"
    exit 0
fi

if [ "$TIER" = "toolchain" ]; then
    echo "$RUSTVER" > "$CACHE/common/rust-stable-version.txt"
    # Standalone, offline-installable Rust toolchains, pinned to CI's stable.
    fetch "https://static.rust-lang.org/dist/rust-$RUSTVER-x86_64-unknown-linux-gnu.tar.xz" "linux/rust-$RUSTVER-x86_64-unknown-linux-gnu.tar.xz"
    fetch "https://static.rust-lang.org/dist/rust-$RUSTVER-x86_64-pc-windows-msvc.msi"      "windows/rust-$RUSTVER-x86_64-pc-windows-msvc.msi"
    # macOS Rust lives on the fast INTERNAL disk (see cache README); only fetch
    # here if that copy is absent.
    macos_int="$HOME/Parallels/vm-toolchain-cache/macos/rust-$RUSTVER-x86_64-apple-darwin.tar.xz"
    if [ ! -s "$macos_int" ]; then
        mkdir -p "$(dirname "$macos_int")"
        echo "  get  macos/rust-$RUSTVER (internal disk)"
        curl -fL --retry 3 -o "$macos_int.part" "https://static.rust-lang.org/dist/rust-$RUSTVER-x86_64-apple-darwin.tar.xz" && mv "$macos_int.part" "$macos_int"
    else
        echo "  have macos rust (internal disk)"
    fi
    # Windows extras.
    fetch "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe" "windows/rustup-init.exe"
    fetch "https://github.com/git-for-windows/git/releases/download/v2.47.1.windows.1/Git-2.47.1-64-bit.exe" "windows/git-64-bit.exe"
    fetch "https://aka.ms/vs/17/release/vs_BuildTools.exe" "windows/vs_BuildTools.exe"
    echo
    echo "Then build the multi-GB parts that require a Windows/Linux guest (once):"
    echo "  MSVC layout : just vm-run \"Windows Pro\" vmtest/scripts/build-msvc-layout.ps1"
    echo "  Ubuntu debs : run 'apt-get install -d build-essential cmake pkg-config libssl-dev perl'"
    echo "                in the Ubuntu guest, copy /var/cache/apt/archives/*.deb to linux/apt-debs/"
    exit 0
fi

echo "unknown TIER=$TIER (use: artifact | toolchain)" >&2
exit 1
