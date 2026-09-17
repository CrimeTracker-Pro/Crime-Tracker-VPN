use axum::{
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::IntoResponse,
};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;
use zerovpn_core::models::UserRole;
use zerovpn_wire::Event;

use crate::{
    error::ApiResult,
    extractors::auth::CurrentUser,
    state::AppState,
};

/// Maximum number of events the broadcast channel buffers per subscriber
/// before lagging consumers drop frames. Live stats are recoverable on next
/// poll, so 64 is plenty.
pub const BROADCAST_BUFFER: usize = 64;

/// Upgrade the connection to a WebSocket and stream live events
/// (`heartbeat`, `stats_delta`, `server_health`, etc.) as MessagePack
/// frames. Filtering: regular users only see events scoped to their
/// own user_id; admins see everything except `Heartbeat` and
/// `ServerSample` (admin-only).
#[utoipa::path(
    get,
    path = "/ws",
    tag = "Realtime",
    responses(
        (status = 101, description = "Switching Protocols — WebSocket upgrade succeeded"),
        (status = 401, description = "No session"),
    ),
    security(("session_cookie" = [])),
)]
pub async fn ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<impl IntoResponse> {
    let rx = state.events.subscribe();
    Ok(ws.on_upgrade(move |socket| async move {
        run(socket, rx, user.id, user.role).await;
    }))
}

async fn run(mut socket: WebSocket, mut rx: broadcast::Receiver<Event>, user_id: Uuid, role: UserRole) {
    info!(%user_id, ?role, "ws client connected");
    metrics::counter!("zerovpn_ws_clients_connected").increment(1);
    loop {
        tokio::select! {
            // Inbound from client (we ignore data; just keep reading so the
            // socket stays open and detect close/ping).
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => {
                        debug!(%user_id, "ws client closed");
                        break;
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = socket.send(Message::Pong(p)).await;
                    }
                    Some(Ok(_)) => {} // ignore text/binary/pong
                    Some(Err(e)) => {
                        warn!(%user_id, ?e, "ws recv error");
                        break;
                    }
                }
            }

            // Outbound from broadcast.
            ev = rx.recv() => {
                match ev {
                    Ok(event) => {
                        if !visible_to(&event, user_id, role) { continue; }
                        match zerovpn_wire::encode(&event) {
                            Ok(bytes) => {
                                if socket.send(Message::Binary(bytes.into())).await.is_err() {
                                    debug!(%user_id, "ws send failed; client gone");
                                    break;
                                }
                            }
                            Err(e) => warn!(?e, "encode event"),
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(%user_id, lagged = n, "ws subscriber lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    info!(%user_id, "ws client disconnected");
}

/// Filter: regular users see only their own events; admins see everything.
fn visible_to(event: &Event, user_id: Uuid, role: UserRole) -> bool {
    if role == UserRole::Admin {
        return true;
    }
    match event {
        // Host health (CPU / mem / I/O) feeds the sidebar status panel for
        // every user, so it's not admin-gated. Heartbeat and per-WG-server
        // samples remain admin-only.
        Event::ServerHealth { .. } => true,
        Event::Heartbeat { .. } | Event::ServerSample { .. } => false,
        Event::StatsDelta { user_id: u, .. }
        | Event::HandshakeChange { user_id: u, .. }
        | Event::PeerStatusChanged { user_id: u, .. } => *u == user_id,
        // Mutation notifications: a user sees changes to their own resources;
        // admin-global ones (`user_id == None`, e.g. server / maintenance)
        // are dropped for non-admins (admins already returned `true` above).
        Event::DataChanged { user_id: owner, .. } => *owner == Some(user_id),
        // Notifications are addressed to a specific user (their sessions) or
        // admin-only (`None`, already handled by the admin short-circuit).
        Event::Notify { user_id: owner, .. } => *owner == Some(user_id),
    }
}
