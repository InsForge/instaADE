//! One sandbox at runtime, as the proxy sees it. InstaADE's own: it takes the place of
//! t3boat's machine runtime (Boat power, watchdog, snapshots), which is not ported. Some of its
//! ideas are: one wake at a time with a cooldown after a failure, messages accepted while
//! asleep and sent once the sandbox is up, the capture T3 Code is answered from while asleep.
//!
//! - Power comes from instabox: a read of the sandbox (`GET /v1/machines/{id}`), which
//!   instabox does not count as use, every `Tuning::poll`.
//! - A wake is `sandbox::ensure_running` (resume or start), then the T3 server answering.
//! - Upstream the proxy uses a DPoP session of its own (`dpop`): every request and WebSocket
//!   upgrade gets a fresh proof, and the one-hour token is renewed before it runs out while the
//!   proxy holds the sandbox, never while it is released or asleep (that request would wake it;
//!   T3 renews an expired token too). A refused renewal pairs again once through the exec API;
//!   if that fails too, requests fail with `dpop::Refused` until the sandbox is paired again.
//!   There is no fallback to a bearer token.
//! - New: after `Tuning::idle_after` without a user action, and with no agent working, the
//!   runtime captures the state and releases the sandbox (`Power::Stopped` while it still runs).
//!   Every session closes its upstream socket then, so the instabox port lease ends and the
//!   sandbox pauses after its own auto-sleep time. The next user action wakes it again (no
//!   resume needed while it still runs).

use super::assets;
use super::auth::{self, Auth};
use super::cache::{self, Cache, Capture};
use super::dpop::{self, Credential, DpopKey, Refused};
use super::hub::TokenStore;
use super::threads::{self, ThreadFlags};
use super::upstream;
use crate::api::Machine;
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::Utc;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// While a wake or a pause settles, the sandbox is read this often.
const POLL_SETTLING: Duration = Duration::from_secs(3);
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(120);
/// After a failed wake, requests fail fast instead of each retrying the wake.
const WAKE_COOLDOWN: Duration = Duration::from_secs(30);
/// Time for a capture, shell and chats.
const CAPTURE_BUDGET: Duration = Duration::from_secs(12);
const THREAD_FETCHES: usize = 4;
/// The chats a capture stores at most, most recent first.
const MAX_STORED_THREADS: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Power {
    Unknown,
    Starting,
    /// Runs, and the proxy holds it: sessions pass frames through.
    Running,
    Stopping,
    /// Paused or stopped, or released by the proxy while it still runs: sessions answer
    /// locally and wake it for a person.
    Stopped,
    /// The sandbox is gone. Final.
    Missing,
}

/// The power a sandbox in instabox `state` has for the proxy. `released`: the proxy let go of
/// it, so whatever it does counts as asleep until a person wakes it through the proxy.
pub fn power_of(state: &str, released: bool) -> Power {
    let p = match state {
        "running" => Power::Running,
        "suspended" | "stopped" | "archived" => Power::Stopped,
        "suspending" | "stopping" | "archiving" => Power::Stopping,
        "creating" | "starting" | "resuming" | "restarting" => Power::Starting,
        "deleting" | "deleted" => Power::Missing,
        _ => Power::Unknown,
    };
    if released && matches!(p, Power::Running | Power::Starting | Power::Stopping) {
        Power::Stopped
    } else {
        p
    }
}

/// A read of the sandbox failed.
#[derive(Debug)]
pub enum ControlError {
    /// It no longer exists (404).
    Gone,
    Other(String),
}

/// What the proxy needs from instabox. The app's is `hub::Instabox`; tests use a fake.
#[async_trait]
pub trait Control: Send + Sync {
    /// The sandbox as instabox has it. A read: instabox does not count it as use.
    async fn machine(&self, id: &str) -> Result<Machine, ControlError>;
    /// Resumes or starts the sandbox and returns once it runs.
    async fn wake(&self, id: &str) -> Result<(), String>;
    /// A one-time T3 pairing credential, issued on the running sandbox
    /// (`dpop::PAIRING_SCRIPT`). Never logged.
    async fn pairing_credential(&self, id: &str) -> Result<String, String>;
}

/// What InstaADE keeps of a sandbox paired through the proxy, in `record.json`. No secret:
/// client tokens are kept as hashes, the upstream token is in the keychain.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub name: String,
    /// The localhost port T3 Code knows the sandbox by.
    pub port: u16,
    /// The T3 server's own id, which T3 Code saves the environment under.
    pub environment_id: String,
    /// The sandbox's published T3 port.
    pub upstream_url: String,
    /// SHA-256 of each token the proxy gave T3 Code.
    #[serde(default)]
    pub client_tokens: Vec<String>,
}

pub fn record_path(dir: &Path) -> PathBuf {
    dir.join("record.json")
}

/// How long the runtime waits for what. The app's is `Tuning::default()`; tests shorten it.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// Released after this long without a user action and without orchestration events.
    pub idle_after: Duration,
    /// The sandbox is read this often while nothing settles.
    pub poll: Duration,
    /// The run loop's step.
    pub tick: Duration,
    /// After a capture found an agent working, the next check waits this long.
    pub busy_recheck: Duration,
    /// A DPoP token is renewed once it has this long left.
    pub renew_before: Duration,
}

