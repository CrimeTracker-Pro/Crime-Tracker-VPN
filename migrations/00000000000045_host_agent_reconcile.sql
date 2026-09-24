-- The database is the source of desired peers; the host journal is the
-- independently verified applied state. A pending row survives API restarts.
CREATE TABLE host_agent_reconcile (
    server_id UUID PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    desired_revision BIGINT NOT NULL DEFAULT 0 CHECK (desired_revision >= 0),
    desired_content_hash TEXT NOT NULL DEFAULT '',
    desired_digest TEXT NOT NULL DEFAULT '',
    applied_revision BIGINT NOT NULL DEFAULT 0 CHECK (applied_revision >= 0),
    applied_digest TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'applied', 'failed')),
    last_error TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- A reconciler holds this transaction-scoped lock across its snapshot and
-- external apply. Peer-affecting writes take the same lock *before* modifying
-- rows, so a commit cannot slip between snapshot and agent journal commit.
CREATE FUNCTION lock_host_agent_desired_state() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(862181921409::bigint);
    RETURN NULL;
END;
$$;

CREATE TRIGGER devices_host_agent_insert_lock
    BEFORE INSERT ON devices FOR EACH STATEMENT
    EXECUTE FUNCTION lock_host_agent_desired_state();
CREATE TRIGGER devices_host_agent_update_lock
    BEFORE UPDATE OF public_key, allocated_ip, status, server_id, preshared_key_encrypted
    ON devices FOR EACH STATEMENT
    EXECUTE FUNCTION lock_host_agent_desired_state();
CREATE TRIGGER devices_host_agent_delete_lock
    BEFORE DELETE ON devices FOR EACH STATEMENT
    EXECUTE FUNCTION lock_host_agent_desired_state();
CREATE TRIGGER users_host_agent_status_lock
    BEFORE UPDATE OF status ON users FOR EACH STATEMENT
    EXECUTE FUNCTION lock_host_agent_desired_state();
CREATE TRIGGER users_host_agent_delete_lock
    BEFORE DELETE ON users FOR EACH STATEMENT
    EXECUTE FUNCTION lock_host_agent_desired_state();
CREATE TRIGGER servers_host_agent_insert_lock
    BEFORE INSERT ON servers FOR EACH STATEMENT
    EXECUTE FUNCTION lock_host_agent_desired_state();
CREATE TRIGGER servers_host_agent_update_lock
    BEFORE UPDATE OF public_key, persistent_keepalive, is_active
    ON servers FOR EACH STATEMENT
    EXECUTE FUNCTION lock_host_agent_desired_state();
CREATE TRIGGER servers_host_agent_delete_lock
    BEFORE DELETE ON servers FOR EACH STATEMENT
    EXECUTE FUNCTION lock_host_agent_desired_state();
