#!/usr/bin/env bash
# Usage: ./scripts/get_token.sh [email] [password]
# Prints a Supabase JWT access_token for use in API requests.
# Credentials are read from .env (SUPABASE_ANON_KEY, TEST_EMAIL, TEST_PASSWORD)
# or passed as arguments.
#
# Example:
#   TOKEN=$(./scripts/get_token.sh)
#   curl -H "Authorization: Bearer $TOKEN" http://localhost:8080/accounts

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$SCRIPT_DIR/../.env"

if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$ENV_FILE"
  set +a
fi

EMAIL="${1:-${TEST_EMAIL:?TEST_EMAIL is not set}}"
PASSWORD="${2:-${TEST_PASSWORD:?TEST_PASSWORD is not set}}"
ANON_KEY="${SUPABASE_ANON_KEY:?SUPABASE_ANON_KEY is not set}"
SUPABASE_URL="${SUPABASE_URL:?SUPABASE_URL is not set}"

curl -s -X POST "${SUPABASE_URL}/auth/v1/token?grant_type=password" \
  -H "apikey: ${ANON_KEY}" \
  -H "Authorization: Bearer ${ANON_KEY}" \
  -H "Content-Type: application/json" \
  -d "{\"email\": \"${EMAIL}\", \"password\": \"${PASSWORD}\"}"