impl Default for Tuning {
    /// `idle_after` is 90 s. instabox starts the sandbox's auto-sleep clock only when the last
    /// port lease ends, so whatever the proxy holds on adds to the user's own setting: a sandbox
    /// set to 5 minutes pauses about 6.5 to 7.5 minutes after the last action. A short hold
    /// keeps that close to the setting; a longer one (the setting minus a margin) would about
    /// double it. Holding on for 90 s still rides out a short pause in a conversation, and an
    /// action after it, while the sandbox still runs, costs only a reconnect, not a resume.
    fn default() -> Self {
        Tuning {
            idle_after: Duration::from_secs(90),
            poll: Duration::from_secs(30),
            tick: Duration::from_secs(5),
            busy_recheck: Duration::from_secs(30),
            // T3's tokens last an hour: renewed about every 50 minutes while in use.
            renew_before: Duration::from_secs(600),
        }
    }
}

/// The proxy's credential on the sandbox's T3, and the key it is bound to.
struct UpstreamAuth {
    credential: Credential,
    key: Option<DpopKey>,
    /// T3 refused the session and pairing again failed: requests fail with this until the
    /// sandbox is paired again (`set_credential`), without asking T3 each time.
    refused: Option<String>,
}

impl UpstreamAuth {
    fn new(credential: Credential) -> Self {
        let (key, refused) = match &credential {
            Credential::Dpop { key, .. } => match DpopKey::from_pkcs8(key) {
                Ok(k) => (Some(k), None),
                Err(e) => (None, Some(format!("{e:#}"))),
            },
            Credential::Bearer { .. } => (None, None),
        };
        UpstreamAuth {
            credential,
            key,
            refused,
        }
    }
}

/// What the runtime last read of the sandbox.
#[derive(Clone, Debug, Default)]
struct Seen {
    state: String,
    revision: Option<i64>,
    /// instabox paused it for idleness, so any use wakes it. A sandbox its owner paused has
    /// none.
    auto_suspended: bool,
}

/// What a capture found.
#[derive(Debug, PartialEq, Eq)]
pub enum Captured {
    Done,
    /// This many chats have an agent working: not released.
    Busy(usize),
}

pub struct Runtime {
    id: String,
    dir: PathBuf,
    record: RwLock<Record>,
    upstream_auth: tokio::sync::Mutex<UpstreamAuth>,
    tokens: Arc<dyn TokenStore>,
    control: Arc<dyn Control>,
    tuning: Tuning,
    pub http: reqwest::Client,
    pub cache: Mutex<Cache>,
    pub auth: Auth,
    power: watch::Sender<Power>,
    seen: Mutex<Option<Seen>>,
    released: AtomicBool,
    /// One wake at a time; a release takes it too.
    waking: tokio::sync::Mutex<()>,
    /// The last failed wake: its message and when.
    wake_failure: Mutex<Option<(String, Instant)>>,
    /// The last user action through the proxy, and the last orchestration event seen.
    last_use: Mutex<Instant>,
    last_state_move: Mutex<Instant>,
}

