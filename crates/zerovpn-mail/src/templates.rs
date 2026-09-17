//! Transactional email templates.
//!
//! Each email type is defined by a public input struct and a paired
//! `impl Email`. The struct carries the runtime data (URLs, names,
//! timestamps); the trait turns it into a tuple of (subject, html, text)
//! ready for [`crate::Mailer::send_email`].
//!
//! ## Design system
//! HTML templates extend `base.html`, which provides:
//!   • wordmark header, eyebrow, title, content slot, footer
//!   • mobile + dark-mode media queries (`<style>` block in <head>)
//!   • bulletproof button + tinted alert + key/value list macros in
//!     `_components.html`
//!
//! Plain-text counterparts live in `.txt` siblings — each email ships
//! both, wrapped in a `multipart/alternative` MIME container so every
//! client picks the best fit.
//!
//! ## Adding a new email
//!   1. Drop `<name>.html` + `<name>.txt` into `templates/`.
//!   2. Add a pair of `#[derive(Template)]` types (one per variant) and
//!      a public input struct that fans into both.
//!   3. Implement [`Email`] on the input struct.

use askama::Template;

/// Render contract every email type implements. Returns the rendered
/// `(subject, html, text)` triple that [`crate::Mailer::send_email`]
/// drops directly into a multipart MIME body.
pub trait Email {
    fn subject(&self) -> &'static str;
    fn render_html(&self) -> Result<String, askama::Error>;
    fn render_text(&self) -> Result<String, askama::Error>;
}

// ── Verify email ─────────────────────────────────────────────────────────

pub struct VerifyEmail<'a> {
    pub link: &'a str,
}

#[derive(Template)]
#[template(path = "verify_email.html", escape = "html")]
struct VerifyEmailHtml<'a> {
    link: &'a str,
}

#[derive(Template)]
#[template(path = "verify_email.txt", escape = "none")]
struct VerifyEmailText<'a> {
    link: &'a str,
}

impl Email for VerifyEmail<'_> {
    fn subject(&self) -> &'static str {
        "You're invited to Crime Tracker VPN"
    }
    fn render_html(&self) -> Result<String, askama::Error> {
        VerifyEmailHtml { link: self.link }.render()
    }
    fn render_text(&self) -> Result<String, askama::Error> {
        VerifyEmailText { link: self.link }.render()
    }
}

// ── Suspicious / new-IP sign-in ──────────────────────────────────────────

pub struct SuspiciousLogin<'a> {
    pub email: &'a str,
    pub when: &'a str,
    pub security_link: &'a str,
    /// Optional client IP, displayed verbatim if present. Pass the full
    /// address now that the prefix-truncation has been removed upstream.
    pub ip: Option<&'a str>,
    /// Optional user-agent string, useful for spotting unfamiliar
    /// browsers / curl-style tooling in the alert.
    pub user_agent: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "suspicious_login.html", escape = "html")]
struct SuspiciousLoginHtml<'a> {
    email: &'a str,
    when: &'a str,
    security_link: &'a str,
    ip: Option<&'a str>,
    user_agent: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "suspicious_login.txt", escape = "none")]
struct SuspiciousLoginText<'a> {
    email: &'a str,
    when: &'a str,
    security_link: &'a str,
    ip: Option<&'a str>,
    user_agent: Option<&'a str>,
}

impl Email for SuspiciousLogin<'_> {
    fn subject(&self) -> &'static str {
        "New sign-in to your Crime Tracker VPN account"
    }
    fn render_html(&self) -> Result<String, askama::Error> {
        SuspiciousLoginHtml {
            email: self.email,
            when: self.when,
            security_link: self.security_link,
            ip: self.ip,
            user_agent: self.user_agent,
        }
        .render()
    }
    fn render_text(&self) -> Result<String, askama::Error> {
        SuspiciousLoginText {
            email: self.email,
            when: self.when,
            security_link: self.security_link,
            ip: self.ip,
            user_agent: self.user_agent,
        }
        .render()
    }
}
