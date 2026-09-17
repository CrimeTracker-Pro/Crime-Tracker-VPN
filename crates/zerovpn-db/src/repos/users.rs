use time::OffsetDateTime;
use uuid::Uuid;
use zerovpn_core::models::{User, UserRole, UserStatus};

use crate::PgPool;

pub async fn find_by_email(pool: &PgPool, email: &str) -> sqlx::Result<Option<UserWithSecrets>> {
    sqlx::query_as::<_, UserWithSecrets>(
        r#"SELECT id, email::TEXT AS email, password_hash, role, status,
                  must_change_password, email_verified_at, totp_enabled,
                  created_at, last_login_at, password_changed_at
           FROM users
           WHERE email = $1::CITEXT AND deleted_at IS NULL"#,
    )
    .bind(email)
    .fetch_optional(pool)
    .await
}

/// Fetch the session-revocation watermark so the caller can refresh its own
/// snapshot after revoking all other sessions.
pub async fn find_password_changed_at(
    pool: &PgPool,
    id: Uuid,
) -> sqlx::Result<Option<OffsetDateTime>> {
    let row: Option<(OffsetDateTime,)> = sqlx::query_as(
        "SELECT password_changed_at FROM users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<User>> {
    sqlx::query_as::<_, User>(
        r#"SELECT id, email::TEXT AS email, role, status, must_change_password,
                  email_verified_at, totp_enabled, created_at, last_login_at,
                  password_changed_at
           FROM users
           WHERE id = $1 AND deleted_at IS NULL"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Bump `password_changed_at` without touching the password hash. The
/// watermark is what the auth extractor checks against the snapshot
/// embedded in the session — bumping it invalidates every outstanding
/// session for this user without us needing to crawl the opaque
/// tower_sessions blob storage. Used for "sign out everywhere",
/// admin-triggered force-logout, and as part of suspend / delete so
/// the user's cookie dies the moment status changes.
pub async fn kill_all_sessions(pool: &PgPool, id: Uuid) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE users SET password_changed_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Admin override: change a user's email. Returns 0 rows when the user
/// doesn't exist or is deleted. Caller is responsible for normalising
/// the input (trim + lowercase) and checking uniqueness up-front, since
/// the underlying CITEXT column will surface a generic
/// `unique_violation` otherwise.
pub async fn admin_set_email(pool: &PgPool, id: Uuid, new_email: &str) -> sqlx::Result<u64> {
    let res = sqlx::query(
        r#"UPDATE users
              SET email = $2::CITEXT
            WHERE id = $1 AND deleted_at IS NULL"#,
    )
    .bind(id)
    .bind(new_email)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

pub async fn create(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
    role: UserRole,
    status: UserStatus,
) -> sqlx::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO users (id, email, password_hash, role, status)
           VALUES ($1, $2::CITEXT, $3, $4, $5)"#,
    )
    .bind(id)
    .bind(email)
    .bind(password_hash)
    .bind(role)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Look up a user by their Google `sub` claim (linked at first OAuth sign-in
/// or by `link_google` after an email match). Mirrors `find_by_email` —
/// returns the full secrets row so the OAuth callback can run the same
/// post-login plumbing (status check, session watermark) as the Google callback.
pub async fn find_by_google_id(
    pool: &PgPool,
    google_id: &str,
) -> sqlx::Result<Option<UserWithSecrets>> {
    sqlx::query_as::<_, UserWithSecrets>(
        r#"SELECT id, email::TEXT AS email, password_hash, role, status,
                  must_change_password, email_verified_at, totp_enabled,
                  created_at, last_login_at, password_changed_at
             FROM users
            WHERE google_id = $1 AND deleted_at IS NULL"#,
    )
    .bind(google_id)
    .fetch_optional(pool)
    .await
}

/// Attach a Google `sub` to an existing invited or bootstrapped user. Called on
/// first OAuth sign-in when the Google email matches an existing account —
/// Google has already verified the email, so we treat that as proof of
/// ownership and link silently. Idempotent: linking the same google_id
/// again is a no-op.
pub async fn link_google(pool: &PgPool, id: Uuid, google_id: &str) -> sqlx::Result<u64> {
    let res = sqlx::query(
        r#"UPDATE users
              SET google_id = $2,
                  email_verified_at = COALESCE(email_verified_at, NOW())
            WHERE id = $1 AND deleted_at IS NULL AND google_id IS NULL"#,
    )
    .bind(id)
    .bind(google_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

pub async fn touch_last_login(pool: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET last_login_at = $2 WHERE id = $1")
        .bind(id)
        .bind(OffsetDateTime::now_utc())
        .execute(pool)
        .await?;
    Ok(())
}

/// Atomically swap the user's last-login IP and return the value that
/// was previously stored. Uses a CTE to capture the OLD value before the
/// UPDATE replaces it, so the caller can compare without a read-then-write
/// race. Renamed from `swap_last_login_ip_prefix` in migration 20 — the
/// column now holds full `/32`/`/128` host networks, not /24 prefixes.
pub async fn swap_last_login_ip(
    pool: &PgPool,
    id: Uuid,
    new_ip: ipnetwork::IpNetwork,
) -> sqlx::Result<Option<ipnetwork::IpNetwork>> {
    let row: Option<(Option<ipnetwork::IpNetwork>,)> = sqlx::query_as(
        r#"WITH old AS (
               SELECT last_login_ip FROM users WHERE id = $1 FOR UPDATE
           ),
           upd AS (
               UPDATE users SET last_login_ip = $2 WHERE id = $1
           )
           SELECT last_login_ip FROM old"#,
    )
    .bind(id)
    .bind(new_ip)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.0))
}

/// Increment a user's monthly bandwidth counter. Resets the counter at the
/// start of a new month. Returns the new total + cap; caller decides whether
/// to enforce.
pub async fn add_monthly_usage(
    pool: &PgPool,
    user_id: Uuid,
    delta_bytes: i64,
) -> sqlx::Result<(i64, Option<i64>)> {
    let now = OffsetDateTime::now_utc();
    let next_reset = first_of_next_month(now);

    let row: Option<(i64, Option<i64>)> = sqlx::query_as(
        r#"UPDATE users
              SET current_month_bytes = CASE
                    WHEN quota_resets_at IS NULL OR quota_resets_at < $2
                      THEN $3
                    ELSE current_month_bytes + $3
                  END,
                  quota_resets_at = CASE
                    WHEN quota_resets_at IS NULL OR quota_resets_at < $2
                      THEN $4
                    ELSE quota_resets_at
                  END
            WHERE id = $1 AND deleted_at IS NULL
        RETURNING current_month_bytes, monthly_byte_cap"#,
    )
    .bind(user_id)
    .bind(now)
    .bind(delta_bytes)
    .bind(next_reset)
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or((0, None)))
}

/// Inputs the dashboard "Quota" card needs: `(monthly_byte_cap, baseline,
/// baseline_at)` — the account cap plus the lifetime-usage snapshot taken at
/// the start of the current cycle. Monthly usage is then derived as
/// `(lifetime now) − baseline` by the caller (see the `/me/usage` handler).
pub async fn month_quota(
    pool: &PgPool,
    id: Uuid,
) -> sqlx::Result<Option<(Option<i64>, i64, Option<OffsetDateTime>)>> {
    sqlx::query_as(
        r#"SELECT monthly_byte_cap, month_baseline_bytes, month_baseline_at
             FROM users
            WHERE id = $1 AND deleted_at IS NULL"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Re-snapshot the monthly usage baseline (lifetime total at cycle start) and
/// stamp which cycle it belongs to. Called lazily when a new month is detected.
pub async fn set_month_baseline(
    pool: &PgPool,
    id: Uuid,
    baseline_bytes: i64,
    cycle_start: OffsetDateTime,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        r#"UPDATE users
              SET month_baseline_bytes = $2, month_baseline_at = $3
            WHERE id = $1"#,
    )
    .bind(id)
    .bind(baseline_bytes)
    .bind(cycle_start)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// First-of-this-month at midnight UTC — the start of the current monthly
/// quota cycle. The cycle runs `[first_of_month, first_of_next_month)`.
pub fn first_of_month(t: OffsetDateTime) -> OffsetDateTime {
    let date =
        time::Date::from_calendar_date(t.year(), t.month(), 1).expect("valid first-of-month");
    OffsetDateTime::new_utc(date, time::Time::MIDNIGHT)
}

/// First-of-next-month at midnight UTC — the monthly quota reset boundary,
/// shared by the per-user counter here, the per-device counter, and the
/// quota-enforcement sweep so every reset lands on the same instant.
pub fn first_of_next_month(t: OffsetDateTime) -> OffsetDateTime {
    let (y, m, _d) = (t.year(), t.month(), t.day());
    let (ny, nm) = if m == time::Month::December {
        (y + 1, time::Month::January)
    } else {
        (y, m.next())
    };
    let date = time::Date::from_calendar_date(ny, nm, 1).expect("valid first-of-month");
    OffsetDateTime::new_utc(date, time::Time::MIDNIGHT)
}

pub async fn count_active_admins(pool: &PgPool) -> sqlx::Result<i64> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM users WHERE role = 'admin' AND status = 'active' AND deleted_at IS NULL",
    )
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Persist enrolled TOTP material (encrypted secret + hashed recovery codes).
pub async fn enable_totp(
    pool: &PgPool,
    user_id: Uuid,
    secret_encrypted: &[u8],
    recovery_hashes: &[String],
) -> sqlx::Result<()> {
    sqlx::query(
        r#"UPDATE users
              SET totp_enabled = TRUE,
                  totp_secret_encrypted = $2,
                  totp_recovery_codes_hashed = $3
            WHERE id = $1"#,
    )
    .bind(user_id)
    .bind(secret_encrypted)
    .bind(recovery_hashes)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn disable_totp(pool: &PgPool, user_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        r#"UPDATE users
              SET totp_enabled = FALSE,
                  totp_secret_encrypted = NULL,
                  totp_recovery_codes_hashed = NULL
            WHERE id = $1"#,
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_totp_material(
    pool: &PgPool,
    user_id: Uuid,
) -> sqlx::Result<Option<(Vec<u8>, Vec<String>)>> {
    type TotpRow = (Option<Vec<u8>>, Option<Vec<String>>);
    let row: Option<TotpRow> = sqlx::query_as(
        "SELECT totp_secret_encrypted, totp_recovery_codes_hashed FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(secret, codes)| match (secret, codes) {
        (Some(s), Some(c)) => Some((s, c)),
        _ => None,
    }))
}

pub async fn replace_recovery_codes(
    pool: &PgPool,
    user_id: Uuid,
    new_codes: &[String],
) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET totp_recovery_codes_hashed = $2 WHERE id = $1")
        .bind(user_id)
        .bind(new_codes)
        .execute(pool)
        .await?;
    Ok(())
}

/// Soft-delete a user — null PII, revoke devices/sessions/tokens.
pub async fn soft_delete(pool: &PgPool, user_id: Uuid) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"UPDATE users
              SET deleted_at = NOW(),
                  status = 'deleted',
                  email = ('deleted-' || id::TEXT || '@deleted.invalid')::CITEXT,
                  password_hash = '!',
                  totp_enabled = FALSE,
                  totp_secret_encrypted = NULL,
                  totp_recovery_codes_hashed = NULL
            WHERE id = $1"#,
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE devices SET status = 'revoked' WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE sessions SET revoked_at = NOW() WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Permanently purge a user and **all** of their data (GDPR-style erasure)
/// — used by the admin "delete user" action. Unlike `soft_delete`, the
/// `users` row is removed outright.
///
/// FK `ON DELETE CASCADE` clears the bulk of it: devices, sessions,
/// verification tokens, user_preferences, session_events,
/// topology_positions, connection_sessions and bandwidth_aggregates (and,
/// via devices, peer_endpoint_history). The tables that DON'T cascade are
/// purged explicitly first, while the user link still exists:
///   - `bandwidth_samples` — partitioned, no FK to devices → delete by device id
///   - `audit_logs` (actor + target), `access_logs`, `destination_ips` —
///     `ON DELETE SET NULL`, so they'd be anonymized-not-removed
///   - `failed_logins` — keyed by email (CITEXT), no FK
///
/// Everything runs in one transaction so a failure leaves the user intact.
pub async fn hard_delete(pool: &PgPool, user_id: Uuid, email: &str) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;

    // Raw per-tick samples have no FK to devices, so the user→devices
    // cascade won't reach them. Grab the device ids first, then purge.
    let device_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM devices WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
    if !device_ids.is_empty() {
        sqlx::query("DELETE FROM bandwidth_samples WHERE device_id = ANY($1)")
            .bind(&device_ids)
            .execute(&mut *tx)
            .await?;
    }

    // SET-NULL / no-FK tables: delete the user's rows while the link is
    // still intact (once the users row goes, SET NULL severs the reference).
    sqlx::query("DELETE FROM audit_logs WHERE actor_user_id = $1 OR target_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM access_logs WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM destination_ips WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM failed_logins WHERE email_attempted = $1::CITEXT")
        .bind(email)
        .execute(&mut *tx)
        .await?;

    // Finally the user — CASCADE removes everything else.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Row used by the admin user list page.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminUserRow {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub totp_enabled: bool,
    pub created_at: OffsetDateTime,
    pub last_login_at: Option<OffsetDateTime>,
    pub device_count: i64,
}

/// Row used by the admin user-detail page. Adds quota fields, the
/// password-change watermark, and verification flags on top of the list
/// row so the detail view can render a complete picture in one query.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminUserDetailRow {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub totp_enabled: bool,
    pub must_change_password: bool,
    pub created_at: OffsetDateTime,
    pub last_login_at: Option<OffsetDateTime>,
    pub email_verified_at: Option<OffsetDateTime>,
    pub password_changed_at: OffsetDateTime,
    pub current_month_bytes: i64,
    pub monthly_byte_cap: Option<i64>,
    pub quota_resets_at: Option<OffsetDateTime>,
    pub device_count: i64,
    /// Max active (non-revoked) devices this user may create. Admin-settable.
    pub device_limit: i32,
}

