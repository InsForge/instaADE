// Ported from t3boat (https://github.com/paukraft/t3boat), MIT License,
// Copyright (c) 2026 Pau Kraft. See NOTICE in src-tauri/src/t3proxy.

//! WebSockets and streams relayed to the sandbox, with a media ticket the proxy issues itself in
//! the URL (an image or a WebSocket cannot send a header) or its bearer token.
//!
//! Device and browser-preview streams are live pictures of what runs on the sandbox, so they
//! exist only while it runs. T3 Code keeps retrying one while it is on screen, so they never wake
//! the sandbox: a sleeping one answers 503. Any other socket (dictation, and whatever T3 adds) is
//! opened by a person and wakes a sandbox that paused itself; one its owner paused or stopped
//! answers 503 with what to do. Every relayed socket and plain stream (MJPEG, events) holds an
//! instabox port lease, so it ends when the proxy releases the sandbox (`super::relay` ends a
//! plain one).

use super::auth::TicketUse;
use super::runtime::{Power, Runtime};
use super::{
    HOP_BY_HOP, M, Relay, client_authorized, query_param, query_without, relay, unauthorized,
    upstream, wake,
};
use axum::body::Body;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame as DownClose, Message as DownMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as UpClose;
use tokio_tungstenite::tungstenite::{Error as UpError, Message as UpMsg};

/// T3 Code acknowledges every preview frame with this: the one message a stream sends with
/// nobody touching it.
const FRAME_ACK: &str = r#"{"type":"ack"}"#;

/// `/api/device-hub/*` and `/api/preview-stream/*`: plain requests (snapshots, MJPEG and
/// event streams, uploads) and WebSockets alike.
pub async fn stream(
    State(m): M,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    if let Ok(upgrade) = upgrade {
        return socket(m, upgrade, &parts, false).await;
    }
    let Some(target) = authorized_target(&m, &parts) else {
        return unauthorized();
    };
    // Anything but a read is someone working the device or the browser.
    if parts.method != Method::GET && parts.method != Method::HEAD {
        m.user_action();
    }
    let route = Relay {
        target,
        authorize: true,
        wake: false,
    };
    relay(&m, parts, body, route).await
}

/// Relays a WebSocket to the same path on the machine. `wakes`: a sleeping machine is woken for
/// it; otherwise it answers 503.
pub(super) async fn socket(
    m: Arc<Runtime>,
    upgrade: WebSocketUpgrade,
    parts: &Parts,
    wakes: bool,
) -> Response {
    let Some(target) = authorized_target(&m, parts) else {
        return unauthorized();
    };
    if m.power() != Power::Running
        && let Err(res) = wake(&m, wakes, &target).await
    {
        return *res;
    }
    match m.connect_upstream(&target).await {
        Ok(up) => upgrade.on_upgrade(move |down| pump(m, down, up)),
        // The machine's own refusal (no device hub, say) reaches T3 Code as it was.
        Err(e) => match e.downcast_ref() {
            Some(UpError::Http(res)) => refusal(res),
            _ => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
        },
    }
}

/// A refused upgrade with its status, headers and body. T3 Code reads the headers: a
/// `/api/preview/tunnel` refused with `x-t3-tunnel-port-refused` tells its locality probe that
/// the sandbox is another machine; with the status alone, the probe retries every 30 s.
fn refusal(res: &axum::http::Response<Option<Vec<u8>>>) -> Response {
    let mut out = Response::builder().status(res.status());
    for (k, v) in res.headers() {
        // The body here may be only what came with the head, so its length is counted anew.
        if k != header::CONTENT_LENGTH && !HOP_BY_HOP.contains(&k.as_str()) {
            out = out.header(k, v);
        }
    }
    out.body(Body::from(res.body().clone().unwrap_or_default()))
        .unwrap_or_else(|_| res.status().into_response())
}

/// Path and query on the machine, minus the proxy's ticket; `None` if the caller holds neither a
/// media ticket nor a token of this proxy.
fn authorized_target(m: &Runtime, parts: &Parts) -> Option<String> {
    let query = parts.uri.query().unwrap_or_default();
    let ticketed =
        query_param(query, "wsTicket").is_some_and(|t| m.auth.check_ticket(&t, TicketUse::Media));
    (ticketed || client_authorized(m, &parts.headers)).then(|| {
        format!(
            "{}?{}",
            parts.uri.path(),
            query_without(query, &["wsTicket", "dpop"])
        )
    })
}

/// Passes frames both ways until either side ends, close code and reason included: T3 Code
/// reads them to tell a closed tab from a stream to reopen.
async fn pump(m: Arc<Runtime>, down: WebSocket, up: upstream::Upstream) {
    let mut power = m.subscribe_power();
    let (mut to_client, mut from_client) = down.split();
    let (mut to_machine, mut from_machine) = up.split();
    let upward = async {
        let mut ping = tokio::time::interval(upstream::PING);
        loop {
            let msg = tokio::select! {
                msg = from_client.next() => {
                    let Some(Ok(msg)) = msg else { break };
                    // A click or a key in the stream is someone using the machine. Watching is
                    // not: an open preview must not keep it from sleeping.
                    if is_input(&msg) {
                        m.user_action();
                    }
                    machine_message(msg)
                }
                _ = ping.tick() => Some(UpMsg::Ping(Default::default())),
            };
            if let Some(msg) = msg
                && to_machine.send(msg).await.is_err()
            {
                break;
            }
        }
    };
    let downward = async {
        // Pongs count: only a machine that stopped answering falls silent.
        while let Ok(Some(Ok(msg))) =
            tokio::time::timeout(upstream::SILENCE, from_machine.next()).await
        {
            if let Some(msg) = client_message(msg)
                && to_client.send(msg).await.is_err()
            {
                break;
            }
        }
    };
    // Released or asleep: the lease this socket holds must end. T3 Code reopens the stream
    // while it is on screen, and a sleeping sandbox answers that with 503.
    let released = power.wait_for(|p| *p != Power::Running);
    tokio::select! {
        _ = upward => {}
        _ = downward => {}
        _ = released => {}
    }
}

fn is_input(msg: &DownMsg) -> bool {
    match msg {
        DownMsg::Text(text) => text.as_str() != FRAME_ACK,
        DownMsg::Binary(_) => true,
        _ => false,
    }
}

/// Each side answers its own pings; the rest crosses over as it is.
fn machine_message(msg: DownMsg) -> Option<UpMsg> {
    Some(match msg {
        DownMsg::Text(text) => UpMsg::text(text.as_str()),
        DownMsg::Binary(bytes) => UpMsg::Binary(bytes),
        DownMsg::Close(frame) => UpMsg::Close(frame.map(|f| UpClose {
            code: f.code.into(),
            reason: f.reason.as_str().into(),
        })),
        DownMsg::Ping(_) | DownMsg::Pong(_) => return None,
    })
}

fn client_message(msg: UpMsg) -> Option<DownMsg> {
    Some(match msg {
        UpMsg::Text(text) => DownMsg::text(text.as_str()),
        UpMsg::Binary(bytes) => DownMsg::Binary(bytes),
        UpMsg::Close(frame) => DownMsg::Close(frame.map(|f| DownClose {
            code: f.code.into(),
            reason: f.reason.as_str().into(),
        })),
        UpMsg::Ping(_) | UpMsg::Pong(_) | UpMsg::Frame(_) => return None,
    })
}