impl Runtime {
    pub fn new(
        dir: PathBuf,
        record: Record,
        credential: Credential,
        tokens: Arc<dyn TokenStore>,
        control: Arc<dyn Control>,
        tuning: Tuning,
    ) -> Self {
        let cache = Cache::load(&cache::cache_path(&dir));
        let http = reqwest::Client::builder()
            .user_agent(concat!("instaade-t3-proxy/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client builds");
        Runtime {
            id: record.id.clone(),
            dir,
            record: RwLock::new(record),
            upstream_auth: tokio::sync::Mutex::new(UpstreamAuth::new(credential)),
            tokens,
            control,
            tuning,
            http,
            cache: Mutex::new(cache),
            auth: Auth::default(),
            power: watch::channel(Power::Unknown).0,
            seen: Mutex::new(None),
            released: AtomicBool::new(false),
            waking: tokio::sync::Mutex::new(()),
            wake_failure: Mutex::new(None),
            last_use: Mutex::new(Instant::now()),
            last_state_move: Mutex::new(Instant::now()),
        }
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }

    pub fn record(&self) -> Record {
        self.record.read().unwrap().clone()
    }

    /// Changes the record and saves it.
    pub fn update_record(&self, f: impl FnOnce(&mut Record)) {
        let bytes = {
            let mut r = self.record.write().unwrap();
            f(&mut r);
            serde_json::to_vec_pretty(&*r)
        };
        let saved = bytes
            .map_err(anyhow::Error::from)
            .and_then(|b| super::store::write_private(&record_path(&self.dir), &b));
        if let Err(e) = saved {
            log_warn!("saving the record of {}: {e:#}", self.id);
        }
    }

    pub fn has_client_token(&self, token: &str) -> bool {
        let hash = auth::token_hash(token);
        self.record.read().unwrap().client_tokens.contains(&hash)
    }

    pub fn add_client_token(&self, token: &str) {
        let hash = auth::token_hash(token);
        self.update_record(|r| r.client_tokens.push(hash));
    }

    /// A new upstream credential (a pairing): it also ends an earlier refusal.
    pub async fn set_credential(&self, credential: Credential) {
        *self.upstream_auth.lock().await = UpstreamAuth::new(credential);
    }

    /// The upstream credential as it stands, renewed or not.
    pub async fn credential(&self) -> Credential {
        self.upstream_auth.lock().await.credential.clone()
    }

    /// The headers that authorize `method` on `url` upstream, for this one request: a DPoP
    /// token with a fresh proof, renewed first when due (or, with `INSTAADE_T3_PROXY=any` on a
    /// T3 without DPoP, a bearer token).
    pub async fn upstream_headers(&self, method: &str, url: &str) -> Result<upstream::AuthHeaders> {
        let mut guard = self.upstream_auth.lock().await;
        let a = &mut *guard;
        if let Credential::Bearer { token } = &a.credential {
            return Ok(vec![("authorization", format!("Bearer {token}"))]);
        }
        self.fresh(a).await?;
        let (Credential::Dpop { token, .. }, Some(key)) = (&a.credential, &a.key) else {
            return Err(Refused("no DPoP key".into()).into());
        };
        let proof = key.proof(method, url, Some(token))?;
        Ok(vec![
            ("authorization", format!("DPoP {token}")),
            ("dpop", proof),
        ])
    }

    /// Renews the DPoP token once it has `renew_before` or less left (an expired one too: T3
    /// renews it within its 30-day window). A refused renewal pairs again once through the
    /// sandbox; both failing is a `Refused` that stays until the sandbox is paired again.
    async fn fresh(&self, a: &mut UpstreamAuth) -> Result<()> {
        if let Some(reason) = &a.refused {
            return Err(Refused(reason.clone()).into());
        }
        let (token, expires_at) = match &a.credential {
            Credential::Dpop {
                token, expires_at, ..
            } => (token.clone(), *expires_at),
            Credential::Bearer { .. } => return Ok(()),
        };
        let now = Utc::now().timestamp_millis();
        if !dpop::renewal_due(expires_at, now, self.tuning.renew_before) {
            return Ok(());
        }
        let Some(key) = a.key.as_ref() else {
            return Err(Refused("no DPoP key".into()).into());
        };
        let base = self.upstream_url();
        let renewed =
            match dpop::exchange(&self.http, &base, key, &token, dpop::ACCESS_TOKEN_TYPE).await {
                Ok(t) => {
                    log_info!("{}: DPoP token renewed", self.id);
                    Ok(t)
                }
                Err(e) => {
                    log_warn!("{}: renewing the DPoP token: {e:#}; pairing again", self.id);
                    self.pair_again(key).await
                }
            };
        match renewed {
            Ok((token, expires_at)) => {
                a.credential = Credential::Dpop {
                    key: key.pkcs8().to_owned(),
                    token,
                    expires_at,
                };
                if let Err(e) = self.tokens.save(&self.id, &a.credential.to_stored()) {
                    log_warn!("{}: saving the renewed token: {e}", self.id);
                }
                Ok(())
            }
            Err(e) => {
                let reason = format!("{e:#}");
                log_warn!("{}: {}", self.id, Refused(reason.clone()));
                a.refused = Some(reason.clone());
                Err(Refused(reason).into())
            }
        }
    }

    /// A new session for the same key, from a pairing credential issued on the sandbox.
    async fn pair_again(&self, key: &DpopKey) -> Result<(String, i64)> {
        let credential = self
            .control
            .pairing_credential(&self.id)
            .await
            .map_err(|e| anyhow!("pairing again: {e}"))?;
        let out = dpop::exchange(
            &self.http,
            &self.upstream_url(),
            key,
            &credential,
            dpop::BOOTSTRAP_TOKEN_TYPE,
        )
        .await
        .context("pairing again")?;
        log_info!("{}: paired again with a new DPoP session", self.id);
        Ok(out)
    }

    /// Renews the token when due. The run loop calls it only while the proxy holds the sandbox.
    pub async fn renew_if_due(&self) {
        let mut guard = self.upstream_auth.lock().await;
        if guard.refused.is_some() {
            return;
        }
        if let Err(e) = self.fresh(&mut guard).await {
            log_debug!("{}: {e:#}", self.id);
        }
    }

    /// Opens `target` (path and query) on the sandbox's T3 as a WebSocket, authorized.
    pub async fn connect_upstream(&self, target: &str) -> Result<upstream::Upstream> {
        let base = self.upstream_url();
        let headers = self
            .upstream_headers("GET", &format!("{base}{target}"))
            .await?;
        upstream::connect(&base, &headers, target).await
    }

    /// The headers for a short-lived RPC socket (`upstream::call_many`, `subscribe_until`).
    async fn rpc_headers(&self) -> Result<upstream::AuthHeaders> {
        self.upstream_headers("GET", &format!("{}/ws", self.upstream_url()))
            .await
    }

    /// Where this sandbox's record, cache, chats and images are kept on this computer.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn power(&self) -> Power {
        *self.power.borrow()
    }

