//! Live stream: binary messages carry waveform frames ([`mho_service::live::encode_frame`]),
//! text messages carry JSON measurements and errors ([`mho_service::live::WsText`]).

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use mho_service::live::{WsText, encode_frame};
use tracing::debug;

use crate::AppState;

pub fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/stream", get(upgrade))
}

async fn upgrade(State(s): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| stream(socket, s))
}

async fn stream(mut socket: WebSocket, s: AppState) {
    let hub = s.backend.hub();
    let mut frames = hub.subscribe_frames();
    let mut meas = hub.subscribe_measurements();
    let mut errors = hub.subscribe_errors();
    frames.mark_changed();
    meas.mark_changed();

    loop {
        let msg = tokio::select! {
            r = frames.changed() => {
                if r.is_err() { break; }
                let frame = frames.borrow_and_update().clone();
                match frame {
                    Some(f) => Message::Binary(encode_frame(&f).into()),
                    None => continue,
                }
            }
            r = meas.changed() => {
                if r.is_err() { break; }
                let m = meas.borrow_and_update().clone();
                match m {
                    Some(m) => text(&WsText::Measurements((*m).clone())),
                    None => continue,
                }
            }
            r = errors.changed() => {
                if r.is_err() { break; }
                let e = errors.borrow_and_update().clone();
                match e {
                    Some(message) => text(&WsText::Error { message }),
                    None => continue,
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            },
        };
        if socket.send(msg).await.is_err() {
            break;
        }
    }
    debug!("websocket client disconnected");
}

fn text(t: &WsText) -> Message {
    Message::Text(serde_json::to_string(t).expect("ws text serialises").into())
}
