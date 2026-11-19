//! HTTP client for the instabox API.

pub mod types;

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

pub use types::*;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const EXEC_TIMEOUT: Duration = Duration::from_secs(610);
const PAGE_LIMIT: &str = "200";

/// An API failure. `status` is 0 when no HTTP response was involved
/// (network failure, bad body, or a failed operation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub retry_after: Option<u64>,
}

impl Error {
    fn local(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Error {
            status: 0,
            code: code.to_string(),
            message: message.into(),
            retryable,
            retry_after: None,
        }
    }

    fn from_body(status: u16, body: ApiErrorBody) -> Self {
        Error {
            status,
            code: body.code,
            message: body.message,
            retryable: body.retryable,
            retry_after: body.retry_after_seconds,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.status, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Source of the bearer token. `refreshed` forces a refresh and returns the new token.
#[async_trait]
pub trait Credentials: Send + Sync {
    async fn bearer(&self) -> Result<String>;
    async fn refreshed(&self) -> Result<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Suspend,
    Resume,
    Start,
    Stop,
}

impl Action {
    fn segment(self) -> &'static str {
        match self {
            Action::Suspend => "suspend",
            Action::Resume => "resume",
            Action::Start => "start",
            Action::Stop => "stop",
        }
    }
}

// Short-lived return value; boxing would only make callers noisier.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum PatchResult {
    Machine(Machine),
    Operation(Operation),
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    creds: Arc<dyn Credentials>,
}

struct Req<'a> {
    method: Method,
    segments: Vec<&'a str>,
    query: Vec<(&'a str, String)>,
    body: Option<serde_json::Value>,
    idempotency_key: Option<&'a str>,
    confirm_delete: Option<&'a str>,
    timeout: Duration,
}

impl<'a> Req<'a> {
    fn new(method: Method, segments: &[&'a str]) -> Self {
        Req {
            method,
            segments: segments.to_vec(),
            query: Vec::new(),
            body: None,
            idempotency_key: None,
            confirm_delete: None,
            timeout: CALL_TIMEOUT,
        }
    }

    fn json<T: Serialize>(mut self, body: &T) -> Self {
        self.body = Some(serde_json::to_value(body).expect("request body serializes"));
        self
    }
}

impl Client {
    pub fn new(base: Url, creds: Arc<dyn Credentials>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("instabox-bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client builds");
        Client { http, base, creds }
    }

    fn url(&self, req: &Req<'_>) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| Error::local("bad_base_url", "base URL cannot be a base", false))?
            .pop_if_empty()
            .extend(&req.segments);
        if !req.query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(req.query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        Ok(url)
    }

    async fn send_once(
        &self,
        req: &Req<'_>,
        token: &str,
    ) -> Result<(StatusCode, Vec<u8>, Option<u64>)> {
        let mut rb = self
            .http
            .request(req.method.clone(), self.url(req)?)
            .bearer_auth(token)
            .timeout(req.timeout);
        if let Some(k) = req.idempotency_key {
            rb = rb.header("Idempotency-Key", k);
        }
        if let Some(id) = req.confirm_delete {
            rb = rb.header("X-Confirm-Delete", id);
        }
        if let Some(body) = &req.body {
            rb = rb.json(body);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| Error::local("network", e.to_string(), true))?;
        let status = resp.status();
        let header_retry = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::local("network", e.to_string(), true))?;
        Ok((status, bytes.to_vec(), header_retry))
    }

    /// Sends the request; on a 401 refreshes the token once and retries once.
    async fn send(&self, req: Req<'_>) -> Result<(StatusCode, Vec<u8>)> {
        let token = self.creds.bearer().await?;
        let mut out = self.send_once(&req, &token).await?;
        if out.0 == StatusCode::UNAUTHORIZED {
            let token = self.creds.refreshed().await?;
            out = self.send_once(&req, &token).await?;
        }
        let (status, bytes, header_retry) = out;
        if status.is_success() {
            return Ok((status, bytes));
        }
        Err(parse_error(status, &bytes, header_retry))
    }

    async fn call<T: DeserializeOwned>(&self, req: Req<'_>) -> Result<T> {
        let (_, bytes) = self.send(req).await?;
        decode(&bytes)
    }

    pub async fn me(&self) -> Result<Me> {
        self.call(Req::new(Method::GET, &["v1", "me"])).await
    }

    pub async fn orgs(&self) -> Result<Vec<Org>> {
        #[derive(serde::Deserialize)]
        struct R {
            orgs: Vec<Org>,
        }
        let r: R = self.call(Req::new(Method::GET, &["v1", "orgs"])).await?;
        Ok(r.orgs)
    }

