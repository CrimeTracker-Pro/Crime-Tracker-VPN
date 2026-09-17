//! OpenAPI 3.1 spec served at `/openapi.json`.
//!
//! Built at runtime from `#[utoipa::path]` attributes on each handler +
//! `ToSchema` derives on every DTO. The aggregator lives below as
//! `ApiDoc`; no hand-curated path list to drift out of sync with the
//! actual routes.
//!
//! Frontend type generation (`openapi-typescript`) reads this directly.

use axum::{Json, response::IntoResponse};
use utoipa::{Modify, OpenApi, openapi::security::{SecurityScheme, ApiKey, ApiKeyValue}};

use super::{admin, auth, bandwidth, connections, devices, dto, health, invitations, me, oauth, totp, ws};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Crime Tracker VPN API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Self-hosted WireGuard VPN management API.",
    ),
    servers((url = "/api/v1")),
    modifiers(&SessionCookieSecurity),
    paths(
        // Health (top-level — not under /api/v1)
        health::health,
        health::ready,
        health::ping,

        // Auth
        auth::logout,
        auth::me,
        totp::setup,
        totp::enable,
        totp::disable,
        totp::regenerate_recovery_codes,
        invitations::verify,
        invitations::list,
        invitations::resend,
        invitations::create_link,
        invitations::revoke,
        me::list_sessions,
        me::revoke_session,
        oauth::google_start,
        oauth::google_callback,
        oauth::google_verify_totp,

        // Account
        me::export,
        me::server_info,
        me::get_topology,
        me::set_topology,
        me::get_preferences,
        me::set_preferences,
        me::delete_account,
        me::usage,
        me::activity,
        me::revoke_other_sessions,

        // Devices
        devices::list,
        devices::get,
        devices::events,
        devices::create,
        devices::connect,
        devices::reorder,
        devices::rotate_keys,
        devices::delete,
        devices::redownload_conf,
        devices::patch,
        devices::set_my_quota,
        devices::pause,
        devices::unpause,

        // Bandwidth
        bandwidth::for_device,
        bandwidth::device_history,
        bandwidth::device_candles,
        bandwidth::server_history,
        bandwidth::server_candles,
        bandwidth::user_candles,
        bandwidth::for_user,
        bandwidth::admin_device_candles,
        bandwidth::admin_user_candles,

        // Admin
        admin::list_users,
        admin::set_user_status,
        admin::list_audit,
        admin::list_audit_csv,
        admin::set_user_quota,
        admin::set_user_device_limit,
        admin::list_failed_logins,
        admin::get_maintenance,
        admin::set_maintenance,
        admin::get_user_policy,
        admin::set_user_policy,
        connections::list_for_user,
        connections::list_all,
        admin::list_devices,
        admin::device_detail,
        admin::device_bandwidth,
        admin::device_endpoint_history,
        admin::device_connection_history,
        admin::list_session_events,
        admin::list_access_logs,
        admin::finder,
        admin::list_servers,
        admin::patch_server,
        admin::rotate_server_keys,
        admin::stats,
        admin::fleet_bandwidth,
        admin::create_user,
        admin::user_detail,
        admin::delete_user,
        admin::set_user_role,
        admin::admin_disable_2fa,
        admin::admin_revoke_sessions,
        admin::admin_set_email_route,
        admin::user_bandwidth,
        admin::list_users_csv,
        admin::server_detail,
        admin::server_bandwidth,
        admin::set_device_quota,
        admin::admin_pause_device,
        admin::admin_unpause_device,
        admin::admin_revoke_device,
        admin::impersonate_user,
        admin::stop_impersonation,

        // Realtime
        ws::ws,
    ),
    components(schemas(
        // Shared
        dto::StatusAck,
        // Domain (zerovpn-core)
        zerovpn_core::models::User,
        zerovpn_core::models::UserRole,
        zerovpn_core::models::UserStatus,
        zerovpn_core::models::Server,
        zerovpn_core::models::Device,
        zerovpn_core::models::DeviceOs,
        zerovpn_core::models::DeviceStatus,
        // Repo-level (zerovpn-db) types that are direct request/response bodies
        zerovpn_db::repos::user_prefs::UserPreferences,
        zerovpn_db::repos::user_prefs::UserPreferencesPatch,
        zerovpn_db::repos::failed_logins::FailedLoginReason,
        // Admin DTOs whose schemas aren't picked up via the path macros
        // (only registered indirectly via the response body).
        admin::AdminStatsResponse,
        admin::AdminFleetBandwidthResponse,
    )),
    tags(
        (name = "Health", description = "Liveness + readiness probes"),
        (name = "Auth", description = "Invitation verification, Google sign-in and 2FA"),
        (name = "Account", description = "Authenticated user's own profile, preferences, topology, account deletion"),
        (name = "Devices", description = "WireGuard peers"),
        (name = "Bandwidth", description = "Bucketed rx/tx history (per device + aggregate)"),
        (name = "Admin", description = "Admin-only operations on users, servers, audit log, maintenance mode"),
        (name = "Realtime", description = "WebSocket event streaming"),
    ),
)]
pub struct ApiDoc;

