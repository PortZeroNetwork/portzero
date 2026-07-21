#!/usr/bin/env bash
# One-time (re)provisioning: bake Homebrew into the macOS VM's "portzero-built"
# reset point — the snapshot `just vm-test macos` and the CI VM-E2E job revert
# to — so both exercise the real `brew install portzero` path.
#
# This is now a thin wrapper over the vmkit `provision` primitive (the reusable
# "reset -> run a guest script -> re-checkpoint" dance; see the kit's
# docs/PROVISIONING.md). vmkit owns the reset/preserve/re-checkpoint mechanics;
# this script only names the pieces specific to Homebrew on our macOS VM.
#
# Runs entirely on the INTERNAL working copy (~/Parallels); macOS runs poorly
# off the 4 TB spinny drive, so the install happens on fast storage and the
# result is mirrored out afterwards with `just vm-sync-macos`.
#
# Snapshot topology it produces (nothing pristine is destroyed):
#   ... -> macOS 15.7.7-built (pristine)     [pre-existing]
#          -> portzero-pre-brew (pristine)   [preservation snapshot, taken once]
#             -> portzero-built (Homebrew)   [re-captured reset point — churns]
#                -> portzero-toolchain (Homebrew)  [permanent CLT+brew anchor]
# The golden powered-off "MacOS 15.7.7" snapshot is never touched, so a fully
# clean baseline also remains available.
#
# Why BOTH portzero-built and portzero-toolchain (identical CLT+brew state):
# `portzero-built` is the reset point vm-test/CI revert to, and a future
# re-provision (`vm-checkpoint built`) REPLACES it — so it's churn-prone.
# `portzero-toolchain` is a permanent anchor the routine flow never overwrites:
# it is the durable cache of the one-time CLT+Homebrew download (metered link —
# see vmtest/README.md "Cache-first"). If `built` is ever clobbered, revert to
# `portzero-toolchain` and re-checkpoint `built` — no re-download, ever. This is
# `vmkit provision`'s `--anchor`; the pristine baseline is its `--preserve-as`.
#
# Idempotent-friendly: the guest install script no-ops if brew already exists,
# and vmkit only takes the pristine preservation snapshot if it isn't there yet.
set -euo pipefail

# reset macos to `built` -> preserve the pristine baseline once as
# `portzero-pre-brew` -> install Homebrew in the guest -> re-capture `built` ->
# capture the permanent `portzero-toolchain` anchor. vmkit supplies the safety
# rails (internal-disk-only recovery, guarded guest exec, host-side timeout).
vmkit provision macos vmtest/scripts/macos-install-homebrew.sh \
    --checkpoint built \
    --preserve-as pre-brew \
    --anchor toolchain \
    --timeout "${VMKIT_RUN_TIMEOUT:-2400}"

cat <<'EOF'

Done.
  portzero-built      -> CLT + Homebrew (vm-test/CI reset point)
  portzero-toolchain  -> CLT + Homebrew (permanent anchor; the durable cache of
                         the one-time download — revert here if 'built' is lost)
  portzero-pre-brew   -> pristine, no CLT/brew (preserved)
Next, mirror the updated VM to the 4 TB drive (off-machine backup of the cache):
    vmkit stop macos
    just vm-sync-macos
EOF
