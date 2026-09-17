-- Extra split-tunnel routes automatically included when a new device is provisioned.
-- The VPN server CIDR itself is always added by the API.
ALTER TABLE servers
    ADD COLUMN default_allowed_ips TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[];