    /// Limits and usage for the personal org, or for `org` when given.
    pub async fn usage(&self, org: Option<&str>) -> Result<Usage> {
        let mut req = Req::new(Method::GET, &["v1", "usage"]);
        if let Some(o) = org {
            req.query.push(("org", o.to_string()));
        }
        self.call(req).await
    }

    pub async fn templates(&self) -> Result<Vec<Template>> {
        #[derive(serde::Deserialize)]
        struct R {
            templates: Vec<Template>,
        }
        let r: R = self
            .call(Req::new(Method::GET, &["v1", "templates"]))
            .await?;
        Ok(r.templates)
    }

    pub async fn snapshots(&self) -> Result<Vec<Snapshot>> {
        #[derive(serde::Deserialize)]
        struct R {
            snapshots: Vec<Snapshot>,
        }
        let r: R = self
            .call(Req::new(Method::GET, &["v1", "snapshots"]))
            .await?;
        Ok(r.snapshots)
    }

    /// Lists all machines, following `nextCursor`. Listing does not count as use.
    pub async fn machines(&self) -> Result<Vec<Machine>> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct R {
            machines: Vec<Machine>,
            #[serde(default)]
            next_cursor: Option<String>,
        }
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut req = Req::new(Method::GET, &["v1", "machines"]);
            req.query.push(("limit", PAGE_LIMIT.to_string()));
            if let Some(c) = &cursor {
                req.query.push(("cursor", c.clone()));
            }
            let page: R = self.call(req).await?;
            all.extend(page.machines);
            match page.next_cursor.filter(|c| !c.is_empty()) {
                Some(c) => cursor = Some(c),
                None => return Ok(all),
            }
        }
    }

    pub async fn machine(&self, id: &str) -> Result<Machine> {
        self.call(Req::new(Method::GET, &["v1", "machines", id]))
            .await
    }

    pub async fn create(&self, req: &CreateMachineRequest, key: &str) -> Result<Operation> {
        let mut r = Req::new(Method::POST, &["v1", "machines"]).json(req);
        r.idempotency_key = Some(key);
        self.call(r).await
    }

    pub async fn patch(
        &self,
        id: &str,
        req: &UpdateMachineRequest,
        key: &str,
    ) -> Result<PatchResult> {
        let mut r = Req::new(Method::PATCH, &["v1", "machines", id]).json(req);
        r.idempotency_key = Some(key);
        let (status, bytes) = self.send(r).await?;
        if status == StatusCode::ACCEPTED {
            Ok(PatchResult::Operation(decode(&bytes)?))
        } else {
            Ok(PatchResult::Machine(decode(&bytes)?))
        }
    }

    pub async fn action(&self, id: &str, action: Action, key: &str) -> Result<Operation> {
        let mut r = Req::new(Method::POST, &["v1", "machines", id, action.segment()])
            .json(&serde_json::json!({}));
        r.idempotency_key = Some(key);
        self.call(r).await
    }

    pub async fn delete(&self, id: &str, key: &str) -> Result<Operation> {
        let mut r = Req::new(Method::DELETE, &["v1", "machines", id]);
        r.idempotency_key = Some(key);
        r.confirm_delete = Some(id);
        self.call(r).await
    }