/// Declare the `session_cookie` security scheme so handlers can reference
/// it via `security(("session_cookie" = []))`. utoipa doesn't have a
/// first-class `cookie` auth kind in 5.5, so we surface it as an
/// `ApiKey { In = Cookie }` — equivalent on the wire and what the
/// generated client should treat it as.
struct SessionCookieSecurity;

impl Modify for SessionCookieSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("id"))),
        );
    }
}

pub async fn spec() -> impl IntoResponse {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full render-and-serialise pass. utoipa's derive can produce an
    /// `OpenApi` struct that fails to serialise (e.g. when a schema
    /// references a type that lacks `ToSchema`); the type system
    /// doesn't catch those — they surface here.
    #[test]
    fn spec_serialises_cleanly_and_has_expected_shape() {
        let doc = ApiDoc::openapi();
        let json = doc.to_pretty_json().expect("spec must serialise");
        // Spot-check the top-level shape and confirm the
        // session_cookie security scheme registered via the modifier
        // made it into the rendered document.
        assert!(json.contains("\"openapi\""));
        assert!(json.contains("\"session_cookie\""));
        // Confirm at least one named component schema rendered —
        // proxy for "the schemas block in ApiDoc is wired correctly".
        assert!(json.contains("\"PublicDevice\""));
        assert!(json.contains("\"User\""));
    }

    /// Cheap drift detector: forgetting to add `#[utoipa::path]` to a
    /// new handler — or to list it under `ApiDoc::paths` — will surface
    /// here, not as a silently missing schema in the generated frontend
    /// client.
    #[test]
    fn spec_lists_every_known_path() {
        let spec = ApiDoc::openapi();
        let paths: Vec<String> = spec.paths.paths.keys().cloned().collect();
        // Floor covers health/auth/account/devices/bandwidth/
        // admin/realtime). Anything below means we lost coverage; a
        // bump just means add the new entries below and update this.
        assert!(
            paths.len() >= 40,
            "openapi spec has only {} paths; expected ≥40. paths: {paths:?}",
            paths.len()
        );
        for needed in [
            "/health",
            "/ping",
            "/auth/google/callback",
            "/auth/invitations/verify",
            "/admin/invitations",
            "/auth/totp/setup",
            "/me",
            "/me/preferences",
            "/devices",
            "/devices/{id}",
            "/devices/order",
            "/devices/{id}/conf",
            "/devices/{id}/bandwidth",
            "/bandwidth",
            "/admin/users",
            "/admin/maintenance",
            "/admin/servers/{id}/rotate-keys",
            // Newly surfaced in the spec (2026-05-31): account self-service
            // + admin endpoints that were annotated but unlisted.
            "/me/usage",
            "/me/activity",
            "/admin/users/{id}",
            "/admin/users/{id}/impersonate",
            "/admin/impersonate/stop",
            "/admin/servers/{id}/bandwidth",
            "/admin/devices/{id}/candles",
            "/admin/users/{id}/candles",
        ] {
            assert!(
                paths.iter().any(|p| p == needed),
                "openapi spec missing path {needed}; have {paths:?}"
            );
        }
    }
}

#[cfg(test)]
mod summary_test {
    use super::ApiDoc;
    use utoipa::OpenApi;

    /// Diagnostic helper: `cargo test -p zerovpn-api print_openapi_summary -- --ignored --nocapture`
    /// prints path + schema counts. Not an assertion — just a quick
    /// way to eyeball the derived spec when adding new handlers.
    #[test]
    #[ignore]
    fn print_openapi_summary() {
        let doc = ApiDoc::openapi();
        let n_paths = doc.paths.paths.len();
        let n_components = doc.components.as_ref().map(|c| c.schemas.len()).unwrap_or(0);
        eprintln!("openapi.json: {n_paths} paths, {n_components} component schemas");
        for p in doc.paths.paths.keys() {
            eprintln!("  {p}");
        }
    }
}
