//! Adds the hosted instabox MCP server to the user's Claude Code and Codex.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use url::Url;

use crate::api::Client;
use crate::gate::{Gate, guarded};
use crate::process::{RunError, command, run_bounded};

const KEYCHAIN_ACCOUNT: &str = "mcp-key";
const KEY_NAME: &str = "Instabox MCP";
const KEY_SCOPES: [&str; 4] = [
    "machines:read",
    "machines:write",
    "machines:delete",
    "images:read",
];
/// The key lives a year; adding MCP again after it expires makes a new one.
const KEY_DAYS: u32 = 365;
const SERVER_NAME: &str = "instabox";
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum McpClient {
    ClaudeCode,
    Codex,
}

/// Where the MCP API key lives between runs.
pub trait KeyStore: Send + Sync {
    fn load(&self) -> std::result::Result<Option<String>, String>;
    fn save(&self, key: &str) -> std::result::Result<(), String>;
    /// Forgets the stored key. Succeeds when there is none.
    fn clear(&self) -> std::result::Result<(), String>;
}

/// The OS keychain.
pub struct KeyringKeyStore;

impl KeyringKeyStore {
    fn entry() -> std::result::Result<keyring::Entry, String> {
        keyring::Entry::new(crate::keychain_service(), KEYCHAIN_ACCOUNT).map_err(|e| e.to_string())
    }
}

