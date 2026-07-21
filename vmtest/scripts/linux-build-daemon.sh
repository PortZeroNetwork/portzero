#!/usr/bin/env bash
# Provision the Ubuntu guest from the OFFLINE cache and build portzero, as root.
# Mirrors win-build-daemon.ps1: install build deps + Rust from cache, copy source
# and vendored crates locally, build fully offline. Idempotent. Prints KEY=value.
set -euo pipefail
CACHE=/media/psf/MBP-Sidecar/loumtech/vm-toolchain-cache
REPO=/media/psf/Home/Documents/src/PortZeroNetwork/portzero
SRC=/root/src/portzero
VENDOR=/root/vendor
export PATH=/usr/local/bin:$PATH

# --- build deps from cached .debs (build-essential, cmake, pkg-config, libssl-dev, perl) ---
# Multi-pass dpkg: the debs have inter-dependencies, so a single `dpkg -i *.deb`
# fails on ordering; repeating resolves it (then configure any left half-set).
if ! command -v gcc >/dev/null 2>&1; then
    echo "sync=apt-debs"
    for _ in 1 2 3; do dpkg -i "$CACHE"/linux/apt-debs/*.deb >/dev/null 2>&1 || true; done
    dpkg --configure -a >/dev/null 2>&1 || true
fi
command -v gcc >/dev/null 2>&1 || { echo "gcc=MISSING (deb install failed)"; exit 1; }
echo "gcc=$(command -v gcc)"

# --- Rust from cached standalone tarball (offline install to /usr/local) ---
# Extract to /var/tmp (real disk) NOT /tmp — /tmp is a small RAM tmpfs that the
# ~1.5 GB tarball overflows. Install only rustc+cargo+std (skip the ~1 GB docs).
if ! command -v cargo >/dev/null 2>&1; then
    echo "sync=rust"
    tmp=$(mktemp -d -p /var/tmp)
    tar -xf "$CACHE"/linux/rust-*-x86_64-unknown-linux-gnu.tar.xz -C "$tmp"
    ( cd "$tmp"/rust-*-x86_64-unknown-linux-gnu && \
        ./install.sh --prefix=/usr/local --disable-ldconfig \
          --components=rustc,cargo,rust-std-x86_64-unknown-linux-gnu >/dev/null )
    rm -rf "$tmp"
fi
command -v cargo >/dev/null 2>&1 || { echo "cargo=MISSING"; exit 1; }
echo "cargo=$(cargo --version)"

# --- source -> local (tar is always present; no rsync dependency) ---
echo "sync=source"
mkdir -p "$SRC"
( cd "$REPO" && tar cf - --exclude=target --exclude=.git --exclude=vendor --exclude=.ticketry --exclude=node_modules . ) \
    | ( cd "$SRC" && tar xf - )

# --- vendored crates -> local (one-time; 841 MB) ---
if [ ! -d "$VENDOR/anstyle" ]; then
    echo "sync=vendor(first-time)"; mkdir -p "$VENDOR"; cp -a "$CACHE"/common/vendor/. "$VENDOR"/
else
    echo "sync=vendor(cached)"
fi

# --- offline cargo config: replace crates.io with the local vendor dir ---
mkdir -p "$SRC/.cargo"
cat > "$SRC/.cargo/config.toml" <<EOF
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "$VENDOR"

[net]
offline = true
EOF

# --- build ---
cd "$SRC"
export CARGO_TARGET_DIR=/root/pz-target
echo "build=start $(date -u +%FT%TZ)"
cargo build --release --offline --bin portzero
code=$?
echo "build_exit=$code"
EXE=/root/pz-target/release/portzero
echo "exe=$EXE exists=$([ -x "$EXE" ] && echo true || echo false)"
[ -x "$EXE" ] && echo "portzero_version=$("$EXE" --version 2>&1)"
