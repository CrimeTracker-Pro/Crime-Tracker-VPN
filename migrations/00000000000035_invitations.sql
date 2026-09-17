-- Invitation state is separate from users so acceptance, resend and revoke
-- remain auditable. Existing pending accounts retain their unexpired links.
CREATE TABLE invitations (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    invited_by UUID REFERENCES users(id) ON DELETE SET NULL,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    verified_at TIMESTAMPTZ,
    accepted_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX invitations_pending_idx ON invitations (expires_at)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;
INSERT INTO invitations (id, user_id, token_hash, expires_at, created_at)
SELECT vt.id, vt.user_id, vt.token_hash, vt.expires_at, vt.created_at
  FROM verification_tokens vt
 WHERE vt.purpose = 'email_verify' AND vt.consumed_at IS NULL AND vt.expires_at > NOW()
   AND EXISTS (SELECT 1 FROM users u WHERE u.id = vt.user_id AND u.status = 'pending_verification')
ON CONFLICT (user_id) DO NOTHING;
