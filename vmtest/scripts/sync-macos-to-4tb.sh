#!/usr/bin/env bash
# Mirror the macOS VM and its toolchain cache from the fast internal disk to the
# 4 TB external drive, alongside the Windows and Ubuntu VMs.
#
# Why macOS lives on the internal disk: macOS is tuned for high-speed storage and
# runs poorly from the external drive; Windows/Linux tolerate it fine. So the
# macOS VM's *working* copy stays internal, and this makes a consolidated backup
# on the 4 TB (so all three VMs and all caches exist in one place off-machine).
#
# The VM MUST be powered off for a consistent copy (rsync of a live .pvm is
# corrupt). Refuses to run if it is not.
#
# Usage: sync-macos-to-4tb.sh [--delete] [--dry-run]
#   --delete   make it a true mirror (remove extra files on the destination)
#   --dry-run  show what would change, copy nothing
set -euo pipefail

VM_NAME="macOS 15.7.7"
SRC_PVM="$HOME/Parallels/$VM_NAME.pvm"
SRC_CACHE="$HOME/Parallels/vm-toolchain-cache/macos"
DST_PVM_DIR="/Volumes/MBP-Sidecar/loumtech/parallels"
DST_CACHE="/Volumes/MBP-Sidecar/loumtech/vm-toolchain-cache/macos"

# Portable across macOS's bundled rsync (2.6.9 / openrsync) and Homebrew rsync 3.
opts=(-a --partial -h --progress)
for a in "$@"; do
    case "$a" in
        --delete)  opts+=(--delete) ;;
        --dry-run) opts+=(--dry-run) ;;
        *) echo "unknown arg: $a" >&2; exit 2 ;;
    esac
done

[ -d "/Volumes/MBP-Sidecar" ] || { echo "4TB drive (/Volumes/MBP-Sidecar) not mounted"; exit 1; }

# Guard: the VM must be off. prlctl status is the source of truth.
status=$(prlctl status "$VM_NAME" 2>/dev/null | awk '{print $NF}')
if [ "$status" != "stopped" ]; then
    echo "Refusing to sync: VM '$VM_NAME' is '$status'. Run: prlctl stop \"$VM_NAME\""
    exit 1
fi

mkdir -p "$DST_PVM_DIR" "$DST_CACHE"
echo ">> VM bundle  ($(du -sh "$SRC_PVM" | cut -f1)) -> $DST_PVM_DIR/"
rsync "${opts[@]}" "$SRC_PVM" "$DST_PVM_DIR/"
if [ -d "$SRC_CACHE" ]; then
    echo ">> macOS cache -> $DST_CACHE/"
    rsync "${opts[@]}" "$SRC_CACHE/" "$DST_CACHE/"
fi
echo "Done. macOS VM + cache mirrored to the 4 TB drive."