    pub fn subscribe_power(&self) -> watch::Receiver<Power> {
        self.power.subscribe()
    }

    /// Missing is final.
    fn set_power(&self, p: Power) {
        let changed = self.power.send_if_modified(|cur| {
            if *cur == Power::Missing || *cur == p {
                return false;
            }
            *cur = p;
            true
        });
        if changed {
            log_debug!("{}: power {p:?}", self.id);
        }
    }

    /// The proxy let go of the sandbox (see the module docs).
    pub fn released(&self) -> bool {
        self.released.load(Ordering::SeqCst)
    }

    /// The sandbox is about to pause or stop: user actions must wait for it and wake it again.
    pub fn is_stopping(&self) -> bool {
        self.seen
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| matches!(s.state.as_str(), "suspending" | "stopping"))
    }

    /// False for a sandbox its owner paused or stopped: only a person's action wakes it then,
    /// never what T3 Code asks on its own. Unknown counts as true.
    pub fn wakes_for_background(&self) -> bool {
        let seen = self.seen.lock().unwrap();
        let Some(s) = seen.as_ref() else { return true };
        let paused = matches!(
            s.state.as_str(),
            "suspended" | "suspending" | "stopped" | "stopping"
        );
        !paused || s.auto_suspended
    }

    pub fn save_cache(&self) {
        let result = {
            let cache = self.cache.lock().unwrap();
            if self.power() == Power::Missing {
                return;
            }
            cache
                .to_bytes()
                .and_then(|b| super::store::write_private(&cache::cache_path(&self.dir), &b))
        };
        if let Err(e) = result {
            log_warn!("saving the cache of {}: {e:#}", self.id);
        }
    }

    fn note(&self, m: &Machine) {
        *self.seen.lock().unwrap() = Some(Seen {
            state: m.state.clone(),
            revision: m.revision,
            auto_suspended: m.auto_suspended_at.is_some(),
        });
    }

    /// Reads the sandbox and publishes its power.
    pub async fn refresh_power(&self) -> Result<Power> {
        match self.control.machine(&self.id).await {
            Ok(m) => {
                self.note(&m);
                let p = power_of(&m.state, self.released());
                if p == Power::Running && self.waking.try_lock().is_err() {
                    // A wake waits for T3 to answer; it publishes Running once it does.
                    return Ok(p);
                }
                self.set_power(p);
                Ok(p)
            }
            Err(ControlError::Gone) => {
                self.set_power(Power::Missing);
                Ok(Power::Missing)
            }
            Err(ControlError::Other(e)) => Err(anyhow!(e)),
        }
    }

    /// Waits out transitional states, so callers see Running or Stopped (or Missing).
    pub async fn settled_power(&self) -> Power {
        let mut rx = self.subscribe_power();
        let settled = |p: &Power| matches!(p, Power::Running | Power::Stopped | Power::Missing);
        let _ = tokio::time::timeout(SETTLE_TIMEOUT, rx.wait_for(settled)).await;
        self.power()
    }

    pub fn upstream_url(&self) -> String {
        self.record
            .read()
            .unwrap()
            .upstream_url
            .trim_end_matches('/')
            .to_owned()
    }

    /// The sandbox's own descriptor, never instabox's answer for a sandbox that does not run.
    /// Cached for while the sandbox sleeps. Only while it runs: a request to the published port
    /// of a paused sandbox wakes it.
    pub async fn fetch_descriptor(&self) -> Option<Value> {
        let url = format!("{}/.well-known/t3/environment", self.upstream_url());
        let res = self
            .http
            .get(url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .ok()?;
        if !res.status().is_success() {
            return None;
        }
        let v: Value = res.json().await.ok()?;
        if v["environmentId"].as_str() != Some(&self.record().environment_id) {
            return None;
        }
        self.cache.lock().unwrap().descriptor = Some(v.clone());
        Some(v)
    }

    /// Brings the sandbox up if needed and returns once its T3 server answers. Concurrent
    /// callers share one wake.
    pub async fn ensure_awake(&self) -> Result<()> {
        let _guard = self.waking.lock().await;
        if self.power() == Power::Missing {
            bail!("the sandbox no longer exists");
        }
        if let Some((e, at)) = self.wake_failure.lock().unwrap().clone()
            && at.elapsed() < WAKE_COOLDOWN
        {
            return Err(anyhow!(e));
        }
        if !self.released()
            && self.power() == Power::Running
            && self.fetch_descriptor().await.is_some()
        {
            *self.wake_failure.lock().unwrap() = None;
            // A message accepted while another caller's wake was finishing.
            self.flush_deferred_commands().await;
            return Ok(());
        }
        let result = self.wake_locked().await;
        match &result {
            Ok(()) => *self.wake_failure.lock().unwrap() = None,
            Err(e) => {
                log_warn!("{} could not wake: {e:#}", self.id);
                *self.wake_failure.lock().unwrap() = Some((format!("{e:#}"), Instant::now()));
                let _ = self.refresh_power().await;
            }
        }
        result
    }

    async fn wake_locked(&self) -> Result<()> {
        let started = Instant::now();
        let fresh = self.fresh_capture().is_some();
        self.released.store(false, Ordering::SeqCst);
        self.set_power(Power::Starting);
        self.control.wake(&self.id).await.map_err(|e| anyhow!(e))?;
        if let Ok(m) = self.control.machine(&self.id).await {
            self.note(&m);
        }
        // From here on the sandbox runs: whatever it slept with is history. A sleep the proxy
        // did not see may have outdated the recorded reads too.
        if !fresh {
            self.cache.lock().unwrap().bump_epoch();
        }
        self.drop_capture();
        self.wait_ready(started).await?;
        // Before Running is published: T3 Code reconnects then, and must find the messages
        // accepted while asleep ahead of anything it sends next.
        self.on_ready().await;
        *self.last_use.lock().unwrap() = Instant::now();
        self.set_power(Power::Running);
        log_info!("{} awake after {:?}", self.id, started.elapsed());
        Ok(())
    }

    /// T3 answers after a wake: sends what waited for it and refreshes the session.
    async fn on_ready(&self) {
        self.flush_deferred_commands().await;
        if let Err(e) = self.prime().await {
            log_warn!("refreshing {}'s session: {e:#}", self.id);
        }
    }

    /// Fills what the proxy must answer before the sandbox can sleep: descriptor and session.
    pub async fn prime(&self) -> Result<()> {
        self.fetch_descriptor()
            .await
            .ok_or_else(|| anyhow!("T3 on the sandbox did not answer"))?;
        let url = format!("{}/api/auth/session", self.upstream_url());
        let mut req = self.http.get(&url).timeout(Duration::from_secs(10));
        for (name, value) in self.upstream_headers("GET", &url).await? {
            req = req.header(name, value);
        }
        let res = req.send().await?;
        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("T3 on the sandbox refused the proxy's session");
        }
        let session = res
            .error_for_status()?
            .json::<Map<String, Value>>()
            .await
            .context("T3 on the sandbox sent a malformed auth session")?;
        if session.get("authenticated") == Some(&Value::Bool(false)) {
            bail!("T3 on the sandbox refused the proxy's session");
        }
        self.cache.lock().unwrap().auth_session = Some(session);
        self.save_cache();
        Ok(())
    }

    async fn wait_ready(&self, started: Instant) -> Result<()> {
        while self.fetch_descriptor().await.is_none() {
            if started.elapsed() > READY_TIMEOUT {
                bail!("T3 on the sandbox did not answer");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Ok(())
    }

    /// Sends the messages accepted while asleep. Left queued until delivered: the run loop
    /// retries a failed send, and the server dedupes them by `commandId`.
    async fn flush_deferred_commands(&self) {
        let commands = self.cache.lock().unwrap().deferred_commands.clone();
        if commands.is_empty() {
            return;
        }
        let requests = commands
            .iter()
            .map(|c| ("orchestration.dispatchCommand".to_owned(), c.clone()))
            .collect();
        let sent = match self.rpc_headers().await {
            Ok(headers) => upstream::call_many(&self.upstream_url(), &headers, requests).await,
            Err(e) => Err(e),
        };
        match sent {
            Ok(results) => {
                self.cache
                    .lock()
                    .unwrap()
                    .deferred_commands
                    .retain(|c| !commands.contains(c));
                // T3 Code was told they were accepted: a refused one is only in the log.
                for (command, result) in commands.iter().zip(results) {
                    if let Err(e) = result {
                        log_warn!("{}: a {} was refused: {e}", self.id, command["type"]);
                    }
                }
            }
            Err(e) => {
                log_warn!("sending deferred commands to {}: {e:#}", self.id);
                return;
            }
        }
        self.save_cache();
    }

    /// Someone used the sandbox through the proxy: it is held, and recorded answers may be
    /// outdated.
    pub fn user_action(&self) {
        self.cache.lock().unwrap().bump_epoch();
        *self.last_use.lock().unwrap() = Instant::now();
    }

    /// An orchestration event passed: the sandbox's state is changing (an agent at work, most
    /// likely), so it is not released yet.
    pub fn state_moved(&self) {
        *self.last_state_move.lock().unwrap() = Instant::now();
    }

    // ---- the capture ----

    /// The capture stands in for the server only while the sandbox counts as asleep, and only
    /// if no other run of the sandbox started since it was taken (someone else woke it).
    pub fn fresh_capture(&self) -> Option<Capture> {
        if self.power() != Power::Stopped {
            return None;
        }
        let capture = self
            .cache
            .lock()
            .unwrap()
            .capture
            .clone()
            .filter(|c| c.confirmed)?;
        let revision = self.seen.lock().unwrap().as_ref().and_then(|s| s.revision);
        trusted(&capture, revision).then_some(capture)
    }

    fn drop_capture(&self) {
        if self.cache.lock().unwrap().capture.take().is_some() {
            self.save_cache();
        }
    }

    /// Records the state the sandbox is released with: everything T3 Code needs to resume its
    /// shell and open its chats without the sandbox. Not when an agent works.
    pub async fn capture(&self) -> Result<Captured> {
        let deadline = tokio::time::Instant::now() + CAPTURE_BUDGET;
        let mut shell = tokio::time::timeout_at(deadline, self.shell_snapshot()).await??;
        let working = working_threads(&shell);
        if working > 0 {
            return Ok(Captured::Busy(working));
        }
        let sequence = shell["snapshotSequence"]
            .as_u64()
            .context("the shell snapshot has no sequence")?;
        if let Some(o) = shell.as_object_mut() {
            o.remove("projectIdentities");
        }
        let (configs, (), stored) = tokio::join!(
            self.refresh_project_configs(deadline),
            self.refresh_scheduled_tasks(deadline),
            self.store_threads(&shell, deadline),
        );
        if let Err(e) = configs {
            log_warn!("reading t3.json again on {}: {e:#}", self.id);
        }
        let revision = self.seen.lock().unwrap().as_ref().and_then(|s| s.revision);
        {
            let mut cache = self.cache.lock().unwrap();
            cache.prune();
            assets::sweep(&self.dir, &cache);
            cache.capture = Some(Capture {
                at: Utc::now().timestamp_millis(),
                sequence,
                threads: stored,
                shell,
                confirmed: true,
                revision,
            });
        }
        self.save_cache();
        log_info!("captured {} at sequence {sequence}", self.id);
        Ok(Captured::Done)
    }

    /// Reads every recorded `t3.json` again, so that it is true while the sandbox sleeps: T3
    /// Code reads it on every chat open, and its editor saves over the file whatever it was
    /// shown. One not read again keeps its epoch, so opening that chat wakes the sandbox.
    async fn refresh_project_configs(&self, deadline: tokio::time::Instant) -> Result<()> {
        let (epoch, reads) = {
            let cache = self.cache.lock().unwrap();
            (cache.epoch, cache.project_config_reads())
        };
        if reads.is_empty() {
            return Ok(());
        }
        let (keys, requests): (Vec<_>, Vec<_>) = reads
            .into_iter()
            .map(|(key, payload)| (key, ("projects.readFile".to_owned(), payload)))
            .unzip();
        let headers = self.rpc_headers().await?;
        let answers = tokio::time::timeout_at(
            deadline,
            upstream::call_many(&self.upstream_url(), &headers, requests),
        )
        .await??;
        let mut cache = self.cache.lock().unwrap();
        for (key, value) in keys.iter().zip(answers) {
            if let Ok(value) = value
                && !value.is_null()
            {
                cache.refresh_unary(key, epoch, value);
            }
        }
        Ok(())
    }

    /// Reads the automations again if T3 Code was shown them: T3 Code's editor saves every field
    /// of a task as it was shown. A list not read again is dropped, so asking for it wakes the
    /// sandbox.
    async fn refresh_scheduled_tasks(&self, deadline: tokio::time::Instant) {
        const TAG: &str = "scheduledTasks.subscribe";
        let key = cache::request_key(TAG, &json!({}));
        if !self.cache.lock().unwrap().streams.contains_key(&key) {
            return;
        }
        let url = self.upstream_url();
        let list = match self.rpc_headers().await {
            Ok(headers) => {
                let first = upstream::subscribe_until(
                    &url,
                    &headers,
                    TAG,
                    json!({}),
                    CAPTURE_BUDGET,
                    |list| Some(list.clone()),
                );
                tokio::time::timeout_at(deadline, first).await
            }
            Err(e) => Ok(Err(e)),
        };
        let mut cache = self.cache.lock().unwrap();
        match (list, cache.streams.get_mut(&key)) {
            (Ok(Ok((Some(list), _))), Some(replay)) => replay.value = vec![list],
            _ => {
                cache.streams.remove(&key);
            }
        }
    }

    /// A read of the sandbox's orchestration state over HTTP. `segments` go after
    /// `/api/orchestration`, each encoded.
    async fn orchestration_get(
        &self,
        segments: &[&str],
        query: Option<&str>,
    ) -> Result<reqwest::Response> {
        let mut url = url::Url::parse(&self.upstream_url())?;
        url.path_segments_mut()
            .map_err(|_| anyhow!("the upstream URL cannot take a path"))?
            .pop_if_empty()
            .extend(["api", "orchestration"])
            .extend(segments);
        url.set_query(query);
        let mut req = self
            .http
            .get(url.clone())
            .header("x-t3-orchestration-protocol", "2")
            .timeout(Duration::from_secs(30));
        for (name, value) in self.upstream_headers("GET", url.as_str()).await? {
            req = req.header(name, value);
        }
        let res = req.send().await?;
        if !res.status().is_success() {
            bail!("orchestration read: HTTP {}", res.status());
        }
        Ok(res)
    }

    /// The whole shell: no thread left out, so it holds every chat to store and serves every
    /// client.
    async fn shell_snapshot(&self) -> Result<Value> {
        Ok(self
            .orchestration_get(&["shell"], None)
            .await?
            .json()
            .await?)
    }

    /// Fetches the chats the store should hold, most recent first, until `deadline`, and drops
    /// every other file. Returns each stored chat's sequence. A chat that is not stored wakes
    /// the sandbox when opened.
    async fn store_threads(
        &self,
        shell: &Value,
        deadline: tokio::time::Instant,
    ) -> HashMap<String, u64> {
        // T3 Code opened no chat through this proxy yet: nothing says how to encode them.
        let Some(flags) = self.cache.lock().unwrap().thread_encoding else {
            return HashMap::new();
        };
        let wanted = threads::plan(shell, Utc::now(), MAX_STORED_THREADS);
        let stored = Mutex::new(HashMap::new());
        let keep = Mutex::new(HashSet::new());
        let fetches =
            futures_util::stream::iter(wanted).for_each_concurrent(THREAD_FETCHES, |thread| {
                let (stored, keep) = (&stored, &keep);
                async move {
                    let fetched = async {
                        let (sequence, body) = self.fetch_thread(thread, flags).await?;
                        let path = threads::path(&self.dir, thread, sequence, flags);
                        if !path.exists() {
                            super::store::write_private(&path, &body)?;
                        }
                        anyhow::Ok((sequence, path))
                    };
                    match fetched.await {
                        Ok((sequence, path)) => {
                            stored.lock().unwrap().insert(thread.to_owned(), sequence);
                            keep.lock().unwrap().insert(path);
                        }
                        Err(e) => log_warn!("storing a chat of {}: {e:#}", self.id),
                    }
                }
            });
        if tokio::time::timeout_at(deadline, fetches).await.is_err() {
            log_info!("{}: not every chat stored", self.id);
        }
        threads::sweep(&self.dir, &keep.into_inner().unwrap());
        stored.into_inner().unwrap()
    }

    /// The chat's bounded snapshot and the thread sequence it is at.
    async fn fetch_thread(&self, thread: &str, flags: ThreadFlags) -> Result<(u64, bytes::Bytes)> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Head {
            snapshot_sequence: u64,
        }
        let query = flags.query();
        let body = self
            .orchestration_get(&["threads", thread, "bounded"], Some(&query))
            .await?
            .bytes()
            .await?;
        let head: Head = serde_json::from_slice(&body)?;
        Ok((head.snapshot_sequence, body))
    }

    /// The chat as the sandbox sleeps with it, in the encoding `flags`. `None`: not stored, or
    /// stored in an earlier state or another encoding.
    pub fn stored_thread(
        &self,
        capture: &Capture,
        thread: &str,
        flags: ThreadFlags,
    ) -> Option<Vec<u8>> {
        std::fs::read(threads::path(
            &self.dir,
            thread,
            *capture.threads.get(thread)?,
            flags,
        ))
        .ok()
    }

    // ---- releasing an idle sandbox (new) ----

    /// True once neither a user action nor an orchestration event came for `idle_after`.
    fn idle(&self) -> bool {
        let quiet = |at: &Mutex<Instant>| at.lock().unwrap().elapsed() >= self.tuning.idle_after;
        quiet(&self.last_use) && quiet(&self.last_state_move)
    }

    /// Releases the sandbox once it is idle and no agent works: captures its state, then
    /// publishes `Stopped`, so every session closes its upstream socket. A user action during
    /// the capture calls it off. `last_check`: when a capture last found an agent working.
    pub async fn maybe_release(&self, last_check: &mut Option<Instant>) {
        if self.power() != Power::Running || self.released() || !self.idle() {
            return;
        }
        if last_check.is_some_and(|t| t.elapsed() < self.tuning.busy_recheck) {
            return;
        }
        let mark = *self.last_use.lock().unwrap();
        match self.capture().await {
            Ok(Captured::Done) => {}
            Ok(Captured::Busy(n)) => {
                log_debug!("{}: {n} chats working, held", self.id);
                *last_check = Some(Instant::now());
                return;
            }
            // T3 does not answer: nothing to show while asleep, but the lease still ends.
            Err(e) => log_warn!("capturing {}: {e:#}; released without a capture", self.id),
        }
        let Ok(_wake) = self.waking.try_lock() else {
            return;
        };
        if *self.last_use.lock().unwrap() != mark || self.power() != Power::Running {
            return;
        }
        self.released.store(true, Ordering::SeqCst);
        log_info!(
            "released {} after {}s without use",
            self.id,
            self.tuning.idle_after.as_secs()
        );
        self.set_power(Power::Stopped);
    }

    /// Follows the sandbox's power and releases it when idle, until it is deleted.
    pub async fn run(self: Arc<Self>) {
        let mut last_poll: Option<Instant> = None;
        let mut last_check: Option<Instant> = None;
        loop {
            let settling = matches!(
                self.power(),
                Power::Unknown | Power::Starting | Power::Stopping
            );
            let every = if settling {
                POLL_SETTLING
            } else {
                self.tuning.poll
            };
            if last_poll.is_none_or(|t| t.elapsed() >= every) {
                last_poll = Some(Instant::now());
                if let Err(e) = self.refresh_power().await {
                    log_debug!("reading {}: {e:#}", self.id);
                }
            }
            match self.power() {
                Power::Missing => return,
                Power::Running => {
                    // Only while the proxy holds the sandbox: Running is never released.
                    self.renew_if_due().await;
                    // Retries a send that failed during the wake.
                    self.flush_deferred_commands().await;
                    self.maybe_release(&mut last_check).await;
                }
                _ => last_check = None,
            }
            tokio::time::sleep(self.tuning.tick).await;
        }
    }
}

