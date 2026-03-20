#!/bin/bash
# Login to Garmin Connect via sciotte credential login
# Usage: ./scripts/garmin-login.sh EMAIL PASSWORD [OTP_CODE]

set -euo pipefail

if [ $# -lt 2 ]; then
  echo "Usage: $0 EMAIL PASSWORD [OTP_CODE]"
  echo "  If OTP_CODE is provided, submits it directly"
  echo "  If omitted, runs credential login first"
  exit 1
fi

PORT="${SCIOTTE_PORT:-3001}"

if [ $# -eq 3 ]; then
  # Submit OTP code
  curl -s -X POST "http://localhost:${PORT}/auth/submit-otp" \
    -H "Content-Type: application/json" \
    -d "{\"code\": \"$3\"}" | jq .
else
  # Credential login
  EMAIL="$1"
  PASSWORD="$2"
  curl -s -X POST "http://localhost:${PORT}/auth/login-with-credentials" \
    -H "Content-Type: application/json" \
    -d "{\"email\": \"${EMAIL}\", \"password\": \"${PASSWORD}\", \"method\": \"email\"}" | jq .
fi
