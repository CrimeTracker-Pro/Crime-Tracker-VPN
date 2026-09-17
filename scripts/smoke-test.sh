#!/usr/bin/env bash
# End-to-end smoke test for the running stack.
# Assumes `make up` was run and the stack is healthy.
set -euo pipefail

BASE="${ZEROVPN_BASE:-http://localhost}"
PASS=0; FAIL=0
check() {
    local name="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "  ✓ $name"
        PASS=$((PASS+1))
    else
        echo "  ✗ $name"
        FAIL=$((FAIL+1))
    fi
}

contains() {
    local haystack="$1" needle="$2"
    [[ "$haystack" == *"$needle"* ]]
}

echo "Smoke test against $BASE"

echo "Reverse proxy (Traefik)"
check "proxy /healthz returns 200" curl -fsS "$BASE/healthz"
check "proxy /healthz body == ok" bash -c "[[ \"\$(curl -fsS $BASE/healthz)\" == 'ok' ]]"

echo "API"
check "api /api/v1/ping pong=true" bash -c "curl -fsS $BASE/api/v1/ping | grep -q '\"pong\":true'"

echo "Frontend"
check "frontend / returns HTML" bash -c "curl -fsSL $BASE/ | grep -qi '<html'"

echo "Containers"
check "db is healthy" bash -c "docker compose ps db --format '{{.Health}}' | grep -q healthy"
check "worker is up" bash -c "docker compose ps worker --format '{{.Status}}' | grep -q '^Up'"
check "api is up" bash -c "docker compose ps api --format '{{.Status}}' | grep -q '^Up'"

echo "Worker → API ZMQ"
check "worker is publishing heartbeats" bash -c "docker compose logs worker | grep -q 'events.heartbeat'"
check "api connected ZMQ subscriber" bash -c "docker compose logs api | grep -q 'zmq subscriber'"

# ---- auth + device flow -----------------------------------------------------
# Invitation-only Google sign-in needs a browser and a verified Google account.
# Public password endpoints and DNS controls must stay unavailable.
echo "Access controls"
check "registration unavailable" bash -c "[[ \$(curl -s -o /dev/null -w '%{http_code}' -X POST $BASE/api/v1/auth/register) == 404 ]]"
check "password login unavailable" bash -c "[[ \$(curl -s -o /dev/null -w '%{http_code}' -X POST $BASE/api/v1/auth/login) == 404 ]]"
check "DNS API unavailable" bash -c "[[ \$(curl -s -o /dev/null -w '%{http_code}' $BASE/api/v1/devices/dns-check) == 404 ]]"
check "unauthenticated /me denied" bash -c "[[ \$(curl -s -o /dev/null -w '%{http_code}' $BASE/api/v1/me) == 401 ]]"

printf '\nPassed: %s  Failed: %s\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
