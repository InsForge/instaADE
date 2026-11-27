// Routing, auth endpoints and relaying are ported from t3boat
// (https://github.com/paukraft/t3boat), MIT License, Copyright (c) 2026 Pau Kraft. See NOTICE
// in this directory. The gate, the instabox runtime (`runtime.rs`), the hub (`hub.rs`) and the
// proxy's DPoP session upstream (`dpop.rs`) are InstaADE's own.

//! PROTOTYPE, off by default: a local proxy between T3 Code and the T3 server on a sandbox.
//! Only with `INSTAADE_T3_PROXY` set does anything here run (see `gate`).
//!
//! T3 Code pairs with `http://localhost:<port>`, one fixed port per sandbox, instead of the
//! sandbox's published port. The proxy holds the upstream WebSocket while someone uses T3 Code,
//! releases it after a while without use so the sandbox can pause on its own, answers T3 Code
//! from what it recorded while the sandbox sleeps, and wakes the sandbox for the next action.

/// One line on stderr, like the rest of the app. Never with a token in it.
macro_rules! log_info {
    ($($t:tt)*) => { eprintln!("t3 proxy: {}", format!($($t)*)) };
}
macro_rules! log_warn {
    ($($t:tt)*) => { eprintln!("t3 proxy: warning: {}", format!($($t)*)) };
}
/// Only with `INSTAADE_T3_PROXY_DEBUG=1`.
macro_rules! log_debug {
    ($($t:tt)*) => {
        if crate::t3proxy::debug() {
            eprintln!("t3 proxy: {}", format!($($t)*))
        }
    };
}

pub mod assets;
pub mod auth;
pub mod cache;
pub mod classify;
pub mod dpop;
pub mod hub;
pub mod media;
#[cfg(test)]
mod proxy_tests;
pub mod runtime;
pub mod session;
pub mod store;
pub mod threads;
pub mod upstream;

use anyhow::Result;
use auth::TicketUse;
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{Path, Query, RawQuery, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Form, Json, Router};
use cache::Shell;
use runtime::{Power, Runtime};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use threads::ThreadFlags;

pub use hub::Hub;

/// The environment variable that turns the prototype on. Unset (or anything else): InstaADE
/// pairs T3 Code with the sandbox's published port, as before.
pub const FLAG: &str = "INSTAADE_T3_PROXY";

/// The T3 Code versions this prototype was tried with. `INSTAADE_T3_PROXY=1` uses the proxy
/// for these only; `INSTAADE_T3_PROXY=any` for every version. The proxy talks to T3 on the
/// sandbox with a DPoP session, which T3 has from 0.1.11 on (0.1.13 was tried). An older T3
/// (0.1.10 was tried) gets a bearer session, only with `any`. Towards T3 Code the proxy takes
/// `Bearer` and `DPoP` tokens alike without checking the proof.
pub const TESTED_T3: &[&str] = &["0.1.13"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Off,
    Tested,
    Any,
}

impl Gate {
    pub fn from_env() -> Gate {
        Gate::parse(std::env::var(FLAG).ok().as_deref())
    }

    fn parse(value: Option<&str>) -> Gate {
        match value.map(str::trim) {
            Some("1") => Gate::Tested,
            Some("any") => Gate::Any,
            _ => Gate::Off,
        }
    }

    pub fn on(self) -> bool {
        self != Gate::Off
    }

    /// True when T3 Code at `desktop` pairs through the proxy.
    pub fn takes(self, desktop: Option<&str>) -> bool {
        match self {
            Gate::Off => false,
            Gate::Any => true,
            Gate::Tested => desktop.is_some_and(|v| TESTED_T3.contains(&v)),
        }
    }
}

pub(crate) fn debug() -> bool {
    std::env::var("INSTAADE_T3_PROXY_DEBUG").is_ok_and(|v| v == "1")
}

type M = State<Arc<Runtime>>;

/// Ten years: the proxy's own tokens never make T3 Code pair again.
const CLIENT_TOKEN_TTL_SECS: i64 = 10 * 365 * 24 * 3600;
/// A T3 server linking to the sandbox gives its descriptor 3 s on loopback. The refresh from a
/// running sandbox gets half of that; the last one seen answers after it.
const DESCRIPTOR_REFRESH: std::time::Duration = std::time::Duration::from_millis(1500);
/// T3's largest request: a 100 MB file into the preview browser.
const MAX_BODY: usize = 128 * 1024 * 1024;

