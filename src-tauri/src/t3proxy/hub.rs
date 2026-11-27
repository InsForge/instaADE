//! InstaADE's own: the proxies of the sandboxes T3 Code paired through, one per sandbox on a
//! fixed localhost port, and the pairing itself.
//!
//! Upstream, the proxy uses a T3 session of its own, issued on the sandbox with
//! `t3 auth session issue` through the instabox exec API and kept in the keychain. That is a
//! bearer token: the T3 servers that ask for DPoP (0.1.11 and later) need the proxy to be a DPoP
//! client before this can ship (its own P-256 key, `t3 auth pairing create` and `/oauth/token`,
//! renewed hourly).

use super::runtime::{self, Control, ControlError, Record, Runtime, Tuning};
use crate::ade::t3;
use crate::api::{Client, ExecRequest, Machine};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// The first port a proxy may take. Each sandbox keeps the one it got: T3 Code saves it.
pub const BASE_PORT: u16 = 37800;
const PORT_RANGE: u16 = 1000;

/// Issues the proxy's T3 session on the sandbox, printed alone on stdout. The base directory is
/// the `t3-instabox` service's (see `t3::PAIR_SCRIPT`).
pub const ISSUE_TOKEN_SCRIPT: &str = r#"t3 auth session issue --base-dir "$HOME/.t3-instabox/$(cat /etc/machine-id)" --token-only --ttl 3650d --label instaade-proxy"#;
const ISSUE_TIMEOUT_SECS: u32 = 60;

/// The instabox client of the account signed in now, if any.
pub type ClientSource = Arc<dyn Fn() -> Option<Client> + Send + Sync>;

/// The proxy's instabox side: reads of the sandbox and `sandbox::ensure_running`.
pub struct Instabox {
    client: ClientSource,
}

impl Instabox {
    pub fn new(client: ClientSource) -> Self {
        Instabox { client }
    }
}

#[async_trait]
impl Control for Instabox {
    async fn machine(&self, id: &str) -> Result<Machine, ControlError> {
        let client = (self.client)().ok_or_else(|| ControlError::Other("signed out".into()))?;
        client.machine(id).await.map_err(|e| {
            if e.status == 404 {
                ControlError::Gone
            } else {
                ControlError::Other(e.message)
            }
        })
    }

    async fn wake(&self, id: &str) -> Result<(), String> {
        let client = (self.client)().ok_or("You are not signed in")?;
        crate::sandbox::ensure_running(&client, id)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Where the proxy's upstream tokens live between runs, one per sandbox.
pub trait TokenStore: Send + Sync {
    fn load(&self, id: &str) -> Result<Option<String>, String>;
    fn save(&self, id: &str, token: &str) -> Result<(), String>;
    fn remove(&self, id: &str) -> Result<(), String>;
}

/// The OS keychain, next to the app's other secrets: one entry, account `t3-proxy-tokens`,
/// holding a JSON map from sandbox id to token.
pub struct KeyringTokenStore;

const KEYCHAIN_ACCOUNT: &str = "t3-proxy-tokens";
/// Serializes read-change-write of the one entry.
static KEYCHAIN_LOCK: Mutex<()> = Mutex::new(());

impl KeyringTokenStore {
    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new(crate::keychain_service(), KEYCHAIN_ACCOUNT).map_err(|e| e.to_string())
    }

    fn read() -> Result<HashMap<String, String>, String> {
        match Self::entry()?.get_password() {
            Ok(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            Err(keyring::Error::NoEntry) => Ok(HashMap::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn write(all: &HashMap<String, String>) -> Result<(), String> {
        if all.is_empty() {
            return match Self::entry()?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.to_string()),
            };
        }
        let json = serde_json::to_string(all).map_err(|e| e.to_string())?;
        Self::entry()?
            .set_password(&json)
            .map_err(|e| e.to_string())
    }
}

impl TokenStore for KeyringTokenStore {
    fn load(&self, id: &str) -> Result<Option<String>, String> {
        let _g = KEYCHAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Self::read()?.remove(id))
    }

    fn save(&self, id: &str, token: &str) -> Result<(), String> {
        let _g = KEYCHAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = Self::read()?;
        all.insert(id.to_string(), token.to_string());
        Self::write(&all)
    }

    fn remove(&self, id: &str) -> Result<(), String> {
        let _g = KEYCHAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = Self::read()?;
        if all.remove(id).is_some() {
            Self::write(&all)?;
        }
        Ok(())
    }
}

/// In-memory store, for tests.
#[derive(Default)]
pub struct MemoryTokenStore(Mutex<HashMap<String, String>>);

impl TokenStore for MemoryTokenStore {
    fn load(&self, id: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }
    fn save(&self, id: &str, token: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(id.into(), token.into());
        Ok(())
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}

/// The token `ISSUE_TOKEN_SCRIPT` printed: its last line, when it looks like one.
pub fn parse_token(stdout: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    let ok = line.len() >= 16
        && line.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '/' | '=')
        });
    ok.then(|| line.to_string())
}

