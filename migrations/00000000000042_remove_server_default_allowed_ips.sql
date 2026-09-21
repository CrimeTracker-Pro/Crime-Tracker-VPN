-- Return peer profiles to the standard split tunnel: only the server VPN CIDR.
-- Keep the legacy column for migration compatibility, but disable its behavior.
UPDATE servers SET default_allowed_ips = ARRAY[]::TEXT[];

UPDATE devices d
SET allowed_ips_override = ARRAY[s.cidr::TEXT]
FROM servers s
WHERE s.id = d.server_id;
