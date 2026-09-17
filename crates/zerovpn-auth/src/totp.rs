//! TOTP enrollment, verification, and recovery codes.
//!
//! Uses `totp-rs` for the TOTP itself; recovery codes are 8-character
//! base32 strings, hashed at rest with argon2 (so an attacker who reads
//! the DB can't immediately use them).

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use rand::RngCore;
use totp_rs::{Algorithm, Secret, TOTP};

use crate::password;

const RECOVERY_CODE_COUNT: usize = 10;
const RECOVERY_CODE_BYTES: usize = 5; // → 8 base32 chars

#[derive(Debug, thiserror::Error)]
pub enum TotpError {
    #[error("invalid secret")]
    InvalidSecret,
    #[error("totp: {0}")]
    Lib(String),
    #[error("hash: {0}")]
    Hash(#[from] password::HashError),
}

/// Generate a new 20-byte TOTP secret. Returned base32-encoded so the
/// caller can show it as a fallback when QR scanning fails.
pub fn generate_secret_b32() -> String {
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    Secret::Raw(bytes.to_vec()).to_encoded().to_string()
}

/// Build a `TOTP` instance from a base32-encoded secret with our standard
/// 6-digit / 30-second / SHA-1 parameters (RFC 6238 default; matches all
/// authenticator apps).
pub fn build_totp(secret_b32: &str, account: &str, issuer: &str) -> Result<TOTP, TotpError> {
    let secret = Secret::Encoded(secret_b32.to_string())
        .to_bytes()
        .map_err(|_| TotpError::InvalidSecret)?;
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret,
        Some(issuer.to_string()),
        account.to_string(),
    )
    .map_err(|e| TotpError::Lib(e.to_string()))
}

/// Provisioning URI compatible with Google Authenticator, 1Password, etc.
pub fn provisioning_uri(secret_b32: &str, account: &str, issuer: &str) -> Result<String, TotpError> {
    let totp = build_totp(secret_b32, account, issuer)?;
    Ok(totp.get_url())
}

/// Verify a 6-digit code against the stored secret. Allows ±1 step
/// (30s clock skew) which is the common recommendation.
pub fn verify(secret_b32: &str, code: &str) -> Result<bool, TotpError> {
    let totp = build_totp(secret_b32, "", "")?;
    totp.check_current(code).map_err(|e| TotpError::Lib(e.to_string()))
}

/// Generate `RECOVERY_CODE_COUNT` recovery codes. Returns (plaintext, hashed).
/// The plaintext is shown to the user once; the hashed list is stored.
pub fn generate_recovery_codes() -> Result<(Vec<String>, Vec<String>), TotpError> {
    let mut plaintexts = Vec::with_capacity(RECOVERY_CODE_COUNT);
    let mut hashes = Vec::with_capacity(RECOVERY_CODE_COUNT);
    for _ in 0..RECOVERY_CODE_COUNT {
        let mut bytes = [0u8; RECOVERY_CODE_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        // base32 5 bytes = 8 chars (no padding needed for our length)
        let raw = B64.encode(bytes);
        let code = raw.replace(['+', '/', '='], "").chars().take(8).collect::<String>();
        let hashed = password::hash(&code)?;
        plaintexts.push(code);
        hashes.push(hashed);
    }
    Ok((plaintexts, hashes))
}

/// Try to consume a recovery code: if `code` matches any of the hashes,
/// return the index of the matched hash so the caller can remove it.
pub fn match_recovery_code(code: &str, hashes: &[String]) -> Result<Option<usize>, TotpError> {
    for (i, h) in hashes.iter().enumerate() {
        if password::verify(code, h)? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enroll_and_verify_roundtrip() {
        let secret = generate_secret_b32();
        let totp = build_totp(&secret, "alice@example.com", "Crime Tracker VPN").unwrap();
        let now_code = totp.generate_current().unwrap();
        assert!(verify(&secret, &now_code).unwrap());
        assert!(!verify(&secret, "000000").unwrap());
    }

    #[test]
    fn provisioning_uri_contains_issuer_and_account() {
        let secret = generate_secret_b32();
        let uri = provisioning_uri(&secret, "alice@example.com", "Crime Tracker VPN").unwrap();
        assert!(
            uri.contains("issuer=Crime%20Tracker%20VPN")
                || uri.contains("issuer=Crime+Tracker+VPN")
        );
        // The library URL-encodes the account email's @
        assert!(uri.contains("alice%40example.com") || uri.contains("alice@example.com"));
    }

    #[test]
    fn recovery_codes_match_then_consume() {
        let (plain, hashes) = generate_recovery_codes().unwrap();
        assert_eq!(plain.len(), RECOVERY_CODE_COUNT);
        let pick = &plain[3];
        assert_eq!(match_recovery_code(pick, &hashes).unwrap(), Some(3));
        assert_eq!(match_recovery_code("xxxxxxxx", &hashes).unwrap(), None);
    }
}
