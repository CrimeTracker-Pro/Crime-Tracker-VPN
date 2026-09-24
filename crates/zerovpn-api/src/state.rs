use std::sync::Arc;

use tokio::sync::broadcast;
use zerovpn_auth::kek::Kek;
use zerovpn_core::config::GoogleOAuthConfig;
use zerovpn_db::PgPool;
use zerovpn_mail::Mailer;
use zerovpn_wg::{WgController, ip_alloc::IpAllocator};
use zerovpn_wire::Event;

use crate::routes::ws::BROADCAST_BUFFER;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// Per-server IP allocator, keyed by server UUID.
    pub allocators: Arc<IpAllocators>,
    /// Broadcast bus into which the ZMQ subscriber writes events.
    pub events: broadcast::Sender<Event>,
    /// Key-encryption key used for column-level secrets (TOTP, optional WG PSK).
    pub kek: Arc<Kek>,
    /// Outbound email transport. None when SMTP is unconfigured (dev fallback);
    /// routes that need to send fall back to logging the link.
    pub mailer: Option<Arc<Mailer>>,
    /// Public URL the API is served at, used to build email links.
    pub public_url: String,
    /// WG runtime controller. Defaults to NoopController in dev.
    pub wg: Arc<dyn WgController>,
    /// Google OAuth 2.0 client config. `None` when the operator hasn't set
    /// `ZEROVPN_GOOGLE_OAUTH__*` — `/auth/google/*` routes return 503 in
    /// that case so the rest of the API still boots without credentials.
    pub google_oauth: Option<Arc<GoogleOAuthConfig>>,
    /// Sliding-window ceiling for invitations and resends.
    pub mail_limits: Arc<crate::ratelimit::MailLimits>,
}

impl AppState {
    /// Fire-and-forget publish onto the live event bus. The WebSocket
    /// handler fans this out to every connected client that
    /// [`crate::routes::ws::visible_to`] lets through (the owning user's
    /// other sessions, plus admins). Used by mutation handlers to make
    /// add/edit/delete reflect across a user's devices in real time.
    /// Errors only when there are zero subscribers, which is fine to drop.
    pub fn broadcast(&self, event: Event) {
        let _ = self.events.send(event);
    }

    pub fn new(
        pool: PgPool,
        allocators: Arc<IpAllocators>,
        kek: Arc<Kek>,
        mailer: Option<Mailer>,
        public_url: String,
        wg: Arc<dyn WgController>,
        google_oauth: Option<GoogleOAuthConfig>,
    ) -> Self {
        let (events, _) = broadcast::channel::<Event>(BROADCAST_BUFFER);
        Self {
            pool,
            allocators,
            events,
            kek,
            mailer: mailer.map(Arc::new),
            public_url,
            wg,
            google_oauth: google_oauth.map(Arc::new),
            mail_limits: Arc::new(crate::ratelimit::MailLimits::default()),
        }
    }
}

#[derive(Default)]
pub struct IpAllocators {
    pub map: parking_lot::RwLock<std::collections::HashMap<uuid::Uuid, Arc<IpAllocator>>>,
}

impl IpAllocators {
    pub fn get(&self, server_id: uuid::Uuid) -> Option<Arc<IpAllocator>> {
        self.map.read().get(&server_id).cloned()
    }

    pub fn insert(&self, server_id: uuid::Uuid, alloc: Arc<IpAllocator>) {
        self.map.write().insert(server_id, alloc);
    }
}
