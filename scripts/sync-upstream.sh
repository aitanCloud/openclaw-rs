#!/usr/bin/env bash
# sync-upstream.sh — Update UPSTREAM.json to pin to the current upstream HEAD
# Run this AFTER reviewing upstream changes and confirming compatibility.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
UPSTREAM_FILE="$PROJECT_DIR/UPSTREAM.json"

echo "Fetching latest upstream state..."

LATEST_JSON=$(curl -sf "https://api.github.com/repos/openclaw/openclaw/commits/main" \
  -H "Accept: application/vnd.github.v3+json")

LATEST_COMMIT=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['sha'])")
LATEST_DATE=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['commit']['committer']['date'])")
LATEST_MSG=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['commit']['message'].split(chr(10))[0][:80])")

LATEST_VERSION=$(curl -sf "https://raw.githubusercontent.com/openclaw/openclaw/main/package.json" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['version'])")

PINNED_COMMIT=$(python3 -c "import json; print(json.load(open('$UPSTREAM_FILE'))['upstream']['pinnedCommit'])")
PINNED_VERSION=$(python3 -c "import json; print(json.load(open('$UPSTREAM_FILE'))['upstream']['pinnedVersion'])")

echo ""
echo "  Current pin:  $PINNED_VERSION @ ${PINNED_COMMIT:0:12}"
echo "  Latest:       $LATEST_VERSION @ ${LATEST_COMMIT:0:12}"
echo "  Commit:       $LATEST_MSG"
echo "  Date:         $LATEST_DATE"
echo ""

if [ "$LATEST_COMMIT" = "$PINNED_COMMIT" ]; then
  echo "Already up to date. Nothing to sync."
  exit 0
fi

read -p "Update pin to $LATEST_VERSION @ ${LATEST_COMMIT:0:12}? [y/N] " confirm
if [ "$confirm" != "y" ] && [ "$confirm" != "Y" ]; then
  echo "Aborted."
  exit 0
fi

NOW=$(date -Iseconds)

python3 -c "
import json
with open('$UPSTREAM_FILE', 'r') as f:
    data = json.load(f)
data['upstream']['pinnedVersion'] = '$LATEST_VERSION'
data['upstream']['pinnedCommit'] = '$LATEST_COMMIT'
data['upstream']['pinnedAt'] = '$LATEST_DATE'
data['upstream']['lastChecked'] = '$NOW'
with open('$UPSTREAM_FILE', 'w') as f:
    json.dump(data, f, indent=2)
    f.write('\n')
"

echo ""
echo "✓ UPSTREAM.json updated: $PINNED_VERSION → $LATEST_VERSION"
echo "  Commit: ${LATEST_COMMIT:0:12}"
echo ""
echo "Next steps:"
echo "  1. Review upstream changes: https://github.com/openclaw/openclaw/compare/${PINNED_COMMIT:0:12}...${LATEST_COMMIT:0:12}"
echo "  2. Update Rust port if needed"
echo "  3. Run tests: cargo test --release"
echo "  4. Commit: git add UPSTREAM.json && git commit -m 'sync: pin to openclaw $LATEST_VERSION'"
