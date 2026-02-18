#!/usr/bin/env bash
# check-upstream.sh — Check if upstream openclaw/openclaw has new commits.
# If upstream has moved ahead of our pin, fetch the latest into the local
# bare mirror at ~/code/openclaw-upstream.git so changes are ready for review.
# Runs daily at 5 PM ET via systemd timer.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
UPSTREAM_FILE="$PROJECT_DIR/UPSTREAM.json"
MIRROR_DIR="$HOME/code/openclaw-upstream.git"
LOG_FILE="$HOME/.openclaw/workspace/logs/upstream-check.log"
NOW=$(date -Iseconds)

mkdir -p "$(dirname "$LOG_FILE")"

log() { echo "[$NOW] $*" | tee -a "$LOG_FILE"; }

# ── Read pinned state ──
PINNED_COMMIT=$(python3 -c "import json; print(json.load(open('$UPSTREAM_FILE'))['upstream']['pinnedCommit'])")
PINNED_VERSION=$(python3 -c "import json; print(json.load(open('$UPSTREAM_FILE'))['upstream']['pinnedVersion'])")

# ── Fetch latest commit metadata from GitHub API ──
LATEST_JSON=$(curl -sf "https://api.github.com/repos/openclaw/openclaw/commits/main" \
  -H "Accept: application/vnd.github.v3+json" 2>/dev/null) || {
  log "ERROR: Failed to fetch upstream commit from GitHub API"
  exit 2
}

LATEST_COMMIT=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['sha'])")
LATEST_DATE=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['commit']['committer']['date'])")
LATEST_MSG=$(echo "$LATEST_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['commit']['message'].split(chr(10))[0][:100])")

# ── Fetch latest package.json version ──
LATEST_VERSION=$(curl -sf "https://raw.githubusercontent.com/openclaw/openclaw/main/package.json" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['version'])" 2>/dev/null) || LATEST_VERSION="unknown"

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
  log "UP-TO-DATE: openclaw $PINNED_VERSION @ ${PINNED_COMMIT:0:12}"
  exit 0
fi

# ── Upstream has moved ──
log "DRIFT DETECTED: pinned $PINNED_VERSION @ ${PINNED_COMMIT:0:12} → upstream $LATEST_VERSION @ ${LATEST_COMMIT:0:12}"
log "  Latest commit: $LATEST_MSG ($LATEST_DATE)"
log "  Compare: https://github.com/openclaw/openclaw/compare/${PINNED_COMMIT:0:12}...${LATEST_COMMIT:0:12}"

if [ "$LATEST_VERSION" != "$PINNED_VERSION" ]; then
  log "  ⚠ VERSION BUMP: $PINNED_VERSION → $LATEST_VERSION"
fi

# ── Pull latest into bare mirror ──
if [ -d "$MIRROR_DIR" ]; then
  log "  Fetching upstream into $MIRROR_DIR ..."
  if git -C "$MIRROR_DIR" fetch origin 2>&1; then
    MIRROR_HEAD=$(git -C "$MIRROR_DIR" rev-parse refs/remotes/origin/main 2>/dev/null || echo "unknown")
    log "  Mirror updated: HEAD now ${MIRROR_HEAD:0:12}"
  else
    log "  ERROR: git fetch failed"
  fi
else
  log "  Mirror not found at $MIRROR_DIR — cloning..."
  if git clone --bare https://github.com/openclaw/openclaw.git "$MIRROR_DIR" 2>&1; then
    log "  Mirror cloned successfully"
  else
    log "  ERROR: git clone failed"
  fi
fi

# ── Write drift summary for easy review ──
DRIFT_FILE="$PROJECT_DIR/UPSTREAM-DRIFT.md"
cat > "$DRIFT_FILE" <<EOF
# Upstream Drift Detected

**Detected:** $NOW
**Pinned:** $PINNED_VERSION @ \`${PINNED_COMMIT:0:12}\`
**Latest:** $LATEST_VERSION @ \`${LATEST_COMMIT:0:12}\`
**Commit:** $LATEST_MSG

## Review

\`\`\`bash
# Compare changes on GitHub:
# https://github.com/openclaw/openclaw/compare/${PINNED_COMMIT:0:12}...${LATEST_COMMIT:0:12}

# View changes locally (bare mirror):
git -C ~/code/openclaw-upstream.git log --oneline ${PINNED_COMMIT:0:12}..origin/main

# View specific file changes:
git -C ~/code/openclaw-upstream.git diff ${PINNED_COMMIT:0:12}..origin/main -- src/agents/workspace.ts
git -C ~/code/openclaw-upstream.git diff ${PINNED_COMMIT:0:12}..origin/main -- src/agents/agent-scope.ts
git -C ~/code/openclaw-upstream.git diff ${PINNED_COMMIT:0:12}..origin/main -- src/agents/openclaw-tools.ts
git -C ~/code/openclaw-upstream.git diff ${PINNED_COMMIT:0:12}..origin/main -- package.json

# After reviewing and updating the Rust port:
./scripts/sync-upstream.sh
\`\`\`
EOF

log "  Drift summary written to UPSTREAM-DRIFT.md"

# ── Notify via OpenClaw gateway ──
GATEWAY_TOKEN=$(python3 -c "
import json
try:
    cfg = json.load(open('$HOME/.openclaw/openclaw-manual.json'))
    auth = cfg.get('gateway', {}).get('auth', {})
    print(auth.get('token', '') if isinstance(auth, dict) else '')
except: print('')
" 2>/dev/null)

if [ -n "$GATEWAY_TOKEN" ]; then
  NOTIFY_MSG="🦀 openclaw-rs upstream drift:
Pinned: $PINNED_VERSION @ ${PINNED_COMMIT:0:12}
Latest: $LATEST_VERSION @ ${LATEST_COMMIT:0:12}
Commit: $LATEST_MSG
Mirror pulled — ready for review."

  NOTIFY_BODY=$(python3 -c "
import json, sys
print(json.dumps({
    'tool': 'message',
    'action': 'send',
    'args': {
        'message': '''$NOTIFY_MSG''',
        'channel': 'last'
    }
}))
")
  curl -sf -X POST "http://127.0.0.1:3001/tools/invoke" \
    -H "Authorization: Bearer $GATEWAY_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$NOTIFY_BODY" > /dev/null 2>&1 || log "  WARN: Gateway notification failed (gateway may be offline)"
fi

exit 1
