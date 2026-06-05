#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Verify the dravr-browser capture hook still feeds activity extraction after the
# SSOT consolidation. The stealth hook now writes captured API responses to
# `window.__dravrCaptures` (renamed from `__sciotteApiCaptures`), and each
# provider's `js_extract` reads that map. This scrapes a few activities end-to-end
# through that path: a non-empty result proves the renamed capture global works.
#
# A browser window opens for login if there's no saved session — complete it.
#
# Usage:
#   ./scripts/verify-capture.sh [garmin|strava] [limit]
#   ./scripts/verify-capture.sh garmin 5
#
# Notes:
#   - Garmin exercises the capture hook hardest (activity-service / workout-service
#     / gc-api responses are read from __dravrCaptures), so it's the best check.
#   - Strava also reads __dravrCaptures; use it if that's the account you have.

set -euo pipefail
cd "$(dirname "$0")/.."

PROVIDER="${1:-garmin}"
LIMIT="${2:-5}"
TOML="providers/${PROVIDER}.toml"

if [ ! -f "$TOML" ]; then
  echo "❌ no provider config at $TOML (expected garmin or strava)" >&2
  exit 1
fi

# sciotte's backpressure limiter requires these env knobs (no built-in defaults;
# normally supplied via .envrc/Terraform). Set sane single-user values for a
# one-off local scrape if the operator hasn't already exported them.
: "${DRAVR_SCIOTTE_MAX_CONCURRENT:=2}"
: "${DRAVR_SCIOTTE_MAX_QUEUE:=8}"
: "${DRAVR_SCIOTTE_QUEUE_TIMEOUT_SECS:=30}"
: "${DRAVR_SCIOTTE_PARKED_PERMIT_TTL_SECS:=300}"
: "${DRAVR_SCIOTTE_WATCHDOG_INTERVAL_SECS:=30}"
: "${DRAVR_SCIOTTE_RETRY_AFTER_HINT_SECS:=5}"
: "${DRAVR_SCIOTTE_CLOSED_RETRY_AFTER_SECS:=5}"
export DRAVR_SCIOTTE_MAX_CONCURRENT DRAVR_SCIOTTE_MAX_QUEUE DRAVR_SCIOTTE_QUEUE_TIMEOUT_SECS \
       DRAVR_SCIOTTE_PARKED_PERMIT_TTL_SECS DRAVR_SCIOTTE_WATCHDOG_INTERVAL_SECS \
       DRAVR_SCIOTTE_RETRY_AFTER_HINT_SECS DRAVR_SCIOTTE_CLOSED_RETRY_AFTER_SECS

# Quiet chromiumoxide's harmless "WS Invalid message" spam so the JSON is readable.
export RUST_LOG="${RUST_LOG:-info,chromiumoxide=error}"

echo "==> Building dravr-sciotte-server (feature/dravr-browser)…"
cargo build -q -p dravr-sciotte-server

# Force a fresh browser login by default so a stale/expired saved session can't
# bounce the headless scrape to the login page. Set RELOGIN=0 to reuse a valid
# saved session on repeat runs (faster — no browser window).
RELOGIN="${RELOGIN:-1}"
LOGIN_FLAG=""
[ "$RELOGIN" = "1" ] && LOGIN_FLAG="--login"

echo "==> Scraping ${LIMIT} ${PROVIDER} activities via the __dravrCaptures path…"
if [ -n "$LOGIN_FLAG" ]; then
  echo "    (a browser window opens — sign in to ${PROVIDER}; set RELOGIN=0 to reuse a saved session)"
fi
# Server logs and the activities JSON share the same stream; extract just the
# JSON array (from the first line starting with '[' to the end).
OUT="$(cargo run -q -p dravr-sciotte-server -- --provider "$TOML" activities --limit "$LIMIT" --format json $LOGIN_FLAG 2>&1)"
JSON="$(printf '%s\n' "$OUT" | awk '/^\[/{f=1} f{print}')"

echo "----- activities JSON -----"
printf '%s\n' "$JSON" | jq . 2>/dev/null || printf '%s\n' "${JSON:-<none>}"
echo "---------------------------"

COUNT="$(printf '%s' "$JSON" | jq 'if type=="array" then length elif .activities then (.activities|length) else 0 end' 2>/dev/null || echo 0)"

if [ "${COUNT:-0}" -gt 0 ]; then
  echo "✅ PASS: extracted ${COUNT} activities — capture hook (window.__dravrCaptures) works."
else
  echo "❌ FAIL: 0 activities extracted — the __dravrCaptures rename may be broken." >&2
  echo "   (If login didn't complete, re-run; otherwise inspect providers/${PROVIDER}.toml js_extract.)" >&2
  exit 1
fi
