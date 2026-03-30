#!/usr/bin/env bash
# Launch ZeroClaw with the best available Anthropic credential.
#
# Auth resolution order:
#   1. ANTHROPIC_OAUTH_TOKEN from environment (if already set)
#   2. OAuth token from macOS keychain (anthropic-oauth-token)
#   3. OAuth token from Claude Code keychain (Claude Code-credentials)
#   4. Session key from Chrome cookies → CLAUDE_SESSION_KEY (needs ZEROCLAW_PROXY_URL)
#   5. ANTHROPIC_API_KEY from environment (if already set)
#
# Usage: ./run.sh gateway --port 8080
#        ./run.sh agent -m "Hi"
#        ./run.sh daemon
#        ./run.sh status

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── Proxy URL for session-key auth (Worker handles claude.ai routing) ──
export ZEROCLAW_PROXY_URL="${ZEROCLAW_PROXY_URL:-https://zeroclaw-proxy.connect-2a2.workers.dev}"

# ── Try OAuth token ────────────────────────────────────────────────────
if [[ -z "${ANTHROPIC_OAUTH_TOKEN:-}" ]]; then
  # 1. Dedicated keychain entry
  OAUTH_TOKEN=$(security find-generic-password -s "anthropic-oauth-token" -w 2>/dev/null) || OAUTH_TOKEN=""

  # 2. Claude Code credentials keychain (fallback)
  if [[ -z "$OAUTH_TOKEN" ]]; then
    OAUTH_JSON=$(security find-generic-password -s "Claude Code-credentials" -w 2>/dev/null) || OAUTH_JSON=""
    if [[ -n "$OAUTH_JSON" ]]; then
      OAUTH_TOKEN=$(echo "$OAUTH_JSON" | python3 -c "
import sys, json, time
try:
    data = json.load(sys.stdin)
    oauth = data.get('claudeAiOauth', {})
    token = oauth.get('accessToken', '')
    expires = oauth.get('expiresAt', 0)
    if token and (expires == 0 or expires / 1000 > time.time()):
        print(token)
except Exception:
    pass
" 2>/dev/null) || OAUTH_TOKEN=""
    fi
  fi

  if [[ -n "$OAUTH_TOKEN" ]]; then
    export ANTHROPIC_OAUTH_TOKEN="$OAUTH_TOKEN"
  fi
fi

# ── Try session key from Chrome cookies ────────────────────────────────
if [[ -z "${ANTHROPIC_OAUTH_TOKEN:-}" && -z "${CLAUDE_SESSION_KEY:-}" ]]; then
  SESSION_KEY=$(python3 "$SCRIPT_DIR/claude-session-kit/scripts/get-session-key.py" --get-key 2>/dev/null) || SESSION_KEY=""
  if [[ -n "$SESSION_KEY" ]]; then
    export CLAUDE_SESSION_KEY="$SESSION_KEY"
  fi
fi

# ── Verify we have at least one credential ─────────────────────────────
if [[ -z "${ANTHROPIC_OAUTH_TOKEN:-}" && -z "${CLAUDE_SESSION_KEY:-}" && -z "${ANTHROPIC_API_KEY:-}" ]]; then
  echo "Error: No Anthropic credentials found."
  echo "  - Store OAuth token: security add-generic-password -a zeroclaw -s anthropic-oauth-token -w 'sk-ant-oat01-...' -U"
  echo "  - Or log into claude.ai in Chrome for session key"
  echo "  - Or set ANTHROPIC_API_KEY"
  exit 1
fi

# ── Log which credential is active ─────────────────────────────────────
if [[ -n "${ANTHROPIC_OAUTH_TOKEN:-}" ]]; then
  echo "[run.sh] Using OAuth token (${ANTHROPIC_OAUTH_TOKEN:0:15}...)"
elif [[ -n "${CLAUDE_SESSION_KEY:-}" ]]; then
  echo "[run.sh] Using session key (${CLAUDE_SESSION_KEY:0:15}...) via proxy"
elif [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
  echo "[run.sh] Using API key"
fi

exec ./target/release/zeroclaw "$@"
