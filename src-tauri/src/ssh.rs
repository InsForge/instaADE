//! SSH login for a sandbox: a long-lived token, the command to show, and the port-forward arguments.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

use chrono::{Duration, Utc};

use crate::api::{Client, Error, Result, SshToken};
use crate::gate::{Gate, guarded};

/// The error for a flow whose account signed out or changed before it could act.
pub fn account_changed() -> Error {
    Error {
        status: 0,
        code: "account_changed".into(),
        message: "The account changed. Nothing was done.".into(),
        retryable: false,
        retry_after: None,
    }
}

const TOKEN_NAME: &str = "Instabox";
const TOKEN_TTL_SECS: u64 = 30 * 24 * 60 * 60;
const REUSE_MARGIN_HOURS: i64 = 24;

/// Where SSH tokens live between runs, one per sandbox.
pub trait SshTokenStore: Send + Sync {
    fn load(&self, id: &str) -> std::result::Result<Option<SshToken>, String>;
    fn save(&self, id: &str, token: &SshToken) -> std::result::Result<(), String>;
    /// Forgets every stored token (sign-out). Succeeds when there is none.
    fn clear_all(&self) -> std::result::Result<(), String>;
}

/// The OS keychain: one entry, account `ssh-tokens`, holding a JSON map from sandbox id to
/// token, so sign-out can remove them all.
pub struct KeyringSshStore;

const SSH_ACCOUNT: &str = "ssh-tokens";
/// Serializes read-change-write of the one entry.
static SSH_LOCK: StdMutex<()> = StdMutex::new(());

type TokenMap = HashMap<String, SshToken>;

impl KeyringSshStore {
    fn entry() -> std::result::Result<keyring::Entry, String> {
        keyring::Entry::new(crate::keychain_service(), SSH_ACCOUNT).map_err(|e| e.to_string())
    }

