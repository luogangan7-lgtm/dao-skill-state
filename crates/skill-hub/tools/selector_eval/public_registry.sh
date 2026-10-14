#!/usr/bin/env bash
# Build a skill-hub registry of the pinned third-party skills (crates/dao/tools/intake_bench.json):
#   public_registry.sh STORE REGISTRY     (expects built dao and skill-hub release binaries)
# Used by the public-bench and selector-eval workflows (evaluation A4). CI only.
set -euo pipefail
root=$(cd "$(dirname "$0")/../../../.." && pwd)
dao=$root/crates/dao/target/release/dao
hub=$root/crates/skill-hub/target/release/skill-hub
jq -c '.sources[]' "$root/crates/dao/tools/intake_bench.json" | while read -r src; do
  repo=$(jq -r .repo <<<"$src"); rev=$(jq -r .rev <<<"$src")
  # stdin closed: a child must not swallow the remaining source lines
  "$dao" intake --store "$1" --git "https://github.com/$repo" --rev "$rev" --all < /dev/null > /dev/null
done
"$hub" index --root thirdparty="$1/objects" --out "$2"
n=$(jq '.skills | length' "$2"); echo "skills in registry: $n"
[ "$n" -ge 100 ] || { echo "expected the 112 pinned skills, got $n"; exit 1; }