/// Fetch a single user with everything the admin detail page needs.
/// Returns None when the id doesn't match a non-deleted user.
pub async fn admin_user_detail(
    pool: &PgPool,
    user_id: Uuid,
) -> sqlx::Result<Option<AdminUserDetailRow>> {
    sqlx::query_as::<_, AdminUserDetailRow>(
        r#"SELECT u.id, u.email::TEXT AS email, u.role, u.status, u.totp_enabled,
                  u.must_change_password, u.created_at, u.last_login_at,
                  u.email_verified_at, u.password_changed_at,
                  COALESCE(u.current_month_bytes, 0)::BIGINT AS current_month_bytes,
                  u.monthly_byte_cap, u.quota_resets_at, u.device_limit,
                  (SELECT COUNT(*) FROM devices d
                    WHERE d.user_id = u.id AND d.status <> 'revoked') AS device_count
             FROM users u
            WHERE u.id = $1 AND u.deleted_at IS NULL"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// The user's active-device cap (defaults to 5 in the schema). Read on device
/// create to enforce the per-user limit. Falls back to 5 if the user is gone.
pub async fn device_limit(pool: &PgPool, user_id: Uuid) -> sqlx::Result<i32> {
    let limit: Option<i32> =
        sqlx::query_scalar("SELECT device_limit FROM users WHERE id = $1 AND deleted_at IS NULL")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(limit.unwrap_or(5))
}

/// Admin: set a user's active-device cap. Returns rows affected (0 = no such
/// non-deleted user). The caller is responsible for clamping `limit` to a sane
/// range before calling.
pub async fn set_device_limit(pool: &PgPool, user_id: Uuid, limit: i32) -> sqlx::Result<u64> {
    let res = sqlx::query("UPDATE users SET device_limit = $2 WHERE id = $1 AND deleted_at IS NULL")
        .bind(user_id)
        .bind(limit)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// Filter knobs for the admin user list. All optional — `None` on a field
/// disables that filter. The same filter set is shared by the JSON list
/// endpoint and the CSV export so they always agree.
#[derive(Debug, Default, Clone, Copy)]
pub struct AdminUserFilters<'a> {
    pub search: Option<&'a str>,
    pub status: Option<UserStatus>,
    pub role: Option<UserRole>,
    pub totp_enabled: Option<bool>,
}

pub async fn admin_list(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    f: AdminUserFilters<'_>,
) -> sqlx::Result<Vec<AdminUserRow>> {
    let pattern = f.search.map(|s| format!("%{}%", s.to_lowercase()));
    sqlx::query_as::<_, AdminUserRow>(
        r#"SELECT u.id, u.email::TEXT AS email, u.role, u.status, u.totp_enabled,
                  u.created_at, u.last_login_at,
                  (SELECT COUNT(*) FROM devices d
                    WHERE d.user_id = u.id AND d.status <> 'revoked') AS device_count
             FROM users u
            WHERE u.deleted_at IS NULL
              AND ($3::TEXT IS NULL OR LOWER(u.email::TEXT) LIKE $3)
              AND ($4::user_status IS NULL OR u.status = $4)
              AND ($5::user_role   IS NULL OR u.role   = $5)
              AND ($6::BOOLEAN     IS NULL OR u.totp_enabled = $6)
            ORDER BY u.created_at DESC
            LIMIT $1 OFFSET $2"#,
    )
    .bind(limit)
    .bind(offset)
    .bind(pattern)
    .bind(f.status)
    .bind(f.role)
    .bind(f.totp_enabled)
    .fetch_all(pool)
    .await
}

pub async fn admin_count(pool: &PgPool, f: AdminUserFilters<'_>) -> sqlx::Result<i64> {
    let pattern = f.search.map(|s| format!("%{}%", s.to_lowercase()));
    let row: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM users
            WHERE deleted_at IS NULL
              AND ($1::TEXT IS NULL OR LOWER(email::TEXT) LIKE $1)
              AND ($2::user_status IS NULL OR status = $2)
              AND ($3::user_role   IS NULL OR role   = $3)
              AND ($4::BOOLEAN     IS NULL OR totp_enabled = $4)"#,
    )
    .bind(pattern)
    .bind(f.status)
    .bind(f.role)
    .bind(f.totp_enabled)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Deployment-wide user + device counts for the admin overview KPI strip.
/// Returned in a single query so the strip doesn't fan out to 5
/// round-trips. Ignores soft-deleted users (the admin UI never surfaces
/// them) but counts every non-revoked device the remaining users own.
pub async fn admin_stats(pool: &PgPool) -> sqlx::Result<AdminStats> {
    let row: AdminStats = sqlx::query_as::<_, AdminStats>(
        r#"SELECT
              COUNT(*) FILTER (WHERE deleted_at IS NULL)::BIGINT
                AS total,
              COUNT(*) FILTER (WHERE deleted_at IS NULL AND status = 'active')::BIGINT
                AS active,
              COUNT(*) FILTER (WHERE deleted_at IS NULL AND status = 'suspended')::BIGINT
                AS suspended,
              COUNT(*) FILTER (WHERE deleted_at IS NULL AND status = 'pending_verification')::BIGINT
                AS pending_verification,
              (SELECT COUNT(*) FROM devices d
                 JOIN users u ON u.id = d.user_id
                WHERE u.deleted_at IS NULL AND d.status <> 'revoked')::BIGINT
                AS devices_total,
              -- Devices online *now*, using the same rule the frontend's
              -- connState() applies (active + handshake within the 180s
              -- window). DB-backed so the admin KPI is correct on first
              -- paint and survives refresh, instead of depending on a live
              -- WS tick that starts empty each session.
              (SELECT COUNT(*) FROM devices d
                 JOIN users u ON u.id = d.user_id
                WHERE u.deleted_at IS NULL
                  AND d.status = 'active'
                  AND d.last_handshake_at > now() - interval '180 seconds')::BIGINT
                AS online_now
           FROM users"#,
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct AdminStats {
    pub total: i64,
    pub active: i64,
    pub suspended: i64,
    pub pending_verification: i64,
    pub devices_total: i64,
    pub online_now: i64,
}

pub async fn admin_set_status(
    pool: &PgPool,
    user_id: Uuid,
    status: UserStatus,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE users SET status = $2 WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user_id)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Admin override: change a user's role between admin and user.
pub async fn admin_set_role(
    pool: &PgPool,
    user_id: Uuid,
    role: UserRole,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE users SET role = $2 WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// User row including the password hash. Only used by the auth crate; never
/// returned over the API.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserWithSecrets {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub must_change_password: bool,
    pub email_verified_at: Option<OffsetDateTime>,
    pub totp_enabled: bool,
    pub created_at: OffsetDateTime,
    pub last_login_at: Option<OffsetDateTime>,
    pub password_changed_at: OffsetDateTime,
}

impl From<UserWithSecrets> for User {
    fn from(u: UserWithSecrets) -> Self {
        User {
            id: u.id,
            email: u.email,
            role: u.role,
            status: u.status,
            must_change_password: u.must_change_password,
            email_verified_at: u.email_verified_at,
            totp_enabled: u.totp_enabled,
            created_at: u.created_at,
            last_login_at: u.last_login_at,
            password_changed_at: u.password_changed_at,
        }
    }
}
