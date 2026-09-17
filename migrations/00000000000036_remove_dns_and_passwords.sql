-- User requested removal of VPN-managed DNS. Existing device/server records
-- remain intact; only DNS configuration columns are removed.
ALTER TABLE devices DROP COLUMN dns_names;
ALTER TABLE devices DROP COLUMN dns_override;
ALTER TABLE servers DROP COLUMN dns_servers;

-- Password sign-in is no longer routed. Invalidate old hashes so restoring an
-- older API image cannot accidentally reopen password authentication.
UPDATE users SET password_hash = '!', must_change_password = FALSE;
