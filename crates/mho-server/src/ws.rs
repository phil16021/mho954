//! Live stream: binary messages carry waveform frames ([`mho_service::live::encode_frame`]),
//! text messages carry JSON measurements and errors ([`mho_service::live::WsText`]).

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use mho_service::live::WsText;
use std::time::Duration;
use tracing::debug;

use crate::AppState;

/// A client that cannot take a message within this time is disconnected, so a stalled client
/// does not keep the live loop polling the scope.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const PING_INTERVAL: Duration = Duration::from_secs(15);

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
    let mut progress = hub.subscribe_progress();
    let mut bode = hub.subscribe_bode();
    frames.mark_changed();
    meas.mark_changed();
    // A viewer that connects later still gets the last sweep.
    bode.mark_changed();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;

    loop {
        let msg = tokio::select! {
            r = frames.changed() => {
                if r.is_err() { break; }
                let frame = frames.borrow_and_update().clone();
                match frame {
                    Some(f) => Message::Binary(f.bytes.clone()),
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
            r = progress.changed() => {
                if r.is_err() { break; }
                let p = progress.borrow_and_update().clone();
                match p {
                    Some(p) => text(&WsText::Progress(p)),
                    None => continue,
                }
            }
            r = bode.changed() => {
                if r.is_err() { break; }
                let b = bode.borrow_and_update().clone();
                match b {
                    Some(b) => text(&WsText::Bode((*b).clone())),
                    None => continue,
                }
            }
            _ = ping.tick() => Message::Ping(Vec::new().into()),
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            },
        };
        match tokio::time::timeout(SEND_TIMEOUT, socket.send(msg)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => break,
            Err(_) => {
                debug!("websocket client too slow, disconnecting");
                break;
            }
        }
    }
    debug!("websocket client disconnected");
}

fn text(t: &WsText) -> Message {
    Message::Text(serde_json::to_string(t).expect("ws text serialises").into())
}
