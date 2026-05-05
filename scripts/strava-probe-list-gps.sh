#!/bin/bash
# Probe the running sciotte server for where Strava embeds activity GPS
# coordinates on the training-list and dashboard-feed pages. Pretty-prints
# JSON so we can see if `start_latlng` lives in row data-* attrs, inline
# <script> bodies, or window.__NEXT_DATA__ — and pick the cheapest scrape.
#
# Prereq: log in first via `./scripts/strava-google-login.sh` or
# `./scripts/strava-email-login.sh`.
#
# NOTE: SCIOTTE_PORT defaults to 3001, but `dravr-canot-claude-channel`
# also defaults to 3001 — if you're running this from a Claude Code
# worktree, start the sciotte server on a free port (e.g. 3010) and
# export `SCIOTTE_PORT=3010` for both this and the login script.
#
# Usage:
#   ./scripts/strava-probe-list-gps.sh                    # latest session
#   ./scripts/strava-probe-list-gps.sh SESSION_ID         # specific session

set -euo pipefail

PORT="${SCIOTTE_PORT:-3001}"
BASE="http://localhost:${PORT}"

if [ $# -eq 1 ]; then
  HEADERS=(-H "X-Session-Id: $1")
else
  HEADERS=()
fi

curl -s "${HEADERS[@]}" "${BASE}/debug/list-page-probe" | jq .
