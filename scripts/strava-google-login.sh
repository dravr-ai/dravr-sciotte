#!/bin/bash
# Login to Strava via Google OAuth using sciotte credential login
# Usage:
#   ./scripts/strava-google-login.sh EMAIL PASSWORD        — start login
#   ./scripts/strava-google-login.sh select OPTION_ID      — select 2FA method
#   ./scripts/strava-google-login.sh otp CODE              — submit OTP code

set -euo pipefail

PORT="${SCIOTTE_PORT:-3001}"

case "${1:-}" in
  select)
    if [ $# -ne 2 ]; then
      echo "Usage: $0 select OPTION_ID"
      exit 1
    fi
    curl -s -X POST "http://localhost:${PORT}/auth/select-2fa" \
      -H "Content-Type: application/json" \
      -d "{\"option_id\": \"$2\"}" | jq .
    ;;
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
      echo "       $0 select OPTION_ID"
      echo "       $0 otp CODE"
      exit 1
    fi
    curl -s -X POST "http://localhost:${PORT}/auth/login-with-credentials" \
      -H "Content-Type: application/json" \
      -d "{\"email\": \"$1\", \"password\": \"$2\", \"method\": \"google\"}" | jq .
    ;;
esac
