-- Invitations supersede self-registration verification and password reset.
-- Migration 35 copied old email-verification links for continuity; invalidate
-- those uninvited links before the new access policy goes live.
DELETE FROM invitations WHERE invited_by IS NULL AND accepted_at IS NULL;
UPDATE users SET google_id = NULL WHERE status = 'pending_verification';
DROP TABLE verification_tokens;