    fn read() -> std::result::Result<TokenMap, String> {
        match Self::entry()?.get_password() {
            Ok(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            Err(keyring::Error::NoEntry) => Ok(TokenMap::new()),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl SshTokenStore for KeyringSshStore {
    fn load(&self, id: &str) -> std::result::Result<Option<SshToken>, String> {
        let _g = SSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Self::read()?.remove(id))
    }

    fn save(&self, id: &str, token: &SshToken) -> std::result::Result<(), String> {
        let _g = SSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = Self::read()?;
        all.insert(id.to_string(), token.clone());
        let json = serde_json::to_string(&all).map_err(|e| e.to_string())?;
        Self::entry()?
            .set_password(&json)
            .map_err(|e| e.to_string())
    }

    fn clear_all(&self) -> std::result::Result<(), String> {
        let _g = SSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// In-memory store, for tests.
#[derive(Default)]
pub struct MemorySshStore(StdMutex<TokenMap>);

impl SshTokenStore for MemorySshStore {
    fn load(&self, id: &str) -> std::result::Result<Option<SshToken>, String> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }
    fn save(&self, id: &str, token: &SshToken) -> std::result::Result<(), String> {
        self.0.lock().unwrap().insert(id.to_string(), token.clone());
        Ok(())
    }
    fn clear_all(&self) -> std::result::Result<(), String> {
        self.0.lock().unwrap().clear();
        Ok(())
    }
}

/// The sandbox's SSH token: the stored one while it has more than 24 h left, else a new one.
pub async fn token(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    gate: &dyn Gate,
) -> Result<SshToken> {
    token_within(client, store, id, gate, Duration::hours(REUSE_MARGIN_HOURS)).await
}

/// `token`, reusing the stored token only while it has more than `margin` left. A stored token
/// with no expiry, or none at all, makes a new one.
pub async fn token_within(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    gate: &dyn Gate,
    margin: Duration,
) -> Result<SshToken> {
    // The token still works this run if saving fails; the next run asks for a new one.
    token_and_save(client, store, id, gate, margin)
        .await
        .map(|(t, _)| t)
}

/// `token_within`, with what the store answered when it saved a new token (`Ok` for the stored
/// one). A token that was not saved still works; the caller decides what a failed save means.
pub async fn token_and_save(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    gate: &dyn Gate,
    margin: Duration,
) -> Result<(SshToken, std::result::Result<(), String>)> {
    // A broken store only costs a fresh token, so treat a load failure as a miss.
    if let Ok(Some(t)) = store.load(id)
        && t.expires_at.is_some_and(|e| e > Utc::now() + margin)
    {
        return Ok((t, Ok(())));
    }
    let t = client.ssh_token(id, TOKEN_NAME, TOKEN_TTL_SECS).await?;
    // After a sign-out nothing is saved, and the caller gets no token either.
    let saved = guarded(gate, || store.save(id, &t)).ok_or_else(account_changed)?;
    Ok((t, saved))
}

/// The login command to show the user, as the server formatted it.
pub fn display_command(t: &SshToken) -> String {
    t.command.clone()
}

/// A login command that runs `remote` on the sandbox, such as `herdr`, in a terminal (`-t`), for
/// `open_in_terminal`. Like the ADEs' other SSH logins, it accepts the gateway's host key the
/// first time instead of asking.
pub fn terminal_command(t: &SshToken, remote: &str) -> String {
    format!(
        "ssh -t -o StrictHostKeyChecking=accept-new -p {} {}@{} {remote}",
        t.port, t.token, t.host
    )
}

/// File content for the macOS `.command` launcher.
#[cfg(any(target_os = "macos", test))]
fn command_file_content(cmd: &str) -> String {
    format!("#!/bin/sh\nexec {cmd}\n")
}

/// The words of a login command (`ssh -p 2222 ibssh_x@host`) when none needs quoting, so
/// they pass to a Windows console as they are. None for anything else.
#[cfg(any(windows, test))]
fn plain_words(cmd: &str) -> Option<Vec<&str>> {
    let plain = |w: &&str| {
        w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.@:=/+,".contains(c))
    };
    let words: Vec<&str> = cmd.split_whitespace().collect();
    (!words.is_empty() && words.iter().all(plain)).then_some(words)
}

/// Open the login in a terminal: Terminal on macOS; Windows Terminal, else a console window,
/// on Windows. The token goes on the command line, never into a file.
pub fn open_in_terminal(cmd: &str) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = std::env::temp_dir().join(format!("instabox-{}.command", uuid::Uuid::new_v4()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)
            .map_err(|e| e.to_string())?;
        f.write_all(command_file_content(cmd).as_bytes())
            .map_err(|e| e.to_string())?;
        drop(f);
        let status = std::process::Command::new("open")
            .args(["-a", "Terminal"])
            .arg(&path)
            .status();
        // The file holds the token: remove it shortly after Terminal has read it.
        let cleanup = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(10));
            let _ = std::fs::remove_file(cleanup);
        });
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("open exited with {s}")),
            Err(e) => Err(e.to_string()),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let words = plain_words(cmd).ok_or("The login command cannot be opened in a terminal")?;
        if crate::process::command("wt.exe")
            .arg("new-tab")
            .args(&words)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        // cmd /k keeps the window open after ssh ends, so its last message stays readable.
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        std::process::Command::new("cmd.exe")
            .raw_arg(format!("/k {}", words.join(" ")))
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(drop)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = cmd;
        Err("Open in Terminal is only on macOS and Windows for now".to_string())
    }
}

