#!/usr/bin/env bash
# Build and deploy the production stack while preserving the api/worker
# network-namespace invariant. The worker uses `network_mode: service:api`;
# whenever Docker replaces api, worker must be recreated only after the new api
# container exists or it can remain attached to the deleted namespace.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

COMPOSE=(docker compose -f docker-compose.yml -f docker-compose.build.yml)

echo "Building and starting the production stack..."
"${COMPOSE[@]}" up -d --build

echo "Reattaching worker to the current api network namespace..."
"${COMPOSE[@]}" up -d --no-deps --force-recreate worker

echo "Verifying api/worker runtime..."
deadline=$((SECONDS + 45))
while (( SECONDS < deadline )); do
    api_id="$(docker inspect -f '{{.Id}}' crimetracker-vpn-api 2>/dev/null || true)"
    worker_mode="$(docker inspect -f '{{.HostConfig.NetworkMode}}' crimetracker-vpn-worker 2>/dev/null || true)"

    namespace_ok=false
    publisher_ok=false
    wireguard_ok=false
    poller_ok=false
    subscriber_ok=false

    [[ -n "$api_id" && "$worker_mode" == "container:$api_id" ]] && namespace_ok=true
    docker exec crimetracker-vpn-api sh -c "ss -ltn | grep -q ':5555 '" 2>/dev/null && publisher_ok=true
    docker exec crimetracker-vpn-api wg show wg0 2>/dev/null | grep -q '^interface: wg0' && wireguard_ok=true
    # Do not use grep -q here: with pipefail it can close the pipe early and
    # turn Compose's SIGPIPE into a false-negative pipeline status.
    "${COMPOSE[@]}" logs worker 2>/dev/null | grep '"message":"wg poll"' >/dev/null && poller_ok=true
    "${COMPOSE[@]}" logs api 2>/dev/null | grep -E '"message":"(zmq subscriber connected|event received)"' >/dev/null && subscriber_ok=true

    if $namespace_ok && $publisher_ok && $wireguard_ok && $poller_ok && $subscriber_ok; then
        echo "Production runtime verified: worker namespace, ZMQ, WireGuard, DB polling, and subscriber are healthy."
        exit 0
    fi
    sleep 2
done

echo "Production runtime verification failed." >&2
echo "  api id:              ${api_id:-missing}" >&2
echo "  worker network mode: ${worker_mode:-missing}" >&2
echo "  namespace match:     $namespace_ok" >&2
echo "  publisher :5555:     $publisher_ok" >&2
echo "  WireGuard wg0:       $wireguard_ok" >&2
echo "  worker DB poll:      $poller_ok" >&2
echo "  API subscriber:      $subscriber_ok" >&2
exit 1