    /// Long-polls the operation until it ends. Returns the machine on success
    /// and the operation's error on failure.
    pub async fn wait(&self, op: &Operation) -> Result<Machine> {
        let mut cur = op.clone();
        loop {
            match cur.state.as_str() {
                "succeeded" => {
                    return match cur.machine {
                        Some(m) => Ok(m),
                        None => self.machine(&cur.machine_id).await,
                    };
                }
                "failed" => {
                    let body = cur.error.unwrap_or_else(|| ApiErrorBody {
                        code: "operation_failed".into(),
                        message: "operation failed".into(),
                        ..Default::default()
                    });
                    return Err(Error::from_body(0, body));
                }
                _ => {}
            }
            let started = Instant::now();
            let mut req = Req::new(Method::GET, &["v1", "operations", &op.id]);
            req.query.push(("wait", "25s".to_string()));
            cur = self.call(req).await?;
            // Guard against a tight loop if the server answers a poll at once.
            if !matches!(cur.state.as_str(), "succeeded" | "failed")
                && started.elapsed() < Duration::from_secs(1)
            {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }

    pub async fn exec(&self, id: &str, req: &ExecRequest) -> Result<ExecResult> {
        let mut r = Req::new(Method::POST, &["v1", "machines", id, "exec"]).json(req);
        r.timeout = EXEC_TIMEOUT;
        self.call(r).await
    }

    pub async fn publish_port(&self, id: &str, port: u16) -> Result<PublicPort> {
        let r = Req::new(Method::POST, &["v1", "machines", id, "ports"])
            .json(&serde_json::json!({ "port": port }));
        self.call(r).await
    }

    pub async fn ssh_token(&self, id: &str, name: &str, ttl_seconds: u64) -> Result<SshToken> {
        let r = Req::new(Method::POST, &["v1", "machines", id, "ssh-tokens"])
            .json(&serde_json::json!({ "name": name, "ttlSeconds": ttl_seconds }));
        self.call(r).await
    }

    /// Needs a sign-in session; an API key cannot create keys.
    pub async fn create_api_key(
        &self,
        name: &str,
        scopes: &[&str],
        expires_in_days: u32,
    ) -> Result<CreatedApiKey> {
        let r = Req::new(Method::POST, &["v1", "keys"]).json(&serde_json::json!({
            "name": name, "scopes": scopes, "expiresInDays": expires_in_days
        }));
        self.call(r).await
    }
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|e| Error::local("bad_response", format!("cannot read response: {e}"), false))
}

fn parse_error(status: StatusCode, bytes: &[u8], header_retry: Option<u64>) -> Error {
    #[derive(serde::Deserialize)]
    struct Wrap {
        error: ApiErrorBody,
    }
    let code = status.as_u16();
    match serde_json::from_slice::<Wrap>(bytes) {
        Ok(w) => {
            let mut e = Error::from_body(code, w.error);
            if e.retry_after.is_none() {
                e.retry_after = header_retry;
            }
            e
        }
        Err(_) => Error {
            status: code,
            code: format!("http_{code}"),
            message: String::from_utf8_lossy(bytes).chars().take(200).collect(),
            retryable: status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS,
            retry_after: header_retry,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Creds {
        refreshes: AtomicUsize,
    }

    #[async_trait]
    impl Credentials for Creds {
        async fn bearer(&self) -> Result<String> {
            Ok("iba_x".into())
        }
        async fn refreshed(&self) -> Result<String> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Ok("iba_new".into())
        }
    }

    fn client(server: &MockServer) -> (Client, Arc<Creds>) {
        let creds = Arc::new(Creds {
            refreshes: AtomicUsize::new(0),
        });
        let c = Client::new(Url::parse(&server.uri()).unwrap(), creds.clone());
        (c, creds)
    }

    fn machine_json(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": "n", "state": "running", "revision": 3, "unknownField": 1,
            "template": {"id": "t", "version": 1, "digest": "d"},
            "spec": {"vcpus": 2, "memMiB": 4096, "persistentDiskGiB": 10},
            "runtime": {"ready": true},
            "autoSuspendAfterSeconds": null, "maxRunMinutes": -1,
            "runStartedAt": null, "cooldownUntil": null,
            "metadata": {}, "orgId": "o1", "free": false, "operationId": null
        })
    }

    fn op_json(state: &str, machine: Option<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({"id": "op1", "machineId": "m1", "type": "create", "state": state,
            "error": null, "machine": machine})
    }

    fn create_req() -> CreateMachineRequest {
        CreateMachineRequest {
            name: "n".into(),
            ..Default::default()
        }
    }

    fn unauth() -> ResponseTemplate {
        ResponseTemplate::new(401).set_body_json(serde_json::json!(
            {"error": {"code": "unauthenticated", "message": "m", "retryable": false}}))
    }

