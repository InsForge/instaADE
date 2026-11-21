//! Device sign-in, token refresh and token storage.

use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::{Mutex, watch};
use url::Url;

use crate::api::{self, ApiErrorBody, Credentials, DeviceStart, Tokens};

const KEYCHAIN_ACCOUNT: &str = "tokens";
const REFRESH_MARGIN_SECS: i64 = 60;
const SLOW_DOWN_SECS: u64 = 5;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// The background logout call gives up after this long.
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    GitHub,
    Google,
}

impl Provider {
    fn as_str(self) -> &'static str {
        match self {
            Provider::GitHub => "github",
            Provider::Google => "google",
        }
    }
}

/// Why sign-in failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The device code expired before the user approved.
    Expired,
    /// The user denied the sign-in.
    Denied,
    Api(api::Error),
    /// The token store failed.
    Store(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Expired => write!(f, "The sign-in code expired. Start again."),
            Error::Denied => write!(f, "Sign-in was denied."),
            Error::Api(e) => write!(f, "{e}"),
            Error::Store(m) => write!(f, "token store: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<api::Error> for Error {
    fn from(e: api::Error) -> Self {
        Error::Api(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Where the session tokens live between runs.
pub trait TokenStore: Send + Sync {
    fn load(&self) -> std::result::Result<Option<Tokens>, String>;
    fn save(&self, tokens: &Tokens) -> std::result::Result<(), String>;
    fn clear(&self) -> std::result::Result<(), String>;
}

/// The OS keychain.
pub struct KeyringStore;

impl KeyringStore {
    fn entry() -> std::result::Result<keyring::Entry, String> {
        keyring::Entry::new(crate::keychain_service(), KEYCHAIN_ACCOUNT).map_err(|e| e.to_string())
    }
}

impl TokenStore for KeyringStore {
    fn load(&self) -> std::result::Result<Option<Tokens>, String> {
        match Self::entry()?.get_password() {
            Ok(json) => Ok(serde_json::from_str(&json).ok()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn save(&self, tokens: &Tokens) -> std::result::Result<(), String> {
        let json = serde_json::to_string(tokens).map_err(|e| e.to_string())?;
        Self::entry()?
            .set_password(&json)
            .map_err(|e| e.to_string())
    }

    fn clear(&self) -> std::result::Result<(), String> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// In-memory store, for tests.
#[derive(Default)]
pub struct MemoryStore(StdMutex<Option<Tokens>>);

impl MemoryStore {
    pub fn with(tokens: Tokens) -> Self {
        MemoryStore(StdMutex::new(Some(tokens)))
    }
}

impl TokenStore for MemoryStore {
    fn load(&self) -> std::result::Result<Option<Tokens>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, tokens: &Tokens) -> std::result::Result<(), String> {
        *self.0.lock().unwrap() = Some(tokens.clone());
        Ok(())
    }
    fn clear(&self) -> std::result::Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

struct Stored {
    base: Url,
    http: reqwest::Client,
    store: Arc<dyn TokenStore>,
    tokens: Mutex<Tokens>,
    /// A copy of the current access token, readable while a refresh holds `tokens`.
    access: StdMutex<String>,
    /// Taken around "check `dead`, then save" and "set `dead`, then clear", so a refresh
    /// that finishes after a sign-out can never write tokens back.
    gate: StdMutex<()>,
    /// Counts completed refreshes; lets a waiting caller see that another refreshed.
    generation: AtomicU64,
    /// Set when the session ends: the user signed out, another session replaced it, or a
    /// refresh answered 401.
    dead: AtomicBool,
    /// Turns true when `dead` is set, so the app learns of an end it did not cause.
    ended: watch::Sender<bool>,
    /// The last failure to save rotated tokens; the running session still works.
    save_error: StdMutex<Option<String>>,
}

enum Kind {
    ApiKey(String),
    Tokens(Box<Stored>),
}

/// A signed-in session. Implements `Credentials`.
pub struct Session {
    kind: Kind,
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("instabox-bridge/", env!("CARGO_PKG_VERSION")))
        .timeout(CALL_TIMEOUT)
        .build()
        .expect("reqwest client builds")
}

fn local_err(code: &str, message: impl Into<String>, retryable: bool) -> api::Error {
    api::Error {
        status: 0,
        code: code.to_string(),
        message: message.into(),
        retryable,
        retry_after: None,
    }
}

fn no_refresh() -> api::Error {
    local_err("api_key_no_refresh", "API keys do not refresh", false)
}

fn signed_out() -> api::Error {
    api::Error {
        status: 401,
        code: "unauthenticated".into(),
        message: "The session ended. Sign in again.".into(),
        retryable: false,
        retry_after: None,
    }
}

fn endpoint(base: &Url, tail: &[&str]) -> std::result::Result<Url, api::Error> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| local_err("bad_base_url", "base URL cannot be a base", false))?
        .pop_if_empty()
        .extend(["v1", "auth"])
        .extend(tail);
    Ok(url)
}

/// POSTs a JSON body; 2xx decodes to `T`, anything else becomes an `api::Error`.
async fn post<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: Url,
    body: serde_json::Value,
) -> std::result::Result<T, api::Error> {
    let resp = http
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| local_err("network", e.to_string(), true))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| local_err("network", e.to_string(), true))?;
    if status.is_success() {
        return serde_json::from_slice(&bytes)
            .map_err(|e| local_err("bad_response", format!("cannot read response: {e}"), false));
    }
    #[derive(serde::Deserialize)]
    struct Wrap {
        error: ApiErrorBody,
    }
    let code = status.as_u16();
    Err(match serde_json::from_slice::<Wrap>(&bytes) {
        Ok(w) => api::Error {
            status: code,
            code: w.error.code,
            message: w.error.message,
            retryable: w.error.retryable,
            retry_after: w.error.retry_after_seconds,
        },
        Err(_) => api::Error {
            status: code,
            code: format!("http_{code}"),
            message: String::from_utf8_lossy(&bytes).chars().take(200).collect(),
            retryable: status.is_server_error(),
            retry_after: None,
        },
    })
}

/// Asks the server for a device code. The app layer opens `start.browser_url()`.
pub async fn start(base: &Url, provider: Provider) -> Result<DeviceStart> {
    let url = endpoint(base, &["device"])?;
    let body = serde_json::json!({ "provider": provider.as_str() });
    Ok(post(&http_client(), url, body).await?)
}

/// Polls until the user approves. The session's tokens are not saved yet: the app saves them
/// with `Session::save` when the session becomes the account (see `gate`).
pub async fn poll(base: &Url, start: &DeviceStart) -> Result<Session> {
    poll_with(base, start, Arc::new(KeyringStore), tokio::time::sleep).await
}

/// `poll` with an injected store and sleep function. `store` is where the session saves.
pub async fn poll_with<S, Fut>(
    base: &Url,
    start: &DeviceStart,
    store: Arc<dyn TokenStore>,
    sleep: S,
) -> Result<Session>
where
    S: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    let http = http_client();
    let url = endpoint(base, &["device", "token"])?;
    let mut interval = start.interval.max(1);
    // Time spent waiting, counted from the sleeps we asked for.
    let mut waited = 0u64;
    loop {
        if waited >= start.expires_in {
            return Err(Error::Expired);
        }
        sleep(Duration::from_secs(interval)).await;
        waited += interval;
        let body = serde_json::json!({ "deviceCode": start.device_code });
        match post::<Tokens>(&http, url.clone(), body).await {
            Ok(tokens) => return Ok(Session::from_tokens(base.clone(), http, store, tokens)),
            Err(e) if e.code == "authorization_pending" => {}
            Err(e) if e.code == "slow_down" => interval += SLOW_DOWN_SECS,
            Err(e) if e.code == "expired_token" => return Err(Error::Expired),
            Err(e) if e.code == "access_denied" => return Err(Error::Denied),
            // A dropped connection should not end a sign-in the user is still approving.
            Err(e) if e.status == 0 && e.retryable => {}
            Err(e) => return Err(e.into()),
        }
    }
}

impl Session {
    fn from_tokens(
        base: Url,
        http: reqwest::Client,
        store: Arc<dyn TokenStore>,
        tokens: Tokens,
    ) -> Self {
        Session {
            kind: Kind::Tokens(Box::new(Stored {
                base,
                http,
                store,
                access: StdMutex::new(tokens.access_token.clone()),
                gate: StdMutex::new(()),
                tokens: Mutex::new(tokens),
                generation: AtomicU64::new(0),
                dead: AtomicBool::new(false),
                ended: watch::channel(false).0,
                save_error: StdMutex::new(None),
            })),
        }
    }

    /// The saved session, or one backed by `INSTABOX_API_KEY`. `None` when signed out.
    pub fn load(base: &Url) -> Option<Session> {
        let key = std::env::var("INSTABOX_API_KEY").ok();
        Self::load_from(base, key, Arc::new(KeyringStore))
    }

    pub fn load_from(
        base: &Url,
        api_key: Option<String>,
        store: Arc<dyn TokenStore>,
    ) -> Option<Session> {
        if let Some(key) = api_key.filter(|k| !k.is_empty()) {
            return Some(Session {
                kind: Kind::ApiKey(key),
            });
        }
        let tokens = store.load().ok().flatten()?;
        Some(Session::from_tokens(
            base.clone(),
            http_client(),
            store,
            tokens,
        ))
    }

    /// True once the server ended the session. Every call then fails with
    /// `unauthenticated` and no request; the app should show the sign-in screen.
    pub fn is_signed_out(&self) -> bool {
        match &self.kind {
            Kind::ApiKey(_) => false,
            Kind::Tokens(s) => s.dead.load(Ordering::SeqCst),
        }
    }

    /// Turns true when the session ends, for whatever reason. None for an API key, which
    /// never ends.
    pub fn ended(&self) -> Option<watch::Receiver<bool>> {
        match &self.kind {
            Kind::ApiKey(_) => None,
            Kind::Tokens(s) => Some(s.ended.subscribe()),
        }
    }

    /// The last failure to save rotated tokens, if any. The session keeps working
    /// in memory, but the user must sign in again after a restart.
    pub fn store_error(&self) -> Option<String> {
        match &self.kind {
            Kind::ApiKey(_) => None,
            Kind::Tokens(s) => s.save_error.lock().unwrap().clone(),
        }
    }

    /// The access token as it is now, without a refresh and without waiting for one, even
    /// while a refresh is running. None for an API key or an ended session.
    pub fn current_access_token(&self) -> Option<String> {
        match &self.kind {
            Kind::ApiKey(_) => None,
            Kind::Tokens(s) => {
                if s.dead.load(Ordering::SeqCst) {
                    return None;
                }
                Some(s.access.lock().unwrap_or_else(|e| e.into_inner()).clone())
            }
        }
    }

    /// Saves the session's tokens: a device sign-in's session as it becomes the account.
    pub fn save(&self) -> Result<()> {
        match &self.kind {
            Kind::ApiKey(_) => Ok(()),
            Kind::Tokens(s) => {
                let _g = s.gate.lock().unwrap_or_else(|e| e.into_inner());
                if s.dead.load(Ordering::SeqCst) {
                    return Err(Error::Store("the session already ended".into()));
                }
                // A refresh that holds the tokens saves them itself when it is done.
                match s.tokens.try_lock() {
                    Ok(cur) => s.store.save(&cur).map_err(Error::Store),
                    Err(_) => Ok(()),
                }
            }
        }
    }

    /// Ends the session here and leaves the token store alone: another session's tokens go
    /// there next. A refresh still running neither saves nor clears after this.
    pub fn retire(&self) {
        if let Kind::Tokens(s) = &self.kind {
            let _g = s.gate.lock().unwrap_or_else(|e| e.into_inner());
            s.end();
        }
    }

    /// Ends the session here and forgets the saved tokens.
    pub fn sign_out(&self) -> Result<()> {
        match &self.kind {
            Kind::ApiKey(_) => Ok(()),
            Kind::Tokens(s) => {
                // End the session first: a refresh still running must not save again.
                let _g = s.gate.lock().unwrap_or_else(|e| e.into_inner());
                s.end();
                s.store.clear().map_err(Error::Store)
            }
        }
    }
}

/// Ends a session on the server (`POST /v1/auth/logout`) with `token`, so its tokens stop
/// working everywhere. Best effort: the whole call is bounded and any failure is ignored.
pub async fn logout_with(base: &Url, token: &str) {
    let Ok(url) = endpoint(base, &["logout"]) else {
        return;
    };
    let call = http_client()
        .post(url)
        .bearer_auth(token)
        .timeout(LOGOUT_TIMEOUT)
        .send();
    let _ = tokio::time::timeout(LOGOUT_TIMEOUT, call).await;
}

impl Stored {
    /// Marks the session ended and tells whoever watches it. The caller holds `gate`.
    /// True when this call ended it.
    fn end(&self) -> bool {
        let first = !self.dead.swap(true, Ordering::SeqCst);
        self.ended.send_replace(true);
        first
    }

    /// Refreshes while the caller holds the token lock. Rotated tokens go to the store.
    async fn refresh_locked(&self, cur: &mut Tokens) -> api::Result<String> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(signed_out());
        }
        let url = endpoint(&self.base, &["refresh"])?;
        let body = serde_json::json!({ "refreshToken": cur.refresh_token });
        match post::<Tokens>(&self.http, url, body).await {
            Ok(new) => {
                let _g = self.gate.lock().unwrap_or_else(|e| e.into_inner());
                if self.dead.load(Ordering::SeqCst) {
                    // Signed out while the call ran: keep nothing.
                    return Err(signed_out());
                }
                // Keep the new tokens first: the old refresh token is already revoked.
                *cur = new;
                *self.access.lock().unwrap_or_else(|e| e.into_inner()) = cur.access_token.clone();
                self.generation.fetch_add(1, Ordering::SeqCst);
                *self.save_error.lock().unwrap() = self.store.save(cur).err();
                Ok(cur.access_token.clone())
            }
            Err(e) => {
                if e.status == 401 {
                    // The session is over: never send this refresh token again. After a
                    // sign-out the store may belong to a newer account: leave it alone.
                    let _g = self.gate.lock().unwrap_or_else(|e| e.into_inner());
                    if self.end() {
                        let _ = self.store.clear();
                    }
                }
                Err(e)
            }
        }
    }
}

