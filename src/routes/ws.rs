use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures_util::SinkExt;
use futures_util::StreamExt;
use tokio::sync::broadcast;

use crate::events::AppEvent;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/ws", axum::routing::get(ws_handler))
}

/// `GET /ws` — upgrade to a WebSocket connection.
///
/// Once upgraded the server streams all [`AppEvent`]s as JSON text frames to
/// the client. Each message is a complete JSON object:
///
/// ```json
/// {"event":"instance:progress","data":{"instance_id":"...","phase":"...","remaining":0}}
/// ```
///
/// The server does **not** currently process messages from the client (the
/// connection is read-only from the client's perspective). Client messages are
/// silently drained to keep the connection alive.
#[utoipa::path(
    get,
    path = "/ws",
    tag = "events",
    responses(
        (status = 101, description = "WebSocket upgrade successful — connection switches to the WebSocket protocol. The server immediately begins sending JSON text frames of [`AppEvent`] values.")
    )
)]
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let rx = state.event_tx.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, rx))
}

async fn handle_socket(socket: WebSocket, mut rx: broadcast::Receiver<AppEvent>) {
    let (mut tx, mut rx_ws) = socket.split();

    let send_task = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let json = serde_json::to_string(&event).unwrap();
            if tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    let recv_task = tokio::spawn(async move {
        while let Some(Ok(_)) = rx_ws.next().await {
            // Keep connection alive by draining incoming messages
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
}
