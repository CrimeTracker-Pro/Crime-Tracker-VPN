-- The raw WireGuard counters and the device lifetime increment must commit
-- together. Existing lifetime totals are intentionally left untouched.
-- No old raw baseline can be reconstructed safely from historical aggregates.
CREATE TABLE peer_usage_checkpoints (
    device_id UUID PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    public_key TEXT NOT NULL,
    source_generation TEXT NOT NULL,
    counter_epoch BIGINT NOT NULL DEFAULT 0 CHECK (counter_epoch >= 0),
    raw_server_rx_bytes BIGINT NOT NULL CHECK (raw_server_rx_bytes >= 0),
    raw_server_tx_bytes BIGINT NOT NULL CHECK (raw_server_tx_bytes >= 0),
    sampled_at TIMESTAMPTZ NOT NULL
);

-- Durable host-perspective VPN traffic total for the sidebar. Initialize from
-- all existing device lifetime totals, including revoked rows still present.
-- Device RX is server TX, and device TX is server RX.
CREATE TABLE server_vpn_usage (
    server_id UUID PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    total_server_rx_bytes BIGINT NOT NULL DEFAULT 0 CHECK (total_server_rx_bytes >= 0),
    total_server_tx_bytes BIGINT NOT NULL DEFAULT 0 CHECK (total_server_tx_bytes >= 0)
);

INSERT INTO server_vpn_usage
    (server_id, total_server_rx_bytes, total_server_tx_bytes)
SELECT s.id, COALESCE(SUM(d.lifetime_tx_bytes), 0)::BIGINT,
       COALESCE(SUM(d.lifetime_rx_bytes), 0)::BIGINT
  FROM servers s
  LEFT JOIN devices d ON d.server_id = s.id
 GROUP BY s.id;