/// `ssh` arguments that forward local `port` to the same port on the sandbox.
pub fn forward_args(t: &SshToken, port: u16) -> Vec<String> {
    [
        "-N".to_string(),
        "-L".to_string(),
        format!("{port}:127.0.0.1:{port}"),
        "-p".to_string(),
        t.port.to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=30".to_string(),
        format!("{}@{}", t.token, t.host),
    ]
    .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Credentials;
    use async_trait::async_trait;
    use std::sync::Arc;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Creds;

    #[async_trait]
    impl Credentials for Creds {
        async fn bearer(&self) -> Result<String> {
            Ok("iba_x".into())
        }
        async fn refreshed(&self) -> Result<String> {
            Ok("iba_new".into())
        }
    }

    fn client(server: &MockServer) -> Client {
        Client::new(Url::parse(&server.uri()).unwrap(), Arc::new(Creds))
    }

    fn tok(token: &str, expires_in: Duration) -> SshToken {
        SshToken {
            host: "ssh.example.test".into(),
            port: 2222,
            token: token.into(),
            command: format!("ssh -p 2222 {token}@ssh.example.test"),
            expires_at: Some(Utc::now() + expires_in),
            host_key_fingerprint: None,
        }
    }

    fn body_for(t: &SshToken) -> serde_json::Value {
        serde_json::json!({
            "id": "k1", "name": "Instabox", "host": t.host, "port": t.port,
            "token": t.token, "command": t.command, "expiresAt": t.expires_at,
        })
    }

    #[test]
    fn forward_args_exact() {
        let t = tok("ibssh_abc", Duration::days(1));
        assert_eq!(
            forward_args(&t, 1455),
            [
                "-N",
                "-L",
                "1455:127.0.0.1:1455",
                "-p",
                "2222",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=30",
                "ibssh_abc@ssh.example.test",
            ]
        );
    }

    #[test]
    fn command_file_content_is_a_shell_exec() {
        assert_eq!(
            command_file_content("ssh -p 2222 ibssh_abc@h"),
            "#!/bin/sh\nexec ssh -p 2222 ibssh_abc@h\n"
        );
    }

    #[test]
    fn plain_words_take_only_a_command_that_needs_no_quoting() {
        assert_eq!(
            plain_words("ssh -p 2222 ibssh_a-B9@ssh.example.test"),
            Some(vec!["ssh", "-p", "2222", "ibssh_a-B9@ssh.example.test"])
        );
        assert_eq!(
            plain_words("ssh ibssh_x@152.236.1.51"),
            Some(vec!["ssh", "ibssh_x@152.236.1.51"])
        );
        for bad in [
            "",
            "ssh x@h & calc",
            "ssh x@h; calc",
            "ssh \"x@h\"",
            "ssh %PATH%@h",
            "ssh x@h|more",
            "ssh ^x@h",
            "ssh x@h > out",
            "ssh !x!@h",
            "ssh (x)@h",
        ] {
            assert_eq!(plain_words(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_terminal_command_runs_the_remote_command_with_a_terminal() {
        let t = tok("ibssh_a-B9", Duration::days(1));
        let cmd = terminal_command(&t, "herdr");
        assert_eq!(
            cmd,
            "ssh -t -o StrictHostKeyChecking=accept-new -p 2222 ibssh_a-B9@ssh.example.test herdr"
        );
        // Windows opens it as it is, with nothing to quote.
        assert_eq!(plain_words(&cmd).map(|w| w.len()), Some(8));
        assert_eq!(
            command_file_content(&cmd),
            format!("#!/bin/sh\nexec {cmd}\n")
        );
    }

    #[test]
    fn display_command_is_verbatim() {
        let t = tok("ibssh_abc", Duration::days(1));
        assert_eq!(display_command(&t), t.command);
    }

    #[tokio::test]
    async fn reuses_token_with_more_than_a_day_left() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ssh-tokens"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        let store = MemorySshStore::default();
        let stored = tok("ibssh_old", Duration::days(10));
        store.save("m1", &stored).unwrap();
        let got = token(&client(&server), &store, "m1", &crate::gate::Open)
            .await
            .unwrap();
        assert_eq!(got, stored);
    }

    #[tokio::test]
    async fn renews_token_close_to_expiry() {
        let server = MockServer::start().await;
        let fresh = tok("ibssh_new", Duration::days(30));
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ssh-tokens"))
            .and(body_json(
                serde_json::json!({"name": "Instabox", "ttlSeconds": 2592000}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(body_for(&fresh)))
            .expect(1)
            .mount(&server)
            .await;
        let store = MemorySshStore::default();
        store
            .save("m1", &tok("ibssh_old", Duration::hours(2)))
            .unwrap();
        let got = token(&client(&server), &store, "m1", &crate::gate::Open)
            .await
            .unwrap();
        assert_eq!(got.token, "ibssh_new");
        assert_eq!(store.load("m1").unwrap().unwrap().token, "ibssh_new");
    }

    #[tokio::test]
    async fn a_closed_gate_saves_nothing_and_returns_no_token() {
        struct Closed;
        impl Gate for Closed {
            fn run(&self, _: &mut dyn FnMut()) -> bool {
                false
            }
        }
        let server = MockServer::start().await;
        let fresh = tok("ibssh_new", Duration::days(30));
        Mock::given(method("POST"))
            .and(path("/v1/machines/m3/ssh-tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(body_for(&fresh)))
            .mount(&server)
            .await;
        let store = MemorySshStore::default();
        let err = token(&client(&server), &store, "m3", &Closed)
            .await
            .unwrap_err();
        assert_eq!(err.code, "account_changed");
        assert!(store.load("m3").unwrap().is_none());
    }

    #[tokio::test]
    async fn creates_token_when_none_stored() {
        let server = MockServer::start().await;
        let fresh = tok("ibssh_new", Duration::days(30));
        Mock::given(method("POST"))
            .and(path("/v1/machines/m2/ssh-tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(body_for(&fresh)))
            .expect(1)
            .mount(&server)
            .await;
        let store = MemorySshStore::default();
        let got = token(&client(&server), &store, "m2", &crate::gate::Open)
            .await
            .unwrap();
        assert_eq!(got.token, "ibssh_new");
        assert!(store.load("m2").unwrap().is_some());
    }
}
