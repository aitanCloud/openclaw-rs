#!/usr/bin/env bash
# check-upstream.sh — Check if upstream openclaw/openclaw has new commits
# Reads pinned version from UPSTREAM.json, compares to latest on GitHub.
# Exits 0 if up-to-date, 1 if upstream has changed.
# Optionally sends a notification via OpenClaw gateway.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
UPSTREAM_FILE="$PROJECT_DIR/UPSTREAM.json"
STATE_FILE="$HOME/.openclaw/workspace/logs/upstream-check.log"

# ── Read pinned state ──
PINNED_COMMIT=$(python3 -c "import json; print(json.load(open('$UPSTREAM_FILE'))['upstream']['pinnedCommit'])")
PINNED_VERSION=$(python3 -c "import json; print(json.load(open('$UPSTREAM_FILE'))['upstream']['pinnedVersion'])")

# ── Fetch latest from GitHub API ──
LATEST_JSON=$(curl -sf "https://api.github.com/repos/openclaw/openclaw/commits/main" \
  -H "Accept: application/vnd.github.v3+json" 2>/dev/null) || {
  echo "[$(date -Iseconds)] ERROR: Failed to fetch upstream commit" | tee -a "$STATE_FILE"
  exit 2
}

LATEST_COMMIT=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['sha'])")
LATEST_DATE=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['commit']['committer']['date'])")
LATEST_MSG=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['commit']['message'].split(chr(10))[0][:80])")

# ── Fetch latest package.json version ──
LATEST_VERSION=$(curl -sf "https://raw.githubusercontent.com/openclaw/openclaw/main/package.json" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['version'])" 2>/dev/null) || LATEST_VERSION="unknown"

NOW=$(date -Iseconds)

# ── Update lastChecked in UPSTREAM.json ──
python3 -c "
import json
with open('$UPSTREAM_FILE', 'r') as f:
    data = json.load(f)
data['upstream']['lastChecked'] = '$NOW'
with open('$UPSTREAM_FILE', 'w') as f:
    json.dump(data, f, indent=2)
    f.write('\n')
"

# ── Compare ──
if [ "$LATEST_COMMIT" = "$PINNED_COMMIT" ]; then
  echo "[$NOW] UP-TO-DATE: openclaw $PINNED_VERSION @ ${PINNED_COMMIT:0:12}" | tee -a "$STATE_FILE"
  exit 0
fi

# ── Upstream has changed ──
DRIFT_MSG="UPSTREAM DRIFT DETECTED
  Pinned:  $PINNED_VERSION @ ${PINNED_COMMIT:0:12}
  Latest:  $LATEST_VERSION @ ${LATEST_COMMIT:0:12} ($LATEST_DATE)
  Commit:  $LATEST_MSG
  Action:  Review changes and update UPSTREAM.json if safe to sync"

echo "[$NOW] $DRIFT_MSG" | tee -a "$STATE_FILE"

# ── Version bump detection ──
if [ "$LATEST_VERSION" != "$PINNED_VERSION" ]; then
  VERSION_MSG="⚠️  VERSION BUMP: openclaw $PINNED_VERSION → $LATEST_VERSION"
  echo "[$NOW] $VERSION_MSG" | tee -a "$STATE_FILE"
fi

# ── Notify via OpenClaw gateway (if running) ──
GATEWAY_TOKEN=$(python3 -c "
import json
try:
    cfg = json.load(open('$HOME/.openclaw/openclaw-manual.json'))
    auth = cfg.get('gateway', {}).get('auth', {})
    print(auth.get('token', '') if isinstance(auth, dict) else '')
except: print('')
" 2>/dev/null)

if [ -n "$GATEWAY_TOKEN" ]; then
  NOTIFY_BODY=$(python3 -c "
import json
msg = '''🦀 openclaw-rs upstream check:
$DRIFT_MSG'''
print(json.dumps({
    'tool': 'message',
    'action': 'send',
    'args': {
        'message': msg,
        'channel': 'last'
    }
}))
")
  curl -sf -X POST "http://127.0.0.1:3001/tools/invoke" \
    -H "Authorization: Bearer $GATEWAY_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$NOTIFY_BODY" > /dev/null 2>&1 || true
fi

exit 1