/// A confirmed capture still describes a sandbox at `revision`: at most one lifecycle change
/// (its idle pause) since it was taken. (`runStartedAt` would not do: a paused sandbox has
/// none.) Unknown counts as trusted.
fn trusted(capture: &Capture, revision: Option<i64>) -> bool {
    capture.confirmed
        && match (revision, capture.revision) {
            (Some(now), Some(then)) => now - then <= 1,
            _ => true,
        }
}

/// The chats in `shell` with an agent at work: a run in flight, or background work T3 waits
/// on. A run stopped at a prompt only a person answers is not: nobody should pay while it
/// waits. (The rules of t3boat's watchdog, read from the shell only.)
pub fn working_threads(shell: &Value) -> usize {
    const LIVE: &[&str] = &["queued", "preparing", "starting", "running", "waiting"];
    let running = |t: &Value| {
        !t["activeRunId"].is_null()
            || !t["activityRunStatus"].is_null()
            || t["status"].as_str().is_some_and(|s| LIVE.contains(&s))
    };
    // T3 lists a watched pull request as a task, awaited only while its checks run.
    let holds = |task: &Value| {
        !(task["taskId"]
            .as_str()
            .is_some_and(|id| id.starts_with("pull-request-watch:"))
            && task["awaited"] != true)
    };
    shell["threads"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| {
            t["pendingBackgroundTasks"]
                .as_array()
                .is_some_and(|tasks| tasks.iter().any(holds))
                || (running(t) && !waits_on_person(t))
        })
        .count()
}

