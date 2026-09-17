
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "user_role", rename_all = "snake_case")]
pub enum UserRole {
    Admin,
    User,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "user_status", rename_all = "snake_case")]
pub enum UserStatus {
    Active,
    Suspended,
    PendingVerification,
    Deleted,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "device_status", rename_all = "snake_case")]
pub enum DeviceStatus {
    Active,
    Paused,
    Revoked,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "device_os", rename_all = "snake_case")]
pub enum DeviceOs {
    Ios,
    Android,
    Macos,
    Windows,
    Linux,
    Other,
}

/// Device form factor — the physical kind of device, independent of its OS
/// (e.g. an Android tablet is `os = Android`, `device_type = Tablet`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "device_type", rename_all = "snake_case")]
pub enum DeviceType {
    Phone,
    Tablet,
    Laptop,
    Desktop,
    Tv,
    Router,
    Watch,
    Iot,
    Server,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub must_change_password: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub email_verified_at: Option<OffsetDateTime>,
    pub totp_enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login_at: Option<OffsetDateTime>,
    /// Bumped on every successful password change (reset link, admin
    /// reset, must-change-password flow). The auth extractor compares
    /// the value snapshotted into the session at login time against the
    /// live row — any mismatch kicks the session.
    #[serde(with = "time::serde::rfc3339")]
    pub password_changed_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    pub region: String,
    pub endpoint_host: String,
    pub endpoint_port: i32,
    pub public_key: String,
    /// CIDR for the server's subnet. Serialised as a string ("10.10.0.0/22").
    #[schema(value_type = String, example = "10.10.0.0/22")]
    pub cidr: IpNetwork,
    pub mtu: i32,
    pub is_active: bool,
    /// WireGuard `PersistentKeepalive` (seconds) handed to every peer on
    /// this server — written into the client `.conf` and used by live
    /// `wg set peer` calls. `0` disables keepalive (WG default semantics).
    pub persistent_keepalive: i16,
    /// Extra split-tunnel routes inherited by newly provisioned devices.
    pub default_allowed_ips: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct Device {
    pub id: Uuid,
    pub user_id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    pub os: DeviceOs,
    pub device_type: DeviceType,
    pub public_key: String,
    /// Peer's allocated address as a host prefix ("10.10.0.5/32" or "fd00::5/128").
    #[schema(value_type = String, example = "10.10.0.5/32")]
    pub allocated_ip: IpNetwork,
    pub status: DeviceStatus,
    pub allowed_ips_override: Option<Vec<String>>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_handshake_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// KEK-encrypted WG private key, when the user opted in at create time.
    /// None for devices created with the default zero-knowledge behaviour.
    /// Never exposed by list/get endpoints — the field is `serde(skip)` at
    /// the API boundary; included here for completeness of the domain shape.
    #[serde(skip)]
    #[schema(value_type = Option<String>, format = Byte)]
    pub private_key_encrypted: Option<Vec<u8>>,
}
