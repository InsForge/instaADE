//! The proxy end to end, against a fake T3 server and a fake instabox: pairing through the
//! proxy, the release of an idle sandbox, and what a sleeping sandbox answers or wakes for.

use super::auth::token_hash;
use super::hub::{Hub, MemoryTokenStore, TokenStore};
use super::runtime::{Control, ControlError, Power, Record, Runtime, Tuning};
use crate::api::{Client, Credentials, Machine};
use async_trait::async_trait;
use axum::extract::ws::{Message as WsMsg, WebSocketUpgrade};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const UP_TOKEN: &str = "up-token-0123456789";
const CLIENT_TOKEN: &str = "client-token";

/// A T3 server: answers every request on its socket, counts its sockets.
#[derive(Default)]
struct FakeT3 {
    connections: AtomicUsize,
    open: AtomicUsize,
    tags: Mutex<Vec<String>>,
    queries: Mutex<Vec<String>>,
    /// The shell shows an agent at work.
    working: AtomicBool,
}

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {UP_TOKEN}"))
}

async fn up_descriptor() -> Json<Value> {
    Json(json!({"environmentId": "env-1", "label": "upstream"}))
}

async fn up_session(headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "authenticated": true,
        "scopes": ["orchestration:read", "orchestration:operate"],
        "permissions": {"orchestration": true}
    }))
    .into_response()
}

async fn up_shell(State(t3): State<Arc<FakeT3>>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let threads = if t3.working.load(Ordering::SeqCst) {
        json!([{"id": "t1", "activeRunId": "r1"}])
    } else {
        json!([{"id": "t1", "activeRunId": null}])
    };
    Json(json!({"snapshotSequence": 5, "threads": threads})).into_response()
}

async fn up_ws(
    State(t3): State<Arc<FakeT3>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    t3.queries.lock().unwrap().push(query.unwrap_or_default());
    upgrade.on_upgrade(move |socket| async move {
        t3.connections.fetch_add(1, Ordering::SeqCst);
        t3.open.fetch_add(1, Ordering::SeqCst);
        let (mut tx, mut rx) = socket.split();
        loop {
            let text = match rx.next().await {
                Some(Ok(WsMsg::Text(t))) => t,
                Some(Ok(_)) => continue,
                _ => break,
            };
            for msg in super::session::messages(text.as_str()).unwrap() {
                if msg["_tag"] != "Request" {
                    continue;
                }
                let tag = msg["tag"].as_str().unwrap_or_default().to_owned();
                t3.tags.lock().unwrap().push(tag.clone());
                let id = msg["id"].clone();
                let reply = if tag.contains("subscribe") {
                    json!({"_tag": "Chunk", "requestId": id, "values": [{"kind": "snapshot"}]})
                } else {
                    json!({"_tag": "Exit", "requestId": id, "exit": {"_tag": "Success", "value": {"ok": tag}}})
                };
                if tx.send(WsMsg::text(reply.to_string())).await.is_err() {
                    break;
                }
            }
        }
        t3.open.fetch_sub(1, Ordering::SeqCst);
    })
}

