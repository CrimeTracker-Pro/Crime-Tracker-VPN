ALTER TABLE vpn_services
    ADD COLUMN allow_all_peers BOOLEAN NOT NULL DEFAULT FALSE;

-- Preserve the two host permissions that predate DB-backed management.
INSERT INTO vpn_services (
    id, server_id, name, description, protocol, gateway_ip, gateway_port,
    backend_ip, backend_port, enabled, allow_all_peers
)
SELECT gen_random_uuid(), id, 'SMB file storage',
       'Authenticated Samba shares on the Crime Tracker host',
       'tcp', '10.0.0.1', 445, '192.168.1.20', 445, TRUE, TRUE
FROM servers
WHERE is_active
ON CONFLICT (server_id, protocol, gateway_ip, gateway_port) DO NOTHING;

INSERT INTO vpn_services (
    id, server_id, name, description, protocol, gateway_ip, gateway_port,
    backend_ip, backend_port, enabled, allow_all_peers
)
SELECT gen_random_uuid(), id, 'Host SSH',
       'Administrator SSH access to the Crime Tracker host',
       'tcp', '10.0.0.1', 22, '192.168.1.20', 22, TRUE, FALSE
FROM servers
WHERE is_active
ON CONFLICT (server_id, protocol, gateway_ip, gateway_port) DO NOTHING;

INSERT INTO vpn_service_devices (service_id, device_id)
SELECT s.id, d.id
FROM vpn_services s
JOIN devices d ON d.server_id = s.server_id
WHERE s.name = 'Host SSH'
  AND d.allocated_ip = '10.0.0.3'
ON CONFLICT DO NOTHING;