/// A sandbox id fit to name a folder.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

struct Running {
    runtime: Arc<Runtime>,
    server: JoinHandle<()>,
}

pub struct Hub {
    dir: PathBuf,
    control: Arc<dyn Control>,
    tokens: Arc<dyn TokenStore>,
    tuning: Tuning,
    http: reqwest::Client,
    running: tokio::sync::Mutex<HashMap<String, Running>>,
}

impl Hub {
    pub fn new(
        dir: PathBuf,
        control: Arc<dyn Control>,
        tokens: Arc<dyn TokenStore>,
        tuning: Tuning,
    ) -> Arc<Hub> {
        Arc::new(Hub {
            dir,
            control,
            tokens,
            tuning,
            http: reqwest::Client::new(),
            running: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    fn records(&self) -> Vec<Record> {
        std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| std::fs::read(runtime::record_path(&e.path())).ok())
            .filter_map(|b| serde_json::from_slice::<Record>(&b).ok())
            .filter(|r| valid_id(&r.id))
            .collect()
    }

    /// Starts the proxies of the sandboxes paired through it before, so T3 Code finds them
    /// after InstaADE restarts. One whose token or port is gone is skipped and logged.
    pub async fn start_saved(self: &Arc<Self>) {
        for record in self.records() {
            let id = record.id.clone();
            let token = match self.tokens.load(&id) {
                Ok(Some(t)) => t,
                Ok(None) => {
                    log_warn!(
                        "{id}: no upstream token in the keychain; Open in T3 Code pairs it again"
                    );
                    continue;
                }
                Err(e) => {
                    log_warn!("{id}: reading the keychain: {e}");
                    continue;
                }
            };
            if let Err(e) = self.start(record, token).await {
                log_warn!("{id}: {e:#}");
            }
        }
    }

    /// The runtime of sandbox `id`, if its proxy runs.
    pub async fn runtime(&self, id: &str) -> Option<Arc<Runtime>> {
        self.running.lock().await.get(id).map(|r| r.runtime.clone())
    }

    /// Serves `record` on its port and follows its sandbox.
    async fn start(self: &Arc<Self>, record: Record, token: String) -> Result<Arc<Runtime>> {
        let id = record.id.clone();
        let listener = TcpListener::bind(("127.0.0.1", record.port))
            .await
            .with_context(|| format!("port {} is taken", record.port))?;
        let rt = Arc::new(Runtime::new(
            self.dir.join(&id),
            record,
            token,
            self.control.clone(),
            self.tuning,
        ));
        // Saved: the port is the sandbox's from here on.
        rt.update_record(|_| {});
        let server = {
            let rt = rt.clone();
            tokio::spawn(async move {
                if let Err(e) = super::serve(listener, rt.clone()).await {
                    log_warn!("{}: the proxy stopped: {e:#}", rt.id());
                }
            })
        };
        {
            let (hub, rt) = (self.clone(), rt.clone());
            tokio::spawn(async move {
                rt.clone().run().await;
                // Deleted: nothing is left to serve.
                hub.forget(&rt.id()).await;
            });
        }
        log_info!("{id}: serving on 127.0.0.1:{}", rt.record().port);
        self.running.lock().await.insert(
            id,
            Running {
                runtime: rt.clone(),
                server,
            },
        );
        Ok(rt)
    }

    /// Stops the proxy of a deleted sandbox and removes what it kept: its record, cache, chats,
    /// images and upstream token.
    async fn forget(&self, id: &str) {
        if let Some(r) = self.running.lock().await.remove(id) {
            r.server.abort();
        }
        let _ = std::fs::remove_dir_all(self.dir.join(id));
        if let Err(e) = self.tokens.remove(id) {
            log_warn!("{id}: removing its token: {e}");
        }
        log_info!("{id}: the sandbox is gone; its proxy stopped");
    }

    /// A free port for a new sandbox, bound: the first in the range no record has.
    async fn free_port(&self) -> Result<u16> {
        let taken: Vec<u16> = self.records().iter().map(|r| r.port).collect();
        for port in BASE_PORT..BASE_PORT + PORT_RANGE {
            if taken.contains(&port) {
                continue;
            }
            if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
                return Ok(port);
            }
        }
        bail!("no free port from {BASE_PORT}")
    }

    async fn environment_id(&self, upstream: &str) -> Result<String> {
        let v: Value = self
            .http
            .get(format!("{upstream}/.well-known/t3/environment"))
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        v["environmentId"]
            .as_str()
            .map(str::to_owned)
            .context("T3 on the sandbox sent no environment id")
    }

    async fn issue_token(&self, client: &Client, id: &str) -> Result<String> {
        let r = client
            .exec(
                id,
                &ExecRequest {
                    cmd: ISSUE_TOKEN_SCRIPT.to_string(),
                    timeout_seconds: Some(ISSUE_TIMEOUT_SECS),
                },
            )
            .await
            .map_err(|e| anyhow!(e.message))?;
        if r.exit_code != 0 || r.timed_out {
            bail!(
                "T3 did not issue a session for the proxy: {}",
                crate::sandbox::connect::tail_lines(&r.stderr)
            );
        }
        let token = parse_token(&r.stdout).context("T3 printed no session token")?;
        self.tokens.save(id, &token).map_err(|e| anyhow!(e))?;
        Ok(token)
    }

    /// Pairs T3 Code with the proxy of running sandbox `id`: publishes its T3 port, makes sure
    /// the proxy has a T3 session there, starts the proxy on its port, and returns the
    /// `t3code://pair` link to `http://localhost:<port>`. The sandbox must be running.
    pub async fn pair_link(
        self: &Arc<Self>,
        client: &Client,
        id: &str,
        name: &str,
    ) -> Result<String> {
        if !valid_id(id) {
            bail!("unexpected sandbox id");
        }
        let upstream = client
            .publish_port(id, t3::PORT)
            .await
            .map_err(|e| anyhow!(e.message))?
            .url
            .trim_end_matches('/')
            .to_owned();
        let environment_id = self.environment_id(&upstream).await?;
        let mut token = match self.tokens.load(id).map_err(|e| anyhow!(e))? {
            Some(t) => t,
            None => self.issue_token(client, id).await?,
        };
        let existing = self.runtime(id).await;
        let rt = match existing {
            Some(rt) => {
                rt.update_record(|r| {
                    r.name = name.to_string();
                    r.upstream_url = upstream.clone();
                    r.environment_id = environment_id.clone();
                });
                rt.set_upstream_token(token.clone());
                rt
            }
            None => {
                let saved = std::fs::read(runtime::record_path(&self.dir.join(id)))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Record>(&b).ok());
                let port = match saved.as_ref().map(|r| r.port) {
                    Some(p) => p,
                    None => self.free_port().await?,
                };
                let record = Record {
                    id: id.to_string(),
                    name: name.to_string(),
                    port,
                    environment_id,
                    upstream_url: upstream,
                    client_tokens: saved.map(|r| r.client_tokens).unwrap_or_default(),
                };
                self.start(record, token.clone()).await?
            }
        };
        // Open is a person: the sandbox is held again, released or not.
        rt.user_action();
        if let Err(e) = rt.ensure_awake().await {
            log_warn!("{id}: {e:#}");
        }
        // T3 refuses a session whose data was reset: one new session, then give up.
        if rt.prime().await.is_err() {
            token = self.issue_token(client, id).await?;
            rt.set_upstream_token(token);
            rt.prime().await?;
        }
        let code = rt.auth.new_pairing_code();
        Ok(t3::pairing_link(
            &format!("http://localhost:{}", rt.record().port),
            &code,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_the_last_line_when_it_looks_like_one() {
        assert_eq!(
            parse_token("t3_abcdefghijklmnop.QRST-uv_w\n").as_deref(),
            Some("t3_abcdefghijklmnop.QRST-uv_w")
        );
        assert_eq!(
            parse_token("warning: x\nabcdefghijklmnopq\n\n").as_deref(),
            Some("abcdefghijklmnopq")
        );
        assert_eq!(parse_token(""), None);
        assert_eq!(parse_token("short"), None);
        assert_eq!(parse_token("has a space in it ok"), None);
        assert_eq!(parse_token("abcdefghijklmnop;rm -rf /"), None);
    }

    #[test]
    fn the_issue_script_uses_the_service_base_dir() {
        assert!(
            ISSUE_TOKEN_SCRIPT
                .contains(r#"--base-dir "$HOME/.t3-instabox/$(cat /etc/machine-id)""#)
        );
        assert!(
            t3::PAIR_SCRIPT.contains(r#"--base-dir "$HOME/.t3-instabox/$(cat /etc/machine-id)""#)
        );
        assert!(ISSUE_TOKEN_SCRIPT.contains("--token-only"));
    }

    #[test]
    fn ids_name_folders_only_when_plain() {
        assert!(valid_id("m_abc-123"));
        for bad in ["", "..", "a/b", "a b", &"x".repeat(65)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn memory_tokens_round_trip() {
        let s = MemoryTokenStore::default();
        assert_eq!(s.load("a").unwrap(), None);
        s.save("a", "t").unwrap();
        assert_eq!(s.load("a").unwrap().as_deref(), Some("t"));
        s.remove("a").unwrap();
        assert_eq!(s.load("a").unwrap(), None);
    }
}
