#!/usr/bin/env bash
# Install Xcode Command Line Tools headlessly (no GUI, no Apple ID) via
# softwareupdate.
#
# CACHE-FIRST (this host is often on a METERED link — never download twice):
# CLT cannot be staged as a portable file in the shared cache — Apple only vends
# it through softwareupdate, and the macOS guest can't read the shared cache
# (TCC) anyway — so the "cache" for CLT is the VM SNAPSHOT taken AFTER this runs
# (baked into `portzero-built` by `just vm-macos-add-brew`, mirrored to the 4 TB
# by `just vm-sync-macos`). This script only downloads when CLT is genuinely
# absent; the guard below is the cache check. To get CLT back after a revert,
# revert to a snapshot that has it (portzero-built) — do NOT re-run this on a
# metered link. See vmtest/README.md "Cache-first".
#
# Idempotent: exits early if CLT already compiles.
set -euo pipefail

if clang -x c -o /tmp/_cltcheck - <<<'int main(){return 0;}' 2>/dev/null; then
    echo "clt_already=yes (cache hit — CLT already present, no download)"; exit 0
fi

# This sentinel makes softwareupdate list the on-demand CLT package.
trigger=/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress
sudo touch "$trigger"
label=$(softwareupdate --list 2>/dev/null \
    | grep -E 'Label: Command Line Tools' \
    | sed -E 's/.*Label: //' \
    | sort -V | tail -1)
echo "clt_label=${label:-NONE}"
if [ -z "$label" ]; then sudo rm -f "$trigger"; echo "clt_install=NO_PACKAGE"; exit 1; fi

sudo softwareupdate --install "$label" --verbose
sudo rm -f "$trigger"

if clang -x c -o /tmp/_cltcheck - <<<'int main(){return 0;}' 2>/dev/null; then
    echo "clt_install=OK"
else
    echo "clt_install=FAILED_VERIFY"; exit 1
fi
