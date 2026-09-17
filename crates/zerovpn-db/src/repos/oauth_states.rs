//! Short-lived OAuth `state` + PKCE verifier storage.
//!
//! `state` is the unguessable nonce we hand to the OAuth provider and
//! check on the callback to prove the response belongs to a redirect we
//! actually initiated. The PKCE `code_verifier` rides alongside it so we
//! can complete the token exchange without keeping the verifier in a
//! cookie (which would either need separate signing or be readable by
//! anything sharing the origin).
//!
//! Stored hashed (sha256, hex) — the same pattern as invitation tokens —
//! so a DB leak doesn't hand an attacker live state values. TTL is short
//! (a few minutes) because the OAuth round-trip is synchronous;
//! `consume` performs the validate-then-delete in one statement to make
//! state values strictly single-use.

use time::OffsetDateTime;

use crate::PgPool;

/// Park a state hash + PKCE verifier with an expiry. Hash is the sha256
/// of the plaintext state in lowercase hex; expiry is an absolute
/// timestamp (typically `now + 10 min`).
pub async fn insert(
    pool: &PgPool,
    state_hash: &str,
    pkce_verifier: &str,
    expires_at: OffsetDateTime,
) -> sqlx::Result<()> {
    sqlx::query(
        r#"INSERT INTO oauth_states (state_hash, pkce_verifier, expires_at)
           VALUES ($1, $2, $3)"#,
    )
    .bind(state_hash)
    .bind(pkce_verifier)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Validate + consume a state in one statement. Returns the PKCE verifier
/// on success; `None` when the row doesn't exist, was already consumed,
/// or has expired — the OAuth handler treats all three identically as
/// "bad state, refuse callback".
pub async fn consume(pool: &PgPool, state_hash: &str) -> sqlx::Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        r#"DELETE FROM oauth_states
            WHERE state_hash = $1 AND expires_at > NOW()
        RETURNING pkce_verifier"#,
    )
    .bind(state_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}
