CREATE TYPE vpn_transport_protocol AS ENUM ('tcp', 'udp');
CREATE TYPE vpn_policy_apply_status AS ENUM ('pending', 'validated', 'applied', 'failed', 'rolled_back');
CREATE TYPE vpn_policy_decision AS ENUM ('allowed', 'denied');

CREATE TABLE vpn_services (
    id UUID PRIMARY KEY,
    server_id UUID NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    protocol vpn_transport_protocol NOT NULL,
    gateway_ip INET NOT NULL,
    gateway_port INTEGER NOT NULL CHECK (gateway_port BETWEEN 1 AND 65535),
    backend_ip INET NOT NULL,
    backend_port INTEGER NOT NULL CHECK (backend_port BETWEEN 1 AND 65535),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (server_id, protocol, gateway_ip, gateway_port),
    UNIQUE (server_id, name)
);

CREATE TABLE vpn_access_groups (
    id UUID PRIMARY KEY,
    server_id UUID NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (server_id, name)
);

CREATE TABLE vpn_group_users (
    group_id UUID NOT NULL REFERENCES vpn_access_groups(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, user_id)
);

CREATE TABLE vpn_group_devices (
    group_id UUID NOT NULL REFERENCES vpn_access_groups(id) ON DELETE CASCADE,
    device_id UUID NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, device_id)
);

CREATE TABLE vpn_service_users (
    service_id UUID NOT NULL REFERENCES vpn_services(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (service_id, user_id)
);

CREATE TABLE vpn_service_devices (
    service_id UUID NOT NULL REFERENCES vpn_services(id) ON DELETE CASCADE,
    device_id UUID NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    PRIMARY KEY (service_id, device_id)
);

CREATE TABLE vpn_service_groups (
    service_id UUID NOT NULL REFERENCES vpn_services(id) ON DELETE CASCADE,
    group_id UUID NOT NULL REFERENCES vpn_access_groups(id) ON DELETE CASCADE,
    PRIMARY KEY (service_id, group_id)
);

CREATE TABLE vpn_policy_revisions (
    id UUID PRIMARY KEY,
    server_id UUID NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    revision_number BIGINT NOT NULL,
    desired_snapshot JSONB NOT NULL,
    compiled_ruleset TEXT NOT NULL,
    checksum TEXT NOT NULL,
    status vpn_policy_apply_status NOT NULL DEFAULT 'pending',
    apply_error TEXT,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    applied_at TIMESTAMPTZ,
    superseded_at TIMESTAMPTZ,
    UNIQUE (server_id, revision_number)
);

CREATE TABLE vpn_policy_runtime (
    server_id UUID PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    desired_generation BIGINT NOT NULL DEFAULT 0,
    applied_generation BIGINT NOT NULL DEFAULT 0,
    active_revision_id UUID REFERENCES vpn_policy_revisions(id) ON DELETE SET NULL,
    last_good_revision_id UUID REFERENCES vpn_policy_revisions(id) ON DELETE SET NULL,
    requested_revision_id UUID REFERENCES vpn_policy_revisions(id) ON DELETE SET NULL,
    reconciler_mode TEXT NOT NULL DEFAULT 'shadow' CHECK (reconciler_mode IN ('shadow', 'enforce')),
    healthy BOOLEAN NOT NULL DEFAULT FALSE,
    drift_detected BOOLEAN NOT NULL DEFAULT FALSE,
    active_checksum TEXT,
    last_error TEXT,
    last_reconciled_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO vpn_policy_runtime (server_id)
SELECT id FROM servers ON CONFLICT DO NOTHING;

CREATE TABLE vpn_policy_connection_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    server_id UUID NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    device_id UUID REFERENCES devices(id) ON DELETE SET NULL,
    service_id UUID REFERENCES vpn_services(id) ON DELETE SET NULL,
    source_ip INET NOT NULL,
    destination_ip INET NOT NULL,
    destination_port INTEGER,
    protocol TEXT NOT NULL,
    decision vpn_policy_decision NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX vpn_policy_connection_events_created_idx
    ON vpn_policy_connection_events (created_at DESC);
CREATE INDEX vpn_policy_connection_events_device_idx
    ON vpn_policy_connection_events (device_id, created_at DESC);