/// The proxy's routes for one sandbox.
pub fn router(runtime: Arc<Runtime>) -> Router {
    Router::new()
        .route("/.well-known/t3/environment", get(descriptor))
        .route("/oauth/token", post(oauth_token))
        .route("/api/auth/session", get(auth_session))
        .route("/api/auth/websocket-ticket", post(rpc_ticket))
        .route("/api/auth/websocket-ticket/{opens}", post(ticket))
        .route("/ws", get(ws))
        .route("/api/device-hub/{*path}", any(media::stream))
        .route("/api/preview-stream/{*path}", any(media::stream))
        .route("/api/orchestration/shell", get(shell_snapshot))
        .route(
            "/api/orchestration/threads/{thread}/bounded",
            get(thread_snapshot),
        )
        .route("/api/voice/status", get(voice_status))
        .route("/api/observability/v1/traces", post(traces))
        .fallback(fallback)
        .layer(middleware::from_fn(cors))
        .layer(middleware::from_fn(local_host_only))
        .with_state(runtime)
}

/// Serves `runtime` on `listener` until the process ends.
pub async fn serve(listener: tokio::net::TcpListener, runtime: Arc<Runtime>) -> Result<()> {
    axum::serve(listener, router(runtime)).await?;
    Ok(())
}

/// Web pages can reach 127.0.0.1 too, through DNS rebinding; only requests addressed to this
/// computer by name are T3 Code's.
async fn local_host_only(req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if !is_local_host(host) {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

fn is_local_host(host: &str) -> bool {
    let name = match host.rsplit_once(':') {
        // `[::1]:port`, or `name:port`. A bare `::1` has colons but no brackets: refused.
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

/// The renderer runs at `t3code://app`; the real server allows any origin, so does the proxy.
async fn cors(req: Request, next: Next) -> Response {
    let mut res = if req.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, k: &'static str, v: &'static str| {
        h.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
    };
    set(h, "access-control-allow-origin", "*");
    set(h, "access-control-allow-methods", "GET, POST, OPTIONS");
    set(
        h,
        "access-control-allow-headers",
        "authorization, b3, traceparent, content-type, dpop, x-t3-orchestration-protocol",
    );
    set(h, "access-control-max-age", "600");
    res
}

fn client_authorized(m: &Runtime, headers: &HeaderMap) -> bool {
    auth::bearer(headers).is_some_and(|t| m.has_client_token(t))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized"})),
    )
        .into_response()
}

fn iso_in(secs: i64) -> String {
    (chrono::Utc::now() + chrono::TimeDelta::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn descriptor(State(m): M) -> Response {
    if m.power() == Power::Running {
        let _ = tokio::time::timeout(DESCRIPTOR_REFRESH, m.fetch_descriptor()).await;
    }
    let cached = m.cache.lock().unwrap().descriptor.clone();
    match cached {
        Some(mut d) => {
            // The sidebar shows the sandbox's name, not its hostname.
            d["label"] = Value::String(m.record().name);
            Json(d).into_response()
        }
        // Transient, so T3 Code keeps retrying instead of blocking the connection.
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn oauth_token(State(m): M, Form(form): Form<HashMap<String, String>>) -> Response {
    let code = form
        .get("subject_token")
        .map(String::as_str)
        .unwrap_or_default();
    if !m.auth.redeem_pairing_code(code) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    }
    let token = auth::random_token();
    m.add_client_token(&token);
    let session = m
        .cache
        .lock()
        .unwrap()
        .auth_session
        .clone()
        .unwrap_or_default();
    let scope = session
        .get("scopes")
        .and_then(Value::as_array)
        .map(|s| {
            s.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let mut body = json!({
        "access_token": token,
        "issued_token_type": "urn:ietf:params:oauth:token-type:access_token",
        "token_type": "Bearer",
        "expires_in": CLIENT_TOKEN_TTL_SECS,
        "scope": scope,
    });
    if let Some(p) = session.get("permissions") {
        body["permissions"] = p.clone();
    }
    log_info!("T3 Code paired with {}", m.id());
    Json(body).into_response()
}

async fn auth_session(State(m): M, headers: HeaderMap) -> Response {
    if !client_authorized(&m, &headers) {
        return unauthorized();
    }
    let cached = m.cache.lock().unwrap().auth_session.clone();
    match cached {
        Some(mut s) => {
            s.insert(
                "expiresAt".into(),
                Value::String(iso_in(CLIENT_TOKEN_TTL_SECS)),
            );
            Json(s).into_response()
        }
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// The proxy's own tickets, for the RPC socket and for media streams alike: asking for one
/// never reaches the sandbox.
async fn ticket(State(m): M, Path(opens): Path<TicketUse>, headers: HeaderMap) -> Response {
    if !client_authorized(&m, &headers) {
        return unauthorized();
    }
    let (ticket, ttl) = m.auth.issue_ticket(opens);
    Json(json!({"ticket": ticket, "expiresAt": iso_in(ttl.as_secs() as i64)})).into_response()
}

/// The ticket route without a use, which T3 Code falls back to: an RPC ticket.
async fn rpc_ticket(state: M, headers: HeaderMap) -> Response {
    ticket(state, Path(TicketUse::Rpc), headers).await
}

async fn ws(
    State(m): M,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let query = query.unwrap_or_default();
    let ticket = query_param(&query, "wsTicket");
    let ok = ticket.is_some_and(|t| m.auth.check_ticket(&t, TicketUse::Rpc))
        || client_authorized(&m, &headers);
    if !ok {
        return unauthorized();
    }
    upgrade.on_upgrade(move |socket| session::run(m, socket, query))
}

/// The HTTP shell snapshot T3 Code loads on start: answered from the capture while asleep.
async fn shell_snapshot(
    State(m): M,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> Response {
    if !client_authorized(&m, req.headers()) {
        return unauthorized();
    }
    // Right after the app starts the power is not known yet: the capture would look stale, and
    // the sandbox be woken for what it holds.
    m.settled_power().await;
    if let Some(capture) = m.fresh_capture() {
        let resume_from = q.get("resumeFrom").and_then(|v| v.parse().ok());
        match capture.shell(resume_from) {
            Some(Shell::UpToDate) => return Json(json!({"resumable": true})).into_response(),
            Some(Shell::Snapshot(snapshot)) => return Json(snapshot).into_response(),
            None => {}
        }
    }
    forward(State(m), req).await
}

/// The HTTP snapshot T3 Code opens a chat with: answered from the thread store while asleep.
async fn thread_snapshot(
    State(m): M,
    Path(thread): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> Response {
    if !client_authorized(&m, req.headers()) {
        return unauthorized();
    }
    let flags = ThreadFlags::from_query(&q);
    // As for the shell: the capture counts once the power is known.
    m.settled_power().await;
    if let Some(body) = m
        .fresh_capture()
        .and_then(|c| m.stored_thread(&c, &thread, flags))
    {
        return ([(header::CONTENT_TYPE, "application/json")], body).into_response();
    }
    m.cache.lock().unwrap().thread_encoding = Some(flags);
    forward(State(m), req).await
}

/// The voice status T3 Code reads on its own as it starts, when this sandbox takes its
/// dictation. A mic click reads the same, so it is answered as last seen while asleep and wakes
/// the sandbox only when never seen.
async fn voice_status(State(m): M, mut req: Request) -> Response {
    if !client_authorized(&m, req.headers()) {
        return unauthorized();
    }
    if m.settled_power().await != Power::Running
        && let Some(status) = m.cache.lock().unwrap().voice_status.clone()
    {
        return Json(status).into_response();
    }
    // The answer is read here, so it must come uncompressed.
    req.headers_mut().remove(header::ACCEPT_ENCODING);
    let answer = forward(State(m.clone()), req).await;
    if answer.status() != StatusCode::OK {
        return answer;
    }
    let (head, body) = answer.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    if let Ok(status) = serde_json::from_slice(&body) {
        m.cache.lock().unwrap().voice_status = Some(status);
        m.save_cache();
    }
    Response::from_parts(head, Body::from(body))
}

/// Renderer traces: periodic, so forwarding them would keep the sandbox from sleeping.
async fn traces() -> Response {
    StatusCode::OK.into_response()
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// Routes whose URL carries its own one-time token instead of a bearer header.
const URL_TOKEN_ROUTES: &[&str] = &[assets::ROUTE, "/api/attachments/upload/"];

fn query_param(query: &str, name: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// `query` without the parameters named in `drop`, the rest byte for byte.
fn query_without(query: &str, drop: &[&str]) -> String {
    query
        .split('&')
        .filter(|pair| {
            !pair.is_empty() && !drop.contains(&pair.split('=').next().unwrap_or_default())
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn target(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_owned()
}

/// What a request was for, safe to log: its first two path segments. The rest of a path can
/// hold a signed token (`/api/assets/<token>/...`), and the query a ticket or a proof.
fn route_name(target: &str) -> String {
    let path = target.split('?').next().unwrap_or_default();
    path.split('/')
        .filter(|s| !s.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join("/")
}

/// How a request goes on to the T3 server on the sandbox.
struct Relay {
    /// Path and query there.
    target: String,
    /// Sent with the proxy's own credential (a DPoP token and a fresh proof), never the
    /// caller's: false only for URL-token requests without a bearer.
    authorize: bool,
    /// Whether it may wake a sleeping sandbox. If not, a sleeping sandbox answers 503.
    wake: bool,
}

/// Everything without a route of its own. Its sockets (dictation, and whatever T3 adds) are
/// opened by a person, so they wake the sandbox.
async fn fallback(
    State(m): M,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    req: Request,
) -> Response {
    match upgrade {
        Ok(upgrade) => media::socket(m, upgrade, &req.into_parts().0, true).await,
        Err(_) => forward(State(m), req).await,
    }
}

/// Plain requests without a route of their own need the live sandbox: wake it if needed, swap
/// in the upstream token. URL-token requests without a bearer may come from any web page: they
/// reach a running sandbox but never wake it or keep it awake. The assets a recorded answer
/// points to are kept as they pass, and answered from what was kept while the sandbox does not
/// run. A sandbox its owner paused wakes only for a POST, the requests a person makes.
async fn forward(State(m): M, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let url_token = URL_TOKEN_ROUTES
        .iter()
        .any(|r| parts.uri.path().starts_with(r));
    let bearer = parts.headers.contains_key(header::AUTHORIZATION);
    let authorized = client_authorized(&m, &parts.headers);
    if (bearer || !url_token) && !authorized {
        return unauthorized();
    }
    let post = parts.method == Method::POST;
    if authorized && post {
        m.user_action();
    }
    let asset = (parts.method == Method::GET && parts.uri.path().starts_with(assets::ROUTE))
        .then(|| assets::path(m.dir(), parts.uri.path()));
    if m.power() != Power::Running
        && let Some(kept) = asset.as_deref().and_then(assets::read)
    {
        return kept;
    }
    let route = Relay {
        target: target(&parts.uri),
        authorize: bearer,
        wake: authorized && (post || m.wakes_for_background()),
    };
    let answer = relay(&m, parts, body, route).await;
    // A range of it (206) is not the asset.
    let keep = asset.filter(|file| {
        answer.status() == StatusCode::OK
            && assets::recorded(m.dir(), &m.cache.lock().unwrap()).contains(file)
    });
    match keep {
        Some(file) => assets::keep(file, answer),
        None => answer,
    }
}

/// Sends the request to the sandbox's server and streams its answer back, unbuffered.
async fn relay(m: &Runtime, parts: Parts, body: Body, route: Relay) -> Response {
    if m.power() != Power::Running
        && let Err(res) = wake(m, route.wake, &route.target).await
    {
        return *res;
    }
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    // A fresh proof for every send: T3 takes each one once.
    let send = || async {
        let url = format!("{}{}", m.upstream_url(), route.target);
        let mut rb = m
            .http
            .request(parts.method.clone(), &url)
            .body(body.clone());
        for (k, v) in &parts.headers {
            // The credential is the route's, and the body's length is what is sent here. A
            // DPoP proof was made for the proxy's URL, not the sandbox's.
            let replaced =
                k == header::AUTHORIZATION || k == header::CONTENT_LENGTH || k.as_str() == "dpop";
            if !replaced && !HOP_BY_HOP.contains(&k.as_str()) {
                rb = rb.header(k, v);
            }
        }
        if route.authorize {
            for (name, value) in m.upstream_headers(parts.method.as_str(), &url).await? {
                rb = rb.header(name, value);
            }
        }
        anyhow::Ok(rb.send().await?)
    };
    let first = send().await;
    // Maybe never reached T3: instabox answers for a sandbox that does not run itself (503
    // while a wake finishes, 503 for one its owner paused, a gateway error). If it fell asleep
    // since the last poll, wake it and send once more.
    let unreached = match &first {
        Ok(r) => matches!(
            r.status(),
            StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
        ),
        Err(e) => e
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_connect),
    };
    let res = if unreached && !matches!(m.refresh_power().await, Ok(Power::Running)) {
        if let Err(res) = wake(m, route.wake, &route.target).await {
            return *res;
        }
        send().await
    } else {
        first
    };
    let res = match res {
        Ok(r) => r,
        // The proxy's session was refused: not the sandbox's fault, and no retry helps.
        Err(e) if e.downcast_ref::<dpop::Refused>().is_some() => {
            return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response();
        }
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    let mut out = Response::builder().status(res.status());
    for (k, v) in res.headers() {
        if !HOP_BY_HOP.contains(&k.as_str()) {
            out = out.header(k, v);
        }
    }
    out.body(Body::from_stream(res.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Wakes the sandbox for a request that may, and answers 503 for one that may not.
/// `why`: what woke it, logged (by route only), as every wake must be explainable.
async fn wake(m: &Runtime, allowed: bool, why: &str) -> Result<(), Box<Response>> {
    if !allowed {
        return Err(Box::new(StatusCode::SERVICE_UNAVAILABLE.into_response()));
    }
    log_info!("waking {} for {}", m.id(), route_name(why));
    m.ensure_awake()
        .await
        .map_err(|e| Box::new((StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")).into_response()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_is_off_unless_asked() {
        assert_eq!(Gate::parse(None), Gate::Off);
        assert_eq!(Gate::parse(Some("")), Gate::Off);
        assert_eq!(Gate::parse(Some("0")), Gate::Off);
        assert_eq!(Gate::parse(Some("true")), Gate::Off);
        assert_eq!(Gate::parse(Some("1")), Gate::Tested);
        assert_eq!(Gate::parse(Some("any")), Gate::Any);
        assert!(!Gate::Off.takes(Some("0.1.13")));
        assert!(Gate::Tested.takes(Some("0.1.13")));
        assert!(
            !Gate::Tested.takes(Some("0.1.10")),
            "no DPoP: only with `any`"
        );
        assert!(!Gate::Tested.takes(None));
        assert!(Gate::Any.takes(Some("0.1.12")));
    }

    #[test]
    fn only_requests_addressed_to_this_computer_by_name_pass() {
        for host in [
            "localhost",
            "localhost:37800",
            "127.0.0.1:37800",
            "[::1]:37800",
            "[::1]",
        ] {
            assert!(is_local_host(host), "{host}");
        }
        for host in [
            "",
            "evil.example",
            "evil.example:37800",
            "localhost.evil.example:37800",
            "127.0.0.1.nip.io:37800",
            "::1",
        ] {
            assert!(!is_local_host(host), "{host}");
        }
    }

    #[test]
    fn logged_routes_hold_no_token() {
        assert_eq!(route_name("/api/assets/eyJ.signed/a.png?x=1"), "api/assets");
        assert_eq!(route_name("/ws?wsTicket=secret&dpop=proof"), "ws");
        assert_eq!(route_name("/"), "");
    }

    #[test]
    fn query_without_keeps_the_rest_byte_for_byte() {
        assert_eq!(
            query_without("a=1&wsTicket=t&b=%20&dpop=p", &["wsTicket", "dpop"]),
            "a=1&b=%20"
        );
        assert_eq!(
            query_param("a=1&wsTicket=t%2Bx", "wsTicket").as_deref(),
            Some("t+x")
        );
    }
}