#[async_trait]
impl Credentials for Session {
    async fn bearer(&self) -> api::Result<String> {
        let s = match &self.kind {
            Kind::ApiKey(k) => return Ok(k.clone()),
            Kind::Tokens(s) => s,
        };
        let mut cur = s.tokens.lock().await;
        if s.dead.load(Ordering::SeqCst) {
            return Err(signed_out());
        }
        if (cur.access_expires_at - Utc::now()).num_seconds() < REFRESH_MARGIN_SECS {
            return s.refresh_locked(&mut cur).await;
        }
        Ok(cur.access_token.clone())
    }

    async fn refreshed(&self) -> api::Result<String> {
        let s = match &self.kind {
            Kind::ApiKey(_) => return Err(no_refresh()),
            Kind::Tokens(s) => s,
        };
        let seen = s.generation.load(Ordering::SeqCst);
        let mut cur = s.tokens.lock().await;
        if s.dead.load(Ordering::SeqCst) {
            // Signed out while this retry waited: hand out no token.
            return Err(signed_out());
        }
        if s.generation.load(Ordering::SeqCst) != seen {
            // Another caller refreshed while we waited for the lock.
            return Ok(cur.access_token.clone());
        }
        s.refresh_locked(&mut cur).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn tokens(access: &str, refresh: &str, expires_in_secs: i64) -> Tokens {
        Tokens {
            access_token: access.into(),
            access_expires_at: Utc::now() + ChronoDuration::seconds(expires_in_secs),
            refresh_token: refresh.into(),
            refresh_expires_at: Utc::now() + ChronoDuration::days(30),
        }
    }

    fn tokens_json(t: &Tokens) -> serde_json::Value {
        serde_json::to_value(t).unwrap()
    }

    fn pending(code: &str) -> ResponseTemplate {
        ResponseTemplate::new(400).set_body_json(serde_json::json!(
            {"error": {"code": code, "message": "m", "retryable": false}}))
    }

    fn device(interval: u64, expires_in: u64) -> DeviceStart {
        DeviceStart {
            device_code: "ibd_1".into(),
            user_code: "ABCD".into(),
            verification_uri: "https://x/device".into(),
            verification_uri_complete: None,
            interval,
            expires_in,
        }
    }

    /// Answers the scripted responses in order, then repeats the last one.
    struct Script(StdMutex<Vec<ResponseTemplate>>);

    impl Respond for Script {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            let mut v = self.0.lock().unwrap();
            if v.len() > 1 {
                v.remove(0)
            } else {
                v[0].clone()
            }
        }
    }