    #[tokio::test]
    async fn usage_reads_limits_with_org_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/usage"))
            .and(query_param("org", "o9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "limits": {"maxMachines": 1, "maxSnapshots": 2}, "org": {"id": "o9", "tier": "free"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let u = c.usage(Some("o9")).await.unwrap();
        assert_eq!(u.limits.max_machines, Some(1));
        assert_eq!(u.org.unwrap().id, "o9");
    }

    #[tokio::test]
    async fn sends_bearer_and_idempotency_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/machines"))
            .and(header("Authorization", "Bearer iba_x"))
            .and(header("Idempotency-Key", "k1"))
            .respond_with(ResponseTemplate::new(202).set_body_json(op_json("queued", None)))
            .expect(1)
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let op = c.create(&create_req(), "k1").await.unwrap();
        assert_eq!(op.id, "op1");
        assert_eq!(op.r#type, "create");
    }

    #[tokio::test]
    async fn refreshes_once_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .and(header("Authorization", "Bearer iba_x"))
            .respond_with(unauth())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .and(header("Authorization", "Bearer iba_new"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"userId": 7, "login": "me", "org": {"id": "o1", "tier": "free", "kind": "personal"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (c, creds) = client(&server);
        let me = c.me().await.unwrap();
        assert_eq!(me.login, "me");
        assert_eq!(me.org.unwrap().tier, "free");
        assert_eq!(creds.refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn second_401_is_returned_without_another_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .respond_with(unauth())
            .expect(2)
            .mount(&server)
            .await;
        let (c, creds) = client(&server);
        let err = c.me().await.unwrap_err();
        assert_eq!(err.status, 401);
        assert_eq!(creds.refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn parses_error_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/resume"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!(
                {"error":{"code":"machine_cooldown","message":"m","retryable":true,
                "retryAfterSeconds":120,"requestId":"r","operationId":null}})))
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let err = c.action("m1", Action::Resume, "k").await.unwrap_err();
        assert_eq!(
            err,
            Error {
                status: 409,
                code: "machine_cooldown".into(),
                message: "m".into(),
                retryable: true,
                retry_after: Some(120)
            }
        );
    }

    #[tokio::test]
    async fn waits_on_operation() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/operations/op1"))
            .and(query_param("wait", "25s"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(op_json("succeeded", Some(machine_json("m1")))),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let queued: Operation = serde_json::from_value(op_json("queued", None)).unwrap();
        let m = c.wait(&queued).await.unwrap();
        assert_eq!(m.id, "m1");
        assert_eq!(m.spec.mem_mib, 4096);
    }

    #[tokio::test]
    async fn wait_returns_operation_error_on_failure() {
        let server = MockServer::start().await;
        let mut body = op_json("failed", None);
        body["error"] =
            serde_json::json!({"code": "no_capacity", "message": "full", "retryable": true});
        Mock::given(method("GET"))
            .and(path("/v1/operations/op1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let queued: Operation = serde_json::from_value(op_json("queued", None)).unwrap();
        let err = c.wait(&queued).await.unwrap_err();
        assert_eq!(err.code, "no_capacity");
        assert!(err.retryable);
    }

    #[tokio::test]
    async fn wait_fetches_machine_when_absent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/machines/m1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(machine_json("m1")))
            .expect(1)
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let done: Operation = serde_json::from_value(op_json("succeeded", None)).unwrap();
        assert_eq!(c.wait(&done).await.unwrap().id, "m1");
    }

    #[tokio::test]
    async fn lists_all_pages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/machines"))
            .and(query_param("cursor", "c2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"machines": [machine_json("m2")], "nextCursor": null}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/machines"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"machines": [machine_json("m1")], "nextCursor": "c2"}),
            ))
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let ids: Vec<String> = c
            .machines()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["m1", "m2"]);
    }

    #[tokio::test]
    async fn delete_sends_confirm() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/machines/m1"))
            .and(header("X-Confirm-Delete", "m1"))
            .and(header("Idempotency-Key", "k9"))
            .respond_with(ResponseTemplate::new(202).set_body_json(op_json("queued", None)))
            .expect(1)
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        assert_eq!(c.delete("m1", "k9").await.unwrap().id, "op1");
    }

    #[tokio::test]
    async fn patch_maps_status_to_variant() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/machines/m1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(machine_json("m1")))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/v1/machines/m2"))
            .respond_with(ResponseTemplate::new(202).set_body_json(op_json("queued", None)))
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let req = UpdateMachineRequest::default();
        assert!(matches!(
            c.patch("m1", &req, "k").await.unwrap(),
            PatchResult::Machine(_)
        ));
        assert!(matches!(
            c.patch("m2", &req, "k").await.unwrap(),
            PatchResult::Operation(_)
        ));
    }

    #[tokio::test]
    async fn exec_and_ssh_token_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"stdout":"o","stderr":"","exitCode":0,"timedOut":false,"truncated":false})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ssh-tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!(
                {"id":"i","name":"n","host":"h","port":22,"token":"ibssh_x",
                "command":"ssh ibssh_x@h","expiresAt":"2026-01-01T00:00:00Z"})))
            .mount(&server)
            .await;
        let (c, _) = client(&server);
        let r = c
            .exec(
                "m1",
                &ExecRequest {
                    cmd: "true".into(),
                    timeout_seconds: None,
                },
            )
            .await
            .unwrap();
        assert_eq!((r.stdout.as_str(), r.exit_code), ("o", 0));
        assert_eq!(c.ssh_token("m1", "n", 60).await.unwrap().port, 22);
    }
}