async fn fake_t3() -> (Arc<FakeT3>, String) {
    let t3 = Arc::new(FakeT3::default());
    let app = Router::new()
        .route("/.well-known/t3/environment", get(up_descriptor))
        .route("/api/auth/session", get(up_session))
        .route("/api/orchestration/shell", get(up_shell))
        .route("/ws", get(up_ws))
        .with_state(t3.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (t3, url)
}

/// instabox: the sandbox's state, and wakes counted.
struct FakeControl {
    state: Mutex<String>,
    auto_suspended: bool,
    wakes: AtomicUsize,
    revision: std::sync::atomic::AtomicI64,
}

impl FakeControl {
    fn new(state: &str, auto_suspended: bool) -> Arc<Self> {
        Arc::new(FakeControl {
            state: Mutex::new(state.into()),
            auto_suspended,
            wakes: AtomicUsize::new(0),
            revision: std::sync::atomic::AtomicI64::new(1),
        })
    }

    fn wakes(&self) -> usize {
        self.wakes.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Control for FakeControl {
    async fn machine(&self, id: &str) -> Result<Machine, ControlError> {
        let state = self.state.lock().unwrap().clone();
        let suspended =
            (state != "running" && self.auto_suspended).then_some("2026-11-27T10:00:00Z");
        Ok(serde_json::from_value(json!({
            "id": id, "name": "qa", "state": state,
            "template": {"id": "agent-browser"}, "spec": {"vcpus": 2, "memMiB": 4096},
            "revision": self.revision.load(Ordering::SeqCst), "autoSuspendedAt": suspended
        }))
        .unwrap())
    }

    async fn wake(&self, _id: &str) -> Result<(), String> {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        self.revision.fetch_add(1, Ordering::SeqCst);
        *self.state.lock().unwrap() = "running".into();
        Ok(())
    }
}

fn tuning(idle_after: Duration) -> Tuning {
    Tuning {
        idle_after,
        poll: Duration::from_millis(50),
        tick: Duration::from_millis(20),
        busy_recheck: Duration::from_millis(100),
    }
}

struct Proxy {
    t3: Arc<FakeT3>,
    control: Arc<FakeControl>,
    rt: Arc<Runtime>,
    port: u16,
    _dir: tempfile::TempDir,
}

/// A proxy on an ephemeral port for a sandbox in `state`, its runtime running.
async fn proxy(state: &str, auto_suspended: bool, idle_after: Duration) -> Proxy {
    let (t3, upstream_url) = fake_t3().await;
    let control = FakeControl::new(state, auto_suspended);
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let record = Record {
        id: "m_test".into(),
        name: "qa".into(),
        port,
        environment_id: "env-1".into(),
        upstream_url,
        client_tokens: vec![token_hash(CLIENT_TOKEN)],
    };
    let rt = Arc::new(Runtime::new(
        dir.path().join("m_test"),
        record,
        UP_TOKEN.into(),
        control.clone(),
        tuning(idle_after),
    ));
    if state == "running" {
        rt.prime().await.unwrap();
    }
    tokio::spawn(super::serve(listener, rt.clone()));
    tokio::spawn(rt.clone().run());
    Proxy {
        t3,
        control,
        rt,
        port,
        _dir: dir,
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn t3_code(port: u16) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/ws?clientSurface=desktop")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {CLIENT_TOKEN}").parse().unwrap(),
    );
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn send(ws: &mut Ws, msg: Value) {
    ws.send(Message::text(msg.to_string())).await.unwrap();
}

fn request(id: &str, tag: &str, payload: Value) -> Value {
    json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []})
}

/// The next text frame as JSON, or the close code as `{"close": code}`.
async fn next(ws: &mut Ws) -> Value {
    let read = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
                Some(Ok(Message::Close(frame))) => {
                    return json!({"close": frame.map(|f| u16::from(f.code))});
                }
                Some(Ok(_)) => continue,
                other => panic!("socket ended: {other:?}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("an answer within 10 s")
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn an_idle_sandbox_is_released_and_the_next_action_takes_it_back() {
    let p = proxy("running", true, Duration::from_millis(400)).await;
    let mut ws = t3_code(p.port).await;
    send(
        &mut ws,
        request("1", "orchestration.subscribeShell", json!({})),
    )
    .await;
    let first = next(&mut ws).await;
    assert_eq!(
        (first["_tag"].as_str(), first["requestId"].as_str()),
        (Some("Chunk"), Some("1"))
    );
    assert_eq!(p.t3.open.load(Ordering::SeqCst), 1);

    // No action for `idle_after`: the upstream socket closes, so the port lease ends.
    eventually("the upstream socket closes", || {
        p.t3.open.load(Ordering::SeqCst) == 0
    })
    .await;
    assert!(p.rt.released());
    assert_eq!(p.rt.power(), Power::Stopped);
    assert_eq!(p.control.wakes(), 0);
    assert!(p.rt.fresh_capture().is_some(), "captured as it let go");

    // T3 Code's own upkeep is answered here and takes nothing back.
    send(&mut ws, json!({"_tag": "Ping"})).await;
    assert_eq!(next(&mut ws).await, json!({"_tag": "Pong"}));
    send(
        &mut ws,
        request("2", "server.reportClientActivity", json!({"focused": true})),
    )
    .await;
    let report = next(&mut ws).await;
    assert_eq!(report["requestId"], "2");
    assert_eq!(report["exit"]["_tag"], "Success");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(p.t3.connections.load(Ordering::SeqCst), 1);
    assert_eq!(p.control.wakes(), 0);

    // A person acts: the proxy takes the sandbox back and the action goes through.
    let archive = json!({"type": "thread.archive", "commandId": "c1", "threadId": "t1"});
    send(
        &mut ws,
        request("3", "orchestration.dispatchCommand", archive),
    )
    .await;
    let done = next(&mut ws).await;
    assert_eq!(done["requestId"], "3");
    assert_eq!(done["exit"]["value"]["ok"], "orchestration.dispatchCommand");
    // Then the socket closes so T3 Code resubscribes against the live server.
    assert_eq!(next(&mut ws).await, json!({"close": 1012}));
    assert_eq!(p.control.wakes(), 1);
    assert_eq!(p.t3.connections.load(Ordering::SeqCst), 2);
    assert!(!p.rt.released());
}

#[tokio::test]
async fn a_sandbox_with_an_agent_working_is_held() {
    let p = proxy("running", true, Duration::from_millis(200)).await;
    p.t3.working.store(true, Ordering::SeqCst);
    let mut ws = t3_code(p.port).await;
    send(
        &mut ws,
        request("1", "orchestration.subscribeShell", json!({})),
    )
    .await;
    next(&mut ws).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        p.t3.open.load(Ordering::SeqCst),
        1,
        "held while the agent works"
    );
    assert!(!p.rt.released());
    p.t3.working.store(false, Ordering::SeqCst);
    eventually("released once the run ends", || {
        p.t3.open.load(Ordering::SeqCst) == 0
    })
    .await;
}

#[tokio::test]
async fn a_sleeping_sandbox_answers_upkeep_here_and_wakes_for_the_unknown() {
    let p = proxy("suspended", true, Duration::from_secs(60)).await;
    let mut ws = t3_code(p.port).await;
    send(&mut ws, json!({"_tag": "Ping"})).await;
    assert_eq!(next(&mut ws).await, json!({"_tag": "Pong"}));
    for (id, tag) in [
        ("1", "server.reportClientActivity"),
        ("2", "preview.reportStatus"),
        ("3", "pullRequests.invalidate"),
    ] {
        send(&mut ws, request(id, tag, json!({}))).await;
        let answer = next(&mut ws).await;
        assert_eq!(
            (
                answer["requestId"].as_str(),
                answer["exit"]["_tag"].as_str()
            ),
            (Some(id), Some("Success")),
            "{tag}"
        );
    }
    send(&mut ws, request("4", "server.probe", json!({}))).await;
    assert_eq!(next(&mut ws).await["requestId"], "4");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(p.control.wakes(), 0);
    assert_eq!(p.t3.connections.load(Ordering::SeqCst), 0);

    // An RPC the proxy does not know wakes the sandbox, which is always safe.
    send(&mut ws, request("5", "something.new", json!({}))).await;
    let answer = next(&mut ws).await;
    assert_eq!(answer["requestId"], "5");
    assert_eq!(answer["exit"]["value"]["ok"], "something.new");
    assert_eq!(p.control.wakes(), 1);
    assert!(
        p.t3.tags
            .lock()
            .unwrap()
            .contains(&"something.new".to_string())
    );
}

#[tokio::test]
async fn a_sandbox_its_owner_paused_wakes_only_for_a_person() {
    let p = proxy("suspended", false, Duration::from_secs(60)).await;
    let mut ws = t3_code(p.port).await;
    // What T3 Code asks on its own as it connects waits, unanswered.
    send(
        &mut ws,
        request("1", "orchestration.subscribeShell", json!({})),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(p.control.wakes(), 0);
    // A person's action wakes it; what waited goes upstream too.
    let archive = json!({"type": "thread.archive", "commandId": "c1", "threadId": "t1"});
    send(
        &mut ws,
        request("2", "orchestration.dispatchCommand", archive),
    )
    .await;
    let mut seen = Vec::new();
    for _ in 0..2 {
        seen.push(
            next(&mut ws).await["requestId"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        );
    }
    seen.sort();
    assert_eq!(seen, vec!["1", "2"]);
    assert_eq!(p.control.wakes(), 1);
}

struct Creds;

#[async_trait]
impl Credentials for Creds {
    async fn bearer(&self) -> crate::api::Result<String> {
        Ok("iba_x".into())
    }
    async fn refreshed(&self) -> crate::api::Result<String> {
        Ok("iba_x".into())
    }
}

#[tokio::test]
async fn pairing_goes_through_the_proxy() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (t3, upstream_url) = fake_t3().await;
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/machines/m_test/ports"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"url": upstream_url})))
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/machines/m_test/exec"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"stdout": format!("{UP_TOKEN}\n"), "stderr": "", "exitCode": 0}),
            ),
        )
        .expect(1)
        .mount(&api)
        .await;
    let client = Client::new(url::Url::parse(&api.uri()).unwrap(), Arc::new(Creds));
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(MemoryTokenStore::default());
    let control = FakeControl::new("running", true);
    let hub = Hub::new(
        dir.path().to_path_buf(),
        control,
        tokens.clone(),
        tuning(Duration::from_secs(60)),
    );

    let link = hub.pair_link(&client, "m_test", "qa").await.unwrap();
    // The upstream session is the keychain's, issued once on the sandbox.
    assert_eq!(tokens.load("m_test").unwrap().as_deref(), Some(UP_TOKEN));
    let pairing: String =
        url::form_urlencoded::parse(link.strip_prefix("t3code://pair?").unwrap().as_bytes())
            .find(|(k, _)| k == "pairingUrl")
            .unwrap()
            .1
            .into_owned();
    let (base, code) = pairing.split_once("/pair#token=").unwrap();
    let port: u16 = base
        .strip_prefix("http://localhost:")
        .unwrap()
        .parse()
        .unwrap();
    assert!(port >= super::hub::BASE_PORT);
    let at = |p: &str| format!("http://127.0.0.1:{port}{p}");
    let http = reqwest::Client::new();

    // What T3 Code reads before it shows Confirm: the sandbox's environment, under its name.
    let d: Value = http
        .get(at("/.well-known/t3/environment"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (d["environmentId"].as_str(), d["label"].as_str()),
        (Some("env-1"), Some("qa"))
    );

    // Confirm: the code becomes the proxy's own token, once.
    let form = [
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:token-exchange",
        ),
        ("subject_token", code),
    ];
    let res = http
        .post(at("/oauth/token"))
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let token: Value = res.json().await.unwrap();
    let access = token["access_token"].as_str().unwrap().to_owned();
    assert_eq!(token["scope"], "orchestration:read orchestration:operate");
    let again = http
        .post(at("/oauth/token"))
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 400, "a code works once");

    // The token opens the proxy, as `Bearer` or as `DPoP` (T3 Code 0.1.11 and later).
    for scheme in ["Bearer", "DPoP"] {
        let res = http
            .get(at("/api/auth/session"))
            .header("authorization", format!("{scheme} {access}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{scheme}");
    }
    let none = http.get(at("/api/auth/session")).send().await.unwrap();
    assert_eq!(none.status(), 401);
    // Only requests addressed to this computer by name: no DNS rebinding.
    let rebound = http
        .get(at("/.well-known/t3/environment"))
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403);

    // The socket opens with a ticket; its DPoP proof stays here.
    let ticket: Value = http
        .post(at("/api/auth/websocket-ticket/rpc"))
        .header("authorization", format!("DPoP {access}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = format!(
        "ws://127.0.0.1:{port}/ws?wsTicket={}&dpop=proof.for.the.proxy&clientSurface=desktop",
        ticket["ticket"].as_str().unwrap()
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    send(&mut ws, request("1", "server.getConfig", json!({}))).await;
    assert_eq!(next(&mut ws).await["requestId"], "1");
    let queries = t3.queries.lock().unwrap().clone();
    assert_eq!(
        queries,
        vec!["orchestrationProtocol=2&clientSurface=desktop".to_string()]
    );

    // What is kept on disk holds no secret.
    let record = std::fs::read_to_string(dir.path().join("m_test").join("record.json")).unwrap();
    assert!(!record.contains(&access) && !record.contains(UP_TOKEN));
    assert!(record.contains(&token_hash(&access)));

    // Open again: the same port, no second session issued (the exec mock expects one call).
    let link2 = hub.pair_link(&client, "m_test", "qa").await.unwrap();
    assert!(link2.contains(&format!("localhost%3A{port}%2Fpair")));
}