    type Slept = Arc<StdMutex<Vec<u64>>>;

    // Injected sleep: wiremock needs the real clock, so the tests skip the waiting instead.
    fn recording_sleep(log: Slept) -> impl Fn(Duration) -> std::future::Ready<()> {
        move |d| {
            log.lock().unwrap().push(d.as_secs());
            std::future::ready(())
        }
    }

    async fn mount_device_token(server: &MockServer, script: Vec<ResponseTemplate>) {
        Mock::given(method("POST"))
            .and(path("/v1/auth/device/token"))
            .and(body_json(serde_json::json!({"deviceCode": "ibd_1"})))
            .respond_with(Script(StdMutex::new(script)))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn start_posts_provider_and_parses() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/device"))
            .and(body_json(serde_json::json!({"provider": "github"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceCode": "ibd_1", "userCode": "ABCD", "verificationUri": "https://x/d",
                "verificationUriComplete": "https://x/d?c=ABCD", "interval": 5, "expiresIn": 900,
                "extra": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        let s = start(&Url::parse(&server.uri()).unwrap(), Provider::GitHub)
            .await
            .unwrap();
        assert_eq!(s.browser_url(), "https://x/d?c=ABCD");
        assert_eq!(s.interval, 5);
    }

    #[tokio::test]
    async fn poll_waits_through_pending_and_slow_down() {
        let server = MockServer::start().await;
        let done = tokens("iba_1", "ibr_1", 3600);
        mount_device_token(
            &server,
            vec![
                pending("authorization_pending"),
                pending("slow_down"),
                ResponseTemplate::new(200).set_body_json(tokens_json(&done)),
            ],
        )
        .await;
        let store = Arc::new(MemoryStore::default());
        let log: Slept = Default::default();
        let base = Url::parse(&server.uri()).unwrap();
        let session = poll_with(
            &base,
            &device(5, 900),
            store.clone(),
            recording_sleep(log.clone()),
        )
        .await
        .unwrap();
        // 5 s, 5 s (after pending), then 10 s (slow_down grew the interval by 5).
        assert_eq!(*log.lock().unwrap(), vec![5, 5, 10]);
        assert_eq!(session.bearer().await.unwrap(), "iba_1");
        assert!(store.load().unwrap().is_none(), "saved only by save()");
        session.save().unwrap();
        assert_eq!(store.load().unwrap().unwrap(), done);
    }

    #[tokio::test]
    async fn poll_stops_on_expired_and_denied() {
        for (code, want) in [
            ("expired_token", Error::Expired),
            ("access_denied", Error::Denied),
        ] {
            let server = MockServer::start().await;
            mount_device_token(&server, vec![pending(code)]).await;
            let base = Url::parse(&server.uri()).unwrap();
            let store = Arc::new(MemoryStore::default());
            let err = poll_with(
                &base,
                &device(5, 900),
                store.clone(),
                recording_sleep(Default::default()),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(err, want);
            assert!(store.load().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn poll_gives_up_when_the_code_runs_out() {
        let server = MockServer::start().await;
        mount_device_token(&server, vec![pending("authorization_pending")]).await;
        let base = Url::parse(&server.uri()).unwrap();
        let err = poll_with(
            &base,
            &device(5, 12),
            Arc::new(MemoryStore::default()),
            recording_sleep(Default::default()),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err, Error::Expired);
    }

    #[tokio::test]
    async fn poll_returns_other_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/device/token"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!(
                {"error": {"code": "invite_required", "message": "m", "retryable": false}})))
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let err = poll_with(
            &base,
            &device(5, 900),
            Arc::new(MemoryStore::default()),
            recording_sleep(Default::default()),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(err, Error::Api(e) if e.code == "invite_required" && e.status == 403));
    }

    #[tokio::test]
    async fn refresh_is_single_flight() {
        let server = MockServer::start().await;
        let new = tokens("iba_new", "ibr_new", 3600);
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .and(body_json(serde_json::json!({"refreshToken": "ibr_old"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(200))
                    .set_body_json(tokens_json(&new)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let store = Arc::new(MemoryStore::with(tokens("iba_old", "ibr_old", 3600)));
        let base = Url::parse(&server.uri()).unwrap();
        let session = Session::load_from(&base, None, store.clone()).unwrap();
        let (a, b) = tokio::join!(session.refreshed(), session.refreshed());
        assert_eq!(a.unwrap(), "iba_new");
        assert_eq!(b.unwrap(), "iba_new");
        assert_eq!(store.load().unwrap().unwrap(), new);
    }

    #[tokio::test]
    async fn refreshes_before_expiry() {
        let server = MockServer::start().await;
        let new = tokens("iba_new", "ibr_new", 3600);
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens_json(&new)))
            .expect(1)
            .mount(&server)
            .await;
        let store = Arc::new(MemoryStore::with(tokens("iba_old", "ibr_old", 30)));
        let base = Url::parse(&server.uri()).unwrap();
        let session = Session::load_from(&base, None, store).unwrap();
        assert_eq!(session.bearer().await.unwrap(), "iba_new");
        // The new token is fresh: no second refresh.
        assert_eq!(session.bearer().await.unwrap(), "iba_new");
    }

    #[tokio::test]
    async fn fresh_token_is_not_refreshed() {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryStore::with(tokens("iba_ok", "ibr_ok", 3000)));
        let base = Url::parse(&server.uri()).unwrap();
        let session = Session::load_from(&base, None, store).unwrap();
        assert_eq!(session.bearer().await.unwrap(), "iba_ok");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn after_a_refresh_401_the_session_stays_signed_out_without_requests() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!(
                {"error": {"code": "unauthenticated", "message": "m", "retryable": false}})))
            .expect(1)
            .mount(&server)
            .await;
        // Access token already expired, so bearer() refreshes.
        let store = Arc::new(MemoryStore::with(tokens("iba_old", "ibr_old", -10)));
        let base = Url::parse(&server.uri()).unwrap();
        let session = Session::load_from(&base, None, store).unwrap();
        assert!(!session.is_signed_out());
        assert_eq!(session.bearer().await.unwrap_err().status, 401);
        assert!(session.is_signed_out());
        let again = session.bearer().await.unwrap_err();
        assert_eq!(
            (again.status, again.code.as_str()),
            (401, "unauthenticated")
        );
        assert_eq!(session.refreshed().await.unwrap_err().status, 401);
        // `expect(1)` on the mock fails the test if a second POST arrived.
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    /// A store whose save always fails.
    struct FailingSave;

    impl TokenStore for FailingSave {
        fn load(&self) -> std::result::Result<Option<Tokens>, String> {
            Ok(None)
        }
        fn save(&self, _: &Tokens) -> std::result::Result<(), String> {
            Err("keychain locked".into())
        }
        fn clear(&self) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn failed_save_keeps_the_rotated_tokens_in_use() {
        let server = MockServer::start().await;
        let new = tokens("iba_new", "ibr_new", 3600);
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .and(body_json(serde_json::json!({"refreshToken": "ibr_old"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens_json(&new)))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let session = Session::from_tokens(
            base,
            http_client(),
            Arc::new(FailingSave),
            tokens("iba_old", "ibr_old", 3600),
        );
        assert_eq!(session.refreshed().await.unwrap(), "iba_new");
        assert_eq!(session.store_error().as_deref(), Some("keychain locked"));
        // Still usable, no further refresh.
        assert_eq!(session.bearer().await.unwrap(), "iba_new");
        assert!(!session.is_signed_out());
    }

    #[tokio::test]
    async fn refresh_401_clears_the_store() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!(
                {"error": {"code": "unauthenticated", "message": "m", "retryable": false}})))
            .mount(&server)
            .await;
        let store = Arc::new(MemoryStore::with(tokens("iba_old", "ibr_old", 3600)));
        let base = Url::parse(&server.uri()).unwrap();
        let session = Session::load_from(&base, None, store.clone()).unwrap();
        let err = session.refreshed().await.unwrap_err();
        assert_eq!(err.status, 401);
        assert!(store.load().unwrap().is_none());
    }

    #[tokio::test]
    async fn api_key_is_the_bearer_and_never_refreshes() {
        let base = Url::parse("http://127.0.0.1:1").unwrap();
        let session = Session::load_from(
            &base,
            Some("ibk_abc".into()),
            Arc::new(MemoryStore::default()),
        )
        .unwrap();
        assert_eq!(session.bearer().await.unwrap(), "ibk_abc");
        assert_eq!(
            session.refreshed().await.unwrap_err().code,
            "api_key_no_refresh"
        );
    }

    #[tokio::test]
    async fn logout_with_sends_the_given_token_and_ignores_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/logout"))
            .and(wiremock::matchers::header("authorization", "Bearer iba_1"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        logout_with(&base, "iba_1").await;
    }

    #[test]
    fn current_access_token_is_none_for_an_api_key() {
        let base = Url::parse("http://127.0.0.1:1").unwrap();
        let key = Session::load_from(
            &base,
            Some("ibk_x".into()),
            Arc::new(MemoryStore::default()),
        )
        .unwrap();
        assert_eq!(key.current_access_token(), None);
        let store = Arc::new(MemoryStore::with(tokens("iba_1", "ibr_1", 3600)));
        let s = Session::load_from(&base, None, store).unwrap();
        assert_eq!(s.current_access_token().as_deref(), Some("iba_1"));
    }

    #[test]
    fn load_is_none_when_signed_out_and_sign_out_clears() {
        let base = Url::parse("http://127.0.0.1:1").unwrap();
        assert!(Session::load_from(&base, None, Arc::new(MemoryStore::default())).is_none());
        let store = Arc::new(MemoryStore::with(tokens("iba", "ibr", 3600)));
        let session = Session::load_from(&base, None, store.clone()).unwrap();
        session.sign_out().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[tokio::test]
    async fn an_old_sessions_401_after_sign_out_leaves_a_newer_store_alone() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_millis(500)))
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let store = Arc::new(MemoryStore::with(tokens("iba_1", "ibr_1", 5)));
        let session = Arc::new(Session::load_from(&base, None, store.clone()).unwrap());
        let s2 = session.clone();
        let call = tokio::spawn(async move { s2.bearer().await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        session.sign_out().unwrap();
        // Another account signs in and saves into the same single store.
        store.save(&tokens("iba_new", "ibr_new", 3600)).unwrap();
        assert!(call.await.unwrap().is_err());
        assert_eq!(store.load().unwrap().unwrap().access_token, "iba_new");
    }

    #[tokio::test]
    async fn every_end_of_a_session_tells_its_watchers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let session = |secs| {
            let store = Arc::new(MemoryStore::with(tokens("iba_1", "ibr_1", secs)));
            Session::load_from(&base, None, store).unwrap()
        };
        // The server ends it: a refresh answers 401.
        let s = session(5);
        let ended = s.ended().unwrap();
        assert!(!*ended.borrow());
        assert!(s.bearer().await.is_err());
        assert!(*ended.borrow(), "a refresh 401");
        let s = session(3600);
        let ended = s.ended().unwrap();
        s.retire();
        assert!(*ended.borrow(), "replaced");
        let s = session(3600);
        let ended = s.ended().unwrap();
        s.sign_out().unwrap();
        assert!(*ended.borrow(), "signed out");
        let key = Session::load_from(&base, Some("ibk".into()), Arc::new(MemoryStore::default()));
        assert!(key.unwrap().ended().is_none(), "an API key never ends");
    }

    #[tokio::test]
    async fn a_queued_retry_gets_the_signed_out_error_after_sign_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let store = Arc::new(MemoryStore::with(tokens("iba_1", "ibr_1", 3600)));
        let session = Arc::new(Session::load_from(&base, None, store.clone()).unwrap());
        let Kind::Tokens(s) = &session.kind else {
            unreachable!()
        };
        // Stand in for a refresh in progress: hold the token lock.
        let held = s.tokens.lock().await;
        let waiter = session.clone();
        let call = tokio::spawn(async move { waiter.refreshed().await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        // That refresh finishes (generation moves), then the user signs out.
        s.generation.fetch_add(1, Ordering::SeqCst);
        session.sign_out().unwrap();
        drop(held);
        let err = call.await.unwrap().unwrap_err();
        assert_eq!(err.code, "unauthenticated", "no token for the retry");
        assert_eq!(store.load().unwrap(), None);
    }

    #[tokio::test]
    async fn a_client_request_that_got_401_is_not_retried_after_sign_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_millis(300)))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let store = Arc::new(MemoryStore::with(tokens("iba_1", "ibr_1", 3600)));
        let session = Arc::new(Session::load_from(&base, None, store.clone()).unwrap());
        let client = api::Client::new(base.clone(), session.clone());
        let call = tokio::spawn(async move { client.me().await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let Kind::Tokens(s) = &session.kind else {
            unreachable!()
        };
        // A refresh in progress holds the token lock while the 401 comes back.
        let held = s.tokens.lock().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        s.generation.fetch_add(1, Ordering::SeqCst);
        session.sign_out().unwrap();
        drop(held);
        let err = call.await.unwrap().unwrap_err();
        assert_eq!(err.code, "unauthenticated");
        // Dropping the server checks the request was sent once, never retried.
    }
}
