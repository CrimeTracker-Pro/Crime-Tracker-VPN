-- Invalidate sessions established through retired password endpoints.
-- The auth extractor compares this watermark with the session snapshot.
UPDATE users SET password_changed_at = NOW();
