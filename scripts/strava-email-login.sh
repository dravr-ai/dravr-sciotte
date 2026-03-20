#!/bin/bash
# Login to Strava with email/password (direct, no OAuth)
# Usage:
#   ./scripts/strava-email-login.sh EMAIL PASSWORD        — start login
#   ./scripts/strava-email-login.sh otp CODE              — submit OTP code

set -euo pipefail

PORT="${SCIOTTE_PORT:-3001}"

case "${1:-}" in
  otp)
    if [ $# -ne 2 ]; then
      echo "Usage: $0 otp CODE"
      exit 1
    fi
    curl -s -X POST "http://localhost:${PORT}/auth/submit-otp" \
      -H "Content-Type: application/json" \
      -d "{\"code\": \"$2\"}" | jq .
    ;;
  *)
    if [ $# -ne 2 ]; then
      echo "Usage: $0 EMAIL PASSWORD"
      echo "       $0 otp CODE"
      exit 1
    fi
    curl -s -X POST "http://localhost:${PORT}/auth/login-with-credentials" \
      -H "Content-Type: application/json" \
      -d "{\"email\": \"$1\", \"password\": \"$2\", \"method\": \"email\"}" | jq .
    ;;
esac
