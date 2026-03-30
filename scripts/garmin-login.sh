#!/bin/bash
# Login to Garmin Connect via sciotte credential login
# Handles the full multi-step flow: login → OTP / 2FA
#
# Usage: ./scripts/garmin-login.sh EMAIL PASSWORD

set -euo pipefail

PORT="${SCIOTTE_PORT:-3001}"
BASE="http://localhost:${PORT}"

if [ $# -ne 2 ]; then
  echo "Usage: $0 EMAIL PASSWORD"
  exit 1
fi

EMAIL="$1"
PASSWORD="$2"

# Step 1: Credential login
echo "Logging in to Garmin Connect..."
RESULT=$(curl -s -X POST "${BASE}/auth/login-with-credentials" \
  -H "Content-Type: application/json" \
  -d "{\"email\": \"${EMAIL}\", \"password\": \"${PASSWORD}\", \"method\": \"email\"}")

STATUS=$(echo "$RESULT" | jq -r '.status')

# Step 2: Handle 2FA choice if needed
if [ "$STATUS" = "two_factor_choice" ]; then
  echo ""
  echo "2FA verification required. Available methods:"
  echo "$RESULT" | jq -r '.options[] | "  [\(.id)] \(.label)"'
  echo ""
  read -p "Select method (e.g., app, otp): " METHOD

  echo "Selecting ${METHOD}..."
  RESULT=$(curl -s -X POST "${BASE}/auth/select-2fa" \
    -H "Content-Type: application/json" \
    -d "{\"option_id\": \"${METHOD}\"}")

  STATUS=$(echo "$RESULT" | jq -r '.status')
fi

# Step 3: Handle phone tap (number match)
if [ "$STATUS" = "number_match" ]; then
  NUMBER=$(echo "$RESULT" | jq -r '.number')
  echo ""
  echo ">>> Tap your phone to approve. ${NUMBER} <<<"
  echo ""
  read -p "Press Enter after approving on your phone..."

  echo "Polling for approval..."
  RESULT=$(curl -s -X POST "${BASE}/auth/select-2fa" \
    -H "Content-Type: application/json" \
    -d '{"option_id": "poll"}')

  STATUS=$(echo "$RESULT" | jq -r '.status')
fi

# Step 4: Handle OTP code entry
if [ "$STATUS" = "otp_required" ]; then
  echo ""
  read -p "Enter verification code: " CODE

  echo "Submitting code..."
  RESULT=$(curl -s -X POST "${BASE}/auth/submit-otp" \
    -H "Content-Type: application/json" \
    -d "{\"code\": \"${CODE}\"}")

  STATUS=$(echo "$RESULT" | jq -r '.status')
fi

# Final result
echo "$RESULT" | jq .

if [ "$STATUS" = "authenticated" ]; then
  echo "Login successful!"
else
  echo "Login failed with status: ${STATUS}" >&2
  exit 1
fi