/// The run stopped at a prompt only a person answers: an approval, or a question it cannot go
/// on past.
fn waits_on_person(thread: &Value) -> bool {
    let request = &thread["pendingRuntimeRequest"];
    if request.is_null() || request["blocking"] == false {
        return false;
    }
    if matches!(
        request["kind"].as_str(),
        Some("auth_refresh" | "dynamic_tool_call")
    ) {
        return false;
    }
    if request["userOnly"] == true {
        return false;
    }
    let delegated = thread["lineage"]["relationshipToParent"] == "subagent"
        || !thread["delegatedFrom"].is_null();
    !(delegated && request["kind"] == "user_input")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_follows_the_sandbox_until_released() {
        assert_eq!(power_of("running", false), Power::Running);
        assert_eq!(power_of("suspended", false), Power::Stopped);
        assert_eq!(power_of("suspending", false), Power::Stopping);
        assert_eq!(power_of("resuming", false), Power::Starting);
        assert_eq!(power_of("deleted", false), Power::Missing);
        assert_eq!(power_of("what", false), Power::Unknown);
        // Released: whatever the sandbox does counts as asleep until a person wakes it.
        for state in ["running", "resuming", "suspending", "suspended"] {
            assert_eq!(power_of(state, true), Power::Stopped, "{state}");
        }
        assert_eq!(power_of("deleted", true), Power::Missing);
    }

    #[test]
    fn a_run_waiting_on_a_person_holds_nothing() {
        let shell = json!({"threads": [
            {"id": "idle", "status": "idle", "activeRunId": null},
            {"id": "run", "activeRunId": "r1"},
            {"id": "status", "status": "running"},
            {"id": "approval", "activeRunId": "r2", "pendingRuntimeRequest": {"kind": "approval"}},
            {"id": "question", "activeRunId": "r3", "pendingRuntimeRequest": {"kind": "user_input", "blocking": false}},
            {"id": "watch", "pendingBackgroundTasks": [{"taskId": "pull-request-watch:1", "awaited": false}]},
        ]});
        // `run`, `status` and `question` (it works past a non-blocking question).
        assert_eq!(working_threads(&shell), 3);
        let waiting = json!({"threads": [{"id": "a", "activeRunId": "r", "pendingRuntimeRequest": {"kind": "approval"}}]});
        assert_eq!(working_threads(&waiting), 0);
        let task =
            json!({"threads": [{"id": "t", "pendingBackgroundTasks": [{"taskId": "bash:1"}]}]});
        assert_eq!(working_threads(&task), 1);
        assert_eq!(working_threads(&json!({})), 0);
    }

    #[test]
    fn a_capture_holds_across_one_pause_only() {
        let capture = |revision| Capture {
            confirmed: true,
            revision,
            ..Default::default()
        };
        assert!(trusted(&capture(Some(3)), Some(3)));
        assert!(trusted(&capture(Some(3)), Some(4)), "its own idle pause");
        assert!(
            !trusted(&capture(Some(3)), Some(6)),
            "woken and paused again since"
        );
        assert!(trusted(&capture(None), Some(6)));
        let unconfirmed = Capture {
            confirmed: false,
            ..capture(Some(3))
        };
        assert!(!trusted(&unconfirmed, Some(3)));
    }

    #[test]
    fn the_default_hold_is_short() {
        let t = Tuning::default();
        assert_eq!(t.idle_after, Duration::from_secs(90));
        assert!(
            t.poll >= Duration::from_secs(10),
            "reads are cheap but not free"
        );
    }
}
