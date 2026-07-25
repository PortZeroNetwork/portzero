#!/usr/bin/env bash
# Stamp one version across every artifact a release build produces.
#
# The workspace has a single `[workspace.package] version` that every crate
# inherits, so `portzero`, the daemon it runs, `portzero-tray`, and
# `portzero-app` built from one commit all report the same version. This script
# rewrites that one version (plus the Cargo.lock entries derived from it and the
# Tauri bundle version) so a release is stamped in exactly three files.
#
# Runtime version reporting (`portzero version`, the app's Version panel,
# `portzero doctor`) compares what each running component reports; that
# comparison only means something if this script leaves nothing behind.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <semver>" >&2
  exit 2
fi

version="$1"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid semantic version: $version" >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$repo_root"

python3 - "$version" <<'PY'
from pathlib import Path
import json
import re
import sys

version = sys.argv[1]
workspace_path = Path("Cargo.toml")
lock_path = Path("Cargo.lock")
tauri_path = Path("client/crates/app/tauri.conf.json")

# 1. The single source of truth: [workspace.package] version.
workspace = workspace_path.read_text()
workspace, count = re.subn(
    r'(?ms)(^\[workspace\.package\]\n(?:[^\[]*?\n)?version = ")[^"]+(")',
    rf"\g<1>{version}\2",
    workspace,
    count=1,
)
if count != 1:
    raise SystemExit(
        "missing [workspace.package] version in Cargo.toml — every crate is "
        "supposed to inherit its version from there"
    )
workspace_path.write_text(workspace)

# 2. Cargo.lock records a resolved version per workspace member. Rewriting them
#    here keeps `--locked` builds (which refuse to update the lockfile) working.
members = re.findall(r'(?m)^\s*"([^"]+)",\s*$', re.search(
    r"(?ms)^members = \[(.*?)\]", workspace_path.read_text()
).group(1))
member_names = []
for member in members:
    manifest = Path(member) / "Cargo.toml"
    name = re.search(r'(?m)^name = "([^"]+)"', manifest.read_text())
    if name:
        member_names.append(name.group(1))
if not member_names:
    raise SystemExit("no workspace members found in Cargo.toml")

lock = lock_path.read_text()
for name in member_names:
    lock, count = re.subn(
        rf'(\[\[package\]\]\nname = "{re.escape(name)}"\nversion = ")([^"]+)(")',
        rf"\g<1>{version}\3",
        lock,
        count=1,
    )
    if count != 1:
        raise SystemExit(f"missing {name} package in Cargo.lock")
lock_path.write_text(lock)

# 3. The Tauri bundle version (installer metadata for the desktop app). The
#    app's own reported version comes from CARGO_PKG_VERSION, but bundle
#    metadata disagreeing with it would show a stale version in the OS's
#    "installed applications" list.
tauri = json.loads(tauri_path.read_text())
tauri["version"] = version
tauri_path.write_text(json.dumps(tauri, indent=2) + "\n")

print(f"stamped version {version} across {1 + len(member_names) + 1} entries")
PY
