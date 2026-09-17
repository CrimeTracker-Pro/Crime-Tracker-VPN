#!/usr/bin/env bash
# Run a command with the dev environment rewritten for native (non-docker)
# execution. Reads `.env` as the source of truth, then rewrites container
# hostnames (db, worker) to their localhost port mappings
# from docker-compose.yml.
#
# Usage:
#   ./scripts/dev-native.sh cargo run -p zerovpn-api
#   ./scripts/dev-native.sh cargo run -p zerovpn-worker
#   ./scripts/dev-native.sh env | grep ZEROVPN_       # inspect resolved values
#
# Requires `make dev` to be running (db up; api +
# worker stopped).
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ ! -f .env ]]; then
    echo "ERROR: .env not found. Run: make setup" >&2
    exit 1
fi

# Parse .env as KEY=VALUE without shell expansion. We can't `source` it
# because values like `Crime Tracker VPN <noreply@localhost>` contain `<`, which bash
# would interpret as a redirect. Docker compose parses .env this way already.
while IFS= read -r line || [[ -n "$line" ]]; do
    [[ -z "${line// }" ]] && continue
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    key="${line%%=*}"
    value="${line#*=}"
    [[ "$key" == "$line" ]] && continue
    if [[ "$value" =~ ^\".*\"$ ]] || [[ "$value" =~ ^\'.*\'$ ]]; then
        value="${value:1:${#value}-2}"
    fi
    export "$key=$value"
done < .env

# Container hostnames → host localhost ports (matches docker-compose.yml).
export ZEROVPN_DATABASE_URL="${ZEROVPN_DATABASE_URL/@db:5432/@localhost:55432}"
export ZEROVPN_SMTP__HOST=localhost
export ZEROVPN_SMTP__PORT=1025

# ZMQ: native worker binds on 127.0.0.1:5555; native api connects there.
export ZEROVPN_EVENTS__PUBLISHER_BIND="tcp://127.0.0.1:5555"
export ZEROVPN_EVENTS__SUBSCRIBER_CONNECT="tcp://127.0.0.1:5555"

# Bind only to loopback when running natively.
export ZEROVPN_BIND_ADDRESS="127.0.0.1:8080"

# Static paths the dockerized api would write into the wg_config volume —
# in native mode point them at a workspace-local dir we control.
mkdir -p .dev-native/wg
export ZEROVPN_WG__SERVER_CONFIG_PATH="$PWD/.dev-native/wg/wg0.conf"

# Peer configs embed the server's `Endpoint`, which other devices on the
# network must be able to reach — `localhost` only works from this Mac. When
# the configured endpoint is localhost/empty, substitute the host's current
# Wi-Fi/LAN IP (en0, then en1) so e.g. a phone on the same network can connect.
# Set ZEROVPN_WG__SERVER_ENDPOINT explicitly in .env to override.
endpoint_host="${ZEROVPN_WG__SERVER_ENDPOINT:-localhost}"
endpoint_host="${endpoint_host%%:*}"
if [[ -z "$endpoint_host" || "$endpoint_host" == "localhost" || "$endpoint_host" == "127.0.0.1" ]]; then
    lan_ip="$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || true)"
    if [[ -n "$lan_ip" ]]; then
        export ZEROVPN_WG__SERVER_ENDPOINT="${lan_ip}:${ZEROVPN_WG__LISTEN_PORT:-51820}"
        echo "dev-native: WG endpoint -> ${ZEROVPN_WG__SERVER_ENDPOINT}" >&2
    fi
fi

if [[ $# -eq 0 ]]; then
    echo "Usage: $0 <command> [args...]" >&2
    exit 1
fi

exec "$@"
