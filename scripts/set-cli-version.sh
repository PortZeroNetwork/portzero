#!/usr/bin/env bash
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
import re
import sys

version = sys.argv[1]
manifest_path = Path("client/crates/cli/Cargo.toml")
lock_path = Path("Cargo.lock")

manifest = manifest_path.read_text()
manifest, manifest_count = re.subn(
    r'(?m)^(version = ")[^"]+(")',
    rf"\g<1>{version}\2",
    manifest,
    count=1,
)
if manifest_count != 1:
    raise SystemExit("missing CLI package version")
manifest_path.write_text(manifest)

lock = lock_path.read_text()
package_re = re.compile(
    r'(\[\[package\]\]\nname = "portzero-cli"\nversion = ")([^"]+)(")'
)
lock, lock_count = package_re.subn(rf"\g<1>{version}\3", lock, count=1)
if lock_count != 1:
    raise SystemExit("missing portzero-cli package in Cargo.lock")
lock_path.write_text(lock)
PY