impl KeyStore for KeyringKeyStore {
    fn load(&self) -> std::result::Result<Option<String>, String> {
        match Self::entry()?.get_password() {
            Ok(k) => Ok(Some(k)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn save(&self, key: &str) -> std::result::Result<(), String> {
        Self::entry()?.set_password(key).map_err(|e| e.to_string())
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
pub struct MemoryKeyStore(Mutex<Option<String>>);

impl KeyStore for MemoryKeyStore {
    fn load(&self) -> std::result::Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, key: &str) -> std::result::Result<(), String> {
        *self.0.lock().unwrap() = Some(key.to_string());
        Ok(())
    }
    fn clear(&self) -> std::result::Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

/// Result of one CLI call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOutput {
    pub code: i32,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// The `claude` CLI is not installed.
    NotFound,
    Failed(String),
}

/// Runs the `claude` CLI. Args are passed as a list, never through a shell.
pub trait CliRunner: Send + Sync {
    fn run(&self, args: &[String]) -> std::result::Result<CliOutput, CliError>;
}

/// Runs the real `claude` CLI, found on the login shell's PATH or the inherited one.
pub struct SystemRunner;

impl SystemRunner {
    /// The `claude` binary and the PATH to run it with. An app started from Finder gets a
    /// minimal PATH, and an npm-installed `claude` (`#!/usr/bin/env node`) needs `node` on
    /// it, so the CLI runs with the login shell's PATH when it can be read.
    fn locate() -> Option<(PathBuf, Option<OsString>)> {
        let path = shell_path();
        let in_shell = path.as_ref().and_then(|p| {
            let cwd = std::env::current_dir().ok()?;
            which::which_in("claude", Some(p), cwd).ok()
        });
        let bin = in_shell.or_else(|| which::which("claude").ok())?;
        Some((bin, path))
    }
}

/// The login shell's PATH, read once per run of the app.
fn shell_path() -> Option<OsString> {
    static PATH: OnceLock<Option<OsString>> = OnceLock::new();
    PATH.get_or_init(login_shell_path).clone()
}

fn login_shell_path() -> Option<OsString> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty())?;
    login_shell_path_with(&shell)
}

fn login_shell_path_with(shell: &str) -> Option<OsString> {
    let mut cmd = command(shell);
    cmd.args(["-lic", "echo $PATH"]);
    let (_, stdout, _) = run_bounded(cmd, SHELL_TIMEOUT).ok()?;
    let text = String::from_utf8_lossy(&stdout).into_owned();
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    Some(line.trim().into())
}

/// The CLI command: `bin` with `args`, and `path` as its PATH when given (else the inherited one).
fn cli_command(bin: &Path, args: &[String], path: Option<&OsStr>) -> Command {
    let mut cmd = command(bin);
    cmd.args(args);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    cmd
}

fn run_cli(
    bin: &Path,
    args: &[String],
    path: Option<&OsStr>,
) -> std::result::Result<CliOutput, CliError> {
    let (code, _, stderr) =
        run_bounded(cli_command(bin, args, path), CLI_TIMEOUT).map_err(|e| match e {
            RunError::TimedOut => CliError::Failed("the claude command timed out".into()),
            RunError::Spawn(e) => CliError::Failed(format!("cannot run claude: {e}")),
        })?;
    Ok(CliOutput {
        code: code.unwrap_or(-1),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

impl CliRunner for SystemRunner {
    fn run(&self, args: &[String]) -> std::result::Result<CliOutput, CliError> {
        let (bin, path) = Self::locate().ok_or(CliError::NotFound)?;
        run_cli(&bin, args, path.as_deref())
    }
}

/// The hosted MCP server of the API at `base`: `<base>/mcp`, so `INSTABOX_API_URL` moves it too.
pub fn mcp_url(base: &Url) -> String {
    let mut u = base.clone();
    u.set_query(None);
    u.set_fragment(None);
    if let Ok(mut segments) = u.path_segments_mut() {
        segments.pop_if_empty().push("mcp");
    }
    u.to_string()
}

fn codex_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

fn get_args() -> Vec<String> {
    ["mcp", "get", SERVER_NAME].map(String::from).to_vec()
}

fn add_args(url: &str, key: &str) -> Vec<String> {
    let mut v: Vec<String> = [
        "mcp",
        "add",
        "--transport",
        "http",
        "--scope",
        "user",
        SERVER_NAME,
        url,
        "--header",
    ]
    .map(String::from)
    .to_vec();
    v.push(format!("X-API-Key: {key}"));
    v
}

/// True when the client already has the instabox server. False when Claude Code is not installed.
pub fn configured(home: &Path, client: McpClient) -> bool {
    configured_with(home, client, &SystemRunner)
}

pub fn configured_with(home: &Path, client: McpClient, runner: &dyn CliRunner) -> bool {
    match client {
        McpClient::ClaudeCode => matches!(runner.run(&get_args()), Ok(o) if o.code == 0),
        McpClient::Codex => std::fs::read_to_string(codex_path(home))
            .ok()
            .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
            .is_some_and(|d| has_server(&d)),
    }
}

/// True when the `claude` CLI can be found.
pub fn claude_installed() -> bool {
    SystemRunner::locate().is_some()
}

/// Adds the instabox MCP server at `url` to `client`, creating the API key in `keys` on
/// first use. Saving the key, writing the Codex config and the whole `claude mcp add` run
/// happen inside `gate`.
pub async fn add(
    api: &Client,
    url: &str,
    home: &Path,
    client: McpClient,
    keys: Arc<dyn KeyStore>,
    gate: Arc<dyn Gate>,
) -> Result<()> {
    let api_key_session = std::env::var("INSTABOX_API_KEY").is_ok_and(|k| !k.is_empty());
    add_with(
        api,
        url,
        home,
        client,
        keys,
        Arc::new(SystemRunner),
        api_key_session,
        gate,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn add_with(
    api: &Client,
    url: &str,
    home: &Path,
    client: McpClient,
    keys: Arc<dyn KeyStore>,
    runner: Arc<dyn CliRunner>,
    api_key_session: bool,
    gate: Arc<dyn Gate>,
) -> Result<()> {
    if client == McpClient::ClaudeCode {
        let r = runner.clone();
        let present = tokio::task::spawn_blocking(move || r.run(&get_args()))
            .await
            .context("claude check failed")?;
        match present {
            Err(CliError::NotFound) => bail!("Claude Code is not installed"),
            Ok(o) if o.code == 0 => return Ok(()),
            _ => {}
        }
    }
    if client == McpClient::Codex && read_codex(&codex_path(home))?.is_some_and(|d| has_server(&d))
    {
        return Ok(());
    }
    let key = api_key(api, keys.as_ref(), api_key_session, &*gate).await?;
    match client {
        McpClient::ClaudeCode => {
            // The whole command runs inside the gate, on its own thread: an account change
            // waits for it (at most CLI_TIMEOUT), and after one it never starts.
            let args = add_args(url, &key);
            let out = tokio::task::spawn_blocking(move || guarded(&*gate, || runner.run(&args)))
                .await
                .context("claude add failed")?
                .ok_or_else(account_changed)?
                .map_err(|e| match e {
                    CliError::NotFound => anyhow!("Claude Code is not installed"),
                    CliError::Failed(m) => anyhow!("claude mcp add failed: {m}"),
                })?;
            if out.code != 0 {
                bail!("claude mcp add failed: {}", out.stderr.trim());
            }
            Ok(())
        }
        McpClient::Codex => {
            guarded(&*gate, || write_codex(home, url, &key)).ok_or_else(account_changed)?
        }
    }
}

fn account_changed() -> anyhow::Error {
    anyhow!("The account changed. Nothing was done.")
}

/// True when `mcp_servers.instabox` exists in any form (table, inline table, stdio or http).
fn has_server(doc: &toml_edit::DocumentMut) -> bool {
    doc.get("mcp_servers")
        .and_then(|s| s.get(SERVER_NAME))
        .is_some()
}

/// Reads the Codex config. `None` when the file is missing; an error when it does not parse.
fn read_codex(path: &Path) -> Result<Option<toml_edit::DocumentMut>> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .map(Some)
            .map_err(|e| anyhow!("{} is not valid TOML: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// The stored key, or a new one. An API-key session cannot create keys.
async fn api_key(
    api: &Client,
    keys: &dyn KeyStore,
    api_key_session: bool,
    gate: &dyn Gate,
) -> Result<String> {
    if let Some(k) = keys.load().map_err(|e| anyhow!("keychain: {e}"))? {
        return Ok(k);
    }
    if api_key_session {
        bail!("Sign in to add MCP");
    }
    let created = api.create_api_key(KEY_NAME, &KEY_SCOPES, KEY_DAYS).await?;
    // After a sign-out the new key is neither saved nor used.
    guarded(gate, || keys.save(&created.key))
        .ok_or_else(account_changed)?
        .map_err(|e| anyhow!("keychain: {e}"))?;
    Ok(created.key)
}

fn write_codex(home: &Path, url: &str, key: &str) -> Result<()> {
    use toml_edit::{DocumentMut, InlineTable, Item, Table, value};

    let path = codex_path(home);
    let mut doc: DocumentMut = read_codex(&path)?.unwrap_or_default();
    // An existing entry in any form is the user's; never modify it.
    if has_server(&doc) {
        return Ok(());
    }
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        })
        .as_table_mut()
        .ok_or_else(|| anyhow!("mcp_servers in {} is not a table", path.display()))?;
    let server = servers
        .entry(SERVER_NAME)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("mcp_servers.{SERVER_NAME} is not a table"))?;
    server["url"] = value(url);
    let mut headers = InlineTable::new();
    headers.insert("X-API-Key", key.into());
    server["http_headers"] = value(headers);
    write_atomic(&path, doc.to_string().as_bytes())
}

/// Writes a temp file in the same directory (mode 0600), then renames it over `path`.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    // A symlinked config stays a link: write next to its target instead.
    let target;
    let path = if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        target = std::fs::canonicalize(path)
            .with_context(|| format!("cannot resolve {}", path.display()))?;
        target.as_path()
    } else {
        path
    };
    let dir = path.parent().context("config path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = dir.join(format!(".config.toml.tmp-{}", uuid::Uuid::new_v4()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let res = (|| -> std::io::Result<()> {
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res.with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Credentials, Result as ApiResult};
    use crate::gate::Open;
    use async_trait::async_trait;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Bearer;

    #[async_trait]
    impl Credentials for Bearer {
        async fn bearer(&self) -> ApiResult<String> {
            Ok("t".into())
        }
        async fn refreshed(&self) -> ApiResult<String> {
            Ok("t".into())
        }
    }

    fn client(server: &MockServer) -> Client {
        Client::new(Url::parse(&server.uri()).unwrap(), Arc::new(Bearer))
    }

    #[derive(Default)]
    struct Fake {
        get_code: i32,
        not_found: bool,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl CliRunner for Fake {
        fn run(&self, args: &[String]) -> std::result::Result<CliOutput, CliError> {
            if self.not_found {
                return Err(CliError::NotFound);
            }
            self.calls.lock().unwrap().push(args.to_vec());
            let code = if args[1] == "get" { self.get_code } else { 0 };
            Ok(CliOutput {
                code,
                stderr: String::new(),
            })
        }
    }

    async fn mount_key(server: &MockServer, times: u64) {
        Mock::given(method("POST"))
            .and(path("/v1/keys"))
            .and(body_json(serde_json::json!({
                "name": "Instabox MCP",
                "scopes": ["machines:read", "machines:write", "machines:delete", "images:read"],
                "expiresInDays": 365
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "apiKey": {"id": "k1"}, "key": "ibk_test"
            })))
            .expect(times)
            .mount(server)
            .await;
    }

    /// The MCP URL of the production API.
    fn prod_url() -> String {
        mcp_url(&crate::app::api_base(None))
    }

    async fn add_codex(server: &MockServer, home: &Path, keys: Arc<MemoryKeyStore>) -> Result<()> {
        add_with(
            &client(server),
            &prod_url(),
            home,
            McpClient::Codex,
            keys,
            Arc::new(Fake::default()),
            false,
            Arc::new(Open),
        )
        .await
    }

    #[tokio::test]
    async fn codex_from_nothing_creates_a_private_file() {
        let server = MockServer::start().await;
        mount_key(&server, 1).await;
        let home = tempfile::tempdir().unwrap();
        assert!(!configured_with(
            home.path(),
            McpClient::Codex,
            &Fake::default()
        ));
        add_codex(&server, home.path(), Default::default())
            .await
            .unwrap();
        let file = home.path().join(".codex/config.toml");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("https://api.instabox.sh/mcp"));
        assert!(text.contains("X-API-Key"));
        assert!(text.contains("ibk_test"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(configured_with(
            home.path(),
            McpClient::Codex,
            &Fake::default()
        ));
    }

    #[tokio::test]
    async fn codex_keeps_other_servers_keys_and_comments() {
        let server = MockServer::start().await;
        mount_key(&server, 1).await;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        let before = "# my codex settings\nmodel = \"gpt-5\"\n\n# keep me\n[mcp_servers.other]\ncommand = \"npx\"\nargs = [\"-y\", \"x\"]\n\n[projects.foo]\ntrust = \"trusted\"\n";
        std::fs::write(dir.join("config.toml"), before).unwrap();
        add_codex(&server, home.path(), Default::default())
            .await
            .unwrap();
        let after = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        for keep in [
            "# my codex settings",
            "model = \"gpt-5\"",
            "# keep me",
            "[mcp_servers.other]",
            "command = \"npx\"",
            "args = [\"-y\", \"x\"]",
            "[projects.foo]",
            "trust = \"trusted\"",
        ] {
            assert!(after.contains(keep), "lost {keep:?} in:\n{after}");
        }
        assert!(configured_with(
            home.path(),
            McpClient::Codex,
            &Fake::default()
        ));
    }

    #[tokio::test]
    async fn codex_unparsable_config_errors_and_is_not_touched() {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        let before = b"this is = = not toml [[";
        std::fs::write(dir.join("config.toml"), before).unwrap();
        // The key exists already, so the failure comes from the parse, not the key call.
        let keys = Arc::new(MemoryKeyStore::default());
        keys.save("ibk_test").unwrap();
        let err = add_codex(&server, home.path(), keys).await.unwrap_err();
        assert!(err.to_string().contains("not valid TOML"), "{err}");
        assert_eq!(std::fs::read(dir.join("config.toml")).unwrap(), before);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn claude_code_adds_when_get_fails() {
        let server = MockServer::start().await;
        mount_key(&server, 1).await;
        let fake = Arc::new(Fake {
            get_code: 1,
            ..Default::default()
        });
        add_with(
            &client(&server),
            &prod_url(),
            Path::new("/nonexistent"),
            McpClient::ClaudeCode,
            Arc::new(MemoryKeyStore::default()),
            fake.clone(),
            false,
            Arc::new(Open),
        )
        .await
        .unwrap();
        let calls = fake.calls.lock().unwrap();
        let adds: Vec<_> = calls.iter().filter(|c| c[1] == "add").collect();
        assert_eq!(adds.len(), 1);
        let want: Vec<String> = [
            "mcp",
            "add",
            "--transport",
            "http",
            "--scope",
            "user",
            "instabox",
            "https://api.instabox.sh/mcp",
            "--header",
            "X-API-Key: ibk_test",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(*adds[0], want);
    }

    /// A gate that is open or closed, and says whether a call happens inside it.
    struct Watched {
        open: bool,
        inside: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Gate for Watched {
        fn run(&self, f: &mut dyn FnMut()) -> bool {
            if !self.open {
                return false;
            }
            self.inside.store(true, std::sync::atomic::Ordering::SeqCst);
            f();
            self.inside
                .store(false, std::sync::atomic::Ordering::SeqCst);
            true
        }
    }

    /// Records whether each `claude` call ran inside the gate.
    struct InsideRunner {
        inside: Arc<std::sync::atomic::AtomicBool>,
        adds: Mutex<Vec<bool>>,
    }

    impl CliRunner for InsideRunner {
        fn run(&self, args: &[String]) -> std::result::Result<CliOutput, CliError> {
            let code = if args[1] == "get" {
                1
            } else {
                let inside = self.inside.load(std::sync::atomic::Ordering::SeqCst);
                self.adds.lock().unwrap().push(inside);
                0
            };
            Ok(CliOutput {
                code,
                stderr: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn claude_code_add_runs_inside_the_gate_and_not_after_it_closed() {
        for open in [true, false] {
            let server = MockServer::start().await;
            let keys = Arc::new(MemoryKeyStore::default());
            keys.save("ibk_test").unwrap();
            let inside = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let runner = Arc::new(InsideRunner {
                inside: inside.clone(),
                adds: Mutex::new(Vec::new()),
            });
            let r = add_with(
                &client(&server),
                &prod_url(),
                Path::new("/nonexistent"),
                McpClient::ClaudeCode,
                keys,
                runner.clone(),
                false,
                Arc::new(Watched { open, inside }),
            )
            .await;
            let adds = runner.adds.lock().unwrap().clone();
            if open {
                r.unwrap();
                assert_eq!(adds, vec![true], "the add ran inside the gate");
            } else {
                assert!(r.unwrap_err().to_string().contains("account changed"));
                assert!(adds.is_empty(), "no add after the gate closed");
            }
        }
    }

    #[tokio::test]
    async fn claude_code_does_nothing_when_get_succeeds() {
        let server = MockServer::start().await;
        mount_key(&server, 0).await;
        let fake = Arc::new(Fake::default());
        add_with(
            &client(&server),
            &prod_url(),
            Path::new("/nonexistent"),
            McpClient::ClaudeCode,
            Arc::new(MemoryKeyStore::default()),
            fake.clone(),
            false,
            Arc::new(Open),
        )
        .await
        .unwrap();
        {
            let calls = fake.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0][1], "get");
        }
        assert!(configured_with(
            Path::new("/x"),
            McpClient::ClaudeCode,
            fake.as_ref()
        ));
    }

    #[tokio::test]
    async fn claude_code_not_installed() {
        let server = MockServer::start().await;
        let fake = Fake {
            not_found: true,
            ..Default::default()
        };
        assert!(!configured_with(
            Path::new("/x"),
            McpClient::ClaudeCode,
            &fake
        ));
        let err = add_with(
            &client(&server),
            &prod_url(),
            Path::new("/x"),
            McpClient::ClaudeCode,
            Arc::new(MemoryKeyStore::default()),
            Arc::new(fake),
            false,
            Arc::new(Open),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not installed"));
    }

    #[tokio::test]
    async fn key_is_created_once_and_reused() {
        let server = MockServer::start().await;
        mount_key(&server, 1).await;
        let home = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::default());
        add_codex(&server, home.path(), keys.clone()).await.unwrap();
        add_codex(&server, home.path(), keys.clone()).await.unwrap();
        assert_eq!(keys.load().unwrap().as_deref(), Some("ibk_test"));
    }

    #[tokio::test]
    async fn api_key_session_cannot_add() {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().unwrap();
        let err = add_with(
            &client(&server),
            &prod_url(),
            home.path(),
            McpClient::Codex,
            Arc::new(MemoryKeyStore::default()),
            Arc::new(Fake::default()),
            true,
            Arc::new(Open),
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Sign in to add MCP");
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(!home.path().join(".codex").exists());
    }

    #[tokio::test]
    async fn codex_existing_entry_in_any_form_is_left_alone() {
        for existing in [
            "[mcp_servers.instabox]\ncommand = \"instabox\"\nargs = [\"mcp\"]\nenabled = false\n",
            "[mcp_servers]\ninstabox = { command = \"instabox\", args = [\"mcp\"] }\n",
        ] {
            let server = MockServer::start().await;
            mount_key(&server, 0).await;
            let home = tempfile::tempdir().unwrap();
            let dir = home.path().join(".codex");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.toml"), existing).unwrap();
            assert!(configured_with(
                home.path(),
                McpClient::Codex,
                &Fake::default()
            ));
            add_codex(&server, home.path(), Default::default())
                .await
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.join("config.toml")).unwrap(),
                existing
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_symlinked_config_stays_a_link() {
        let server = MockServer::start().await;
        mount_key(&server, 1).await;
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().join("real.toml");
        std::fs::write(&target, "model = \"gpt-5\"\n").unwrap();
        let dir = home.path().join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        add_codex(&server, home.path(), Default::default())
            .await
            .unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("model = \"gpt-5\"") && text.contains("ibk_test"));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn shell_lookup_is_bounded_when_a_background_process_holds_the_pipe() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("fakeshell");
        std::fs::write(&shell, "#!/bin/sh\nsleep 30 &\necho /usr/bin:/bin\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let start = std::time::Instant::now();
        let path = login_shell_path_with(shell.to_str().unwrap());
        assert!(
            start.elapsed() < Duration::from_secs(6),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(path.unwrap(), "/usr/bin:/bin");
    }

    #[test]
    fn the_mcp_url_follows_the_api_base() {
        let url = |b: &str| mcp_url(&Url::parse(b).unwrap());
        assert_eq!(prod_url(), "https://api.instabox.sh/mcp");
        assert_eq!(url("http://localhost:8080"), "http://localhost:8080/mcp");
        assert_eq!(
            url("https://staging.example.test/api/"),
            "https://staging.example.test/api/mcp"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_cli_runs_with_the_login_shell_path() {
        use std::os::unix::fs::PermissionsExt;
        // A stand-in for claude that prints the PATH it was given.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("claude");
        std::fs::write(&fake, "#!/bin/sh\necho \"$PATH\" >&2\nexit 4\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let login = "/opt/homebrew/bin:/usr/bin:/bin";
        let out = run_cli(&fake, &get_args(), Some(OsStr::new(login))).unwrap();
        assert_eq!(out.code, 4);
        assert_eq!(out.stderr.trim(), login);
        // Without a login PATH the child keeps the inherited one.
        let out = run_cli(&fake, &get_args(), None).unwrap();
        assert_eq!(out.stderr.trim(), std::env::var("PATH").unwrap_or_default());
    }

    #[test]
    fn client_serializes_for_the_ui() {
        assert_eq!(
            serde_json::to_string(&McpClient::ClaudeCode).unwrap(),
            "\"claudeCode\""
        );
        assert_eq!(
            serde_json::to_string(&McpClient::Codex).unwrap(),
            "\"codex\""
        );
    }
}
