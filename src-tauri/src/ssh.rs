//! SSH login for a sandbox: its two long-lived tokens (`Kind`), the command to show, and the
//! port-forward arguments.

use std::collections::HashMap;
use std::path::Path;
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

/// Which of a sandbox's two SSH tokens. Each is stored apart and renewed by the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// For a login the user starts: the SSH command, Open in Terminal, Herdr, Paradev's host
    /// setup and an agent sign-in. Its connections wake the sandbox and count as activity.
    Ordinary,
    /// For an app that stays connected on its own and sends heartbeats (cmux, Orca, Paseo).
    /// While instabox applies background tokens, its connections neither wake the sandbox nor
    /// count as activity, so an idle app does not keep the sandbox awake. An agent at work
    /// still does, through the ADE's activity probe and the sandbox's terminals and CPU.
    Background,
}

/// Where SSH tokens live between runs, one of each kind per sandbox.
pub trait SshTokenStore: Send + Sync {
    fn load(&self, id: &str, kind: Kind) -> std::result::Result<Option<SshToken>, String>;
    fn save(&self, id: &str, kind: Kind, token: &SshToken) -> std::result::Result<(), String>;
    /// Forgets every stored token of both kinds (sign-out). Succeeds when there is none.
    fn clear_all(&self) -> std::result::Result<(), String>;
}

/// The OS keychain: one entry per kind, account `ssh-tokens` (ordinary) or
/// `ssh-tokens-background`, each holding a JSON map from sandbox id to token, so sign-out can
/// remove them all. The ordinary entry is the one older releases keep every token in.
pub struct KeyringSshStore;

const SSH_ACCOUNT: &str = "ssh-tokens";
const BACKGROUND_ACCOUNT: &str = "ssh-tokens-background";
/// Serializes read-change-write of the entries.
static SSH_LOCK: StdMutex<()> = StdMutex::new(());

type TokenMap = HashMap<String, SshToken>;

impl KeyringSshStore {
    fn account(kind: Kind) -> &'static str {
        match kind {
            Kind::Ordinary => SSH_ACCOUNT,
            Kind::Background => BACKGROUND_ACCOUNT,
        }
    }

    fn entry(kind: Kind) -> std::result::Result<keyring::Entry, String> {
        keyring::Entry::new(crate::keychain_service(), Self::account(kind))
            .map_err(|e| e.to_string())
    }

    fn read(kind: Kind) -> std::result::Result<TokenMap, String> {
        match Self::entry(kind)?.get_password() {
            Ok(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            Err(keyring::Error::NoEntry) => Ok(TokenMap::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn delete(kind: Kind) -> std::result::Result<(), String> {
        match Self::entry(kind)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl SshTokenStore for KeyringSshStore {
    fn load(&self, id: &str, kind: Kind) -> std::result::Result<Option<SshToken>, String> {
        let _g = SSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Self::read(kind)?.remove(id))
    }

    fn save(&self, id: &str, kind: Kind, token: &SshToken) -> std::result::Result<(), String> {
        let _g = SSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = Self::read(kind)?;
        all.insert(id.to_string(), token.clone());
        let json = serde_json::to_string(&all).map_err(|e| e.to_string())?;
        Self::entry(kind)?
            .set_password(&json)
            .map_err(|e| e.to_string())
    }

    /// Removes both entries, the second also when the first fails, and answers the first
    /// failure.
    fn clear_all(&self) -> std::result::Result<(), String> {
        let _g = SSH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let ordinary = Self::delete(Kind::Ordinary);
        let background = Self::delete(Kind::Background);
        ordinary.and(background)
    }
}

/// In-memory store, for tests.
#[derive(Default)]
pub struct MemorySshStore(StdMutex<HashMap<(Kind, String), SshToken>>);

impl SshTokenStore for MemorySshStore {
    fn load(&self, id: &str, kind: Kind) -> std::result::Result<Option<SshToken>, String> {
        Ok(self.0.lock().unwrap().get(&(kind, id.to_string())).cloned())
    }
    fn save(&self, id: &str, kind: Kind, token: &SshToken) -> std::result::Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert((kind, id.to_string()), token.clone());
        Ok(())
    }
    fn clear_all(&self) -> std::result::Result<(), String> {
        self.0.lock().unwrap().clear();
        Ok(())
    }
}

/// The sandbox's SSH token of `kind`: the stored one while it has more than 24 h left, else a
/// new one.
pub async fn token(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    kind: Kind,
    gate: &dyn Gate,
) -> Result<SshToken> {
    let margin = Duration::hours(REUSE_MARGIN_HOURS);
    token_within(client, store, id, kind, gate, margin).await
}

/// `token`, reusing the stored token only while it has more than `margin` left. A stored token
/// with no expiry, or none at all, makes a new one.
pub async fn token_within(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    kind: Kind,
    gate: &dyn Gate,
    margin: Duration,
) -> Result<SshToken> {
    // The token still works this run if saving fails; the next run asks for a new one.
    token_and_save(client, store, id, kind, gate, margin)
        .await
        .map(|(t, _)| t)
}

/// `token_within`, with what the store answered when it saved a new token (`Ok` for the stored
/// one). A token that was not saved still works; the caller decides what a failed save means.
/// A new background token is asked for with `background`. A server that does not know it makes
/// an ordinary token, whose reply has no `background`: that token is kept and used in its
/// place all the same (`ordinary_in_place_of_background`).
pub async fn token_and_save(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    kind: Kind,
    gate: &dyn Gate,
    margin: Duration,
) -> Result<(SshToken, std::result::Result<(), String>)> {
    // A broken store only costs a fresh token, so treat a load failure as a miss.
    if let Ok(Some(t)) = store.load(id, kind)
        && t.expires_at.is_some_and(|e| e > Utc::now() + margin)
    {
        return Ok((t, Ok(())));
    }
    let background = kind == Kind::Background;
    let t = client
        .ssh_token(id, TOKEN_NAME, TOKEN_TTL_SECS, background)
        .await?;
    // After a sign-out nothing is saved, and the caller gets no token either.
    let saved = guarded(gate, || store.save(id, kind, &t)).ok_or_else(account_changed)?;
    Ok((t, saved))
}

/// True when `t`, a token of `kind`, stands in for a background token but is an ordinary one,
/// as a server without background tokens makes.
pub fn ordinary_in_place_of_background(kind: Kind, t: &SshToken) -> bool {
    kind == Kind::Background && !t.background
}

/// The login command to show the user, as the server formatted it.
pub fn display_command(t: &SshToken) -> String {
    t.command.clone()
}

/// The error for a login whose host or token could hold something other than a login.
pub const NOT_PLAIN: &str =
    "The SSH login has characters a terminal could run as commands, so it was not opened.";

/// True when a field of the API's SSH login (its host or token) can stand on a command line as
/// it is: ASCII letters, digits, `.`, `_` and `-`, starting with a letter or digit. So it holds
/// no shell syntax (`;`, `$()`, backticks, quotes, spaces, newlines) and never reads as an ssh
/// option.
fn plain_field(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `ssh` arguments that log in to the sandbox as the token: `options`, then
/// `-p <port> <token>@<host>`, then `remote` when there is one. An error when the host, the
/// token or `remote` is not plain (`plain_field`).
fn login_with(
    t: &SshToken,
    options: &[&str],
    remote: Option<&str>,
) -> std::result::Result<Vec<String>, String> {
    if !plain_field(&t.host) || !plain_field(&t.token) || !remote.is_none_or(plain_field) {
        return Err(NOT_PLAIN.to_string());
    }
    let mut args: Vec<String> = options.iter().map(|o| o.to_string()).collect();
    args.extend([
        "-p".to_string(),
        t.port.to_string(),
        format!("{}@{}", t.token, t.host),
    ]);
    args.extend(remote.map(str::to_string));
    Ok(args)
}

/// The `ssh` arguments of Open in Terminal: `-p <port> <token>@<host>`, from the token's
/// fields, never from the command string the server formats.
pub fn login_args(t: &SshToken) -> std::result::Result<Vec<String>, String> {
    login_with(t, &[], None)
}

/// The `ssh` arguments that run `remote` on the sandbox, such as `herdr`, in a terminal (`-t`).
/// Like the ADEs' other SSH logins, they accept the gateway's host key the first time instead
/// of asking.
pub fn terminal_args(t: &SshToken, remote: &str) -> std::result::Result<Vec<String>, String> {
    login_with(
        t,
        &["-t", "-o", "StrictHostKeyChecking=accept-new"],
        Some(remote),
    )
}

/// True when an argument passes to a shell or a Windows console as it is: ASCII letters,
/// digits and `-_.@:=/+,`.
fn plain_arg(a: &str) -> bool {
    !a.is_empty()
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.@:=/+,".contains(c))
}

/// True when the ssh client's path can go into the macOS launcher as it is: ASCII letters,
/// digits and `/._-`, such as `/usr/bin/ssh` or `ssh`.
#[cfg(any(target_os = "macos", test))]
fn plain_unix_program(p: &str) -> bool {
    !p.is_empty()
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
}

/// True when the ssh client's path can go into a Windows console in quotes: no control
/// characters, and none of `"%;!^&|<>`, which quotes do not keep from cmd and Windows Terminal.
#[cfg(any(windows, test))]
fn plain_windows_program(p: &str) -> bool {
    !p.is_empty()
        && !p
            .chars()
            .any(|c| c.is_control() || "\"%;!^&|<>".contains(c))
}

/// Checks a login before any of it reaches a terminal: every argument is plain (`plain_arg`),
/// and so is the program (`plain_program`), so nothing in it can run as a command here.
fn check_login(
    program: &str,
    args: &[String],
    plain_program: fn(&str) -> bool,
) -> std::result::Result<(), String> {
    if plain_program(program) && args.iter().all(|a| plain_arg(a)) {
        Ok(())
    } else {
        Err(NOT_PLAIN.to_string())
    }
}

/// File content for the macOS `.command` launcher. Only for a login `check_login` passed: no
/// word needs quoting.
#[cfg(any(target_os = "macos", test))]
fn command_file_content(program: &str, args: &[String]) -> String {
    format!("#!/bin/sh\nexec {program} {}\n", args.join(" "))
}

/// What `cmd.exe` gets after its name: `/k` and the login, with the program in quotes, since
/// its path can have spaces (`C:\Program Files\...`). The outer quotes are the ones cmd strips.
/// Only for a login `check_login` passed.
#[cfg(any(windows, test))]
fn cmd_line(program: &str, args: &[String]) -> String {
    format!("/k \"\"{program}\" {}\"", args.join(" "))
}

/// Open the login, `program` with `args`, in a terminal: Terminal on macOS; Windows Terminal,
/// else a console window, on Windows. `program` is the ssh client to run, such as the one
/// `Ade::locate` found. The login is checked first (`check_login`), so nothing the API sent
/// can run as a command on this computer. The token goes on the command line, never into a
/// file on Windows; on macOS into a launcher only you can read, removed 10 s later.
pub fn open_in_terminal(program: &Path, args: &[String]) -> std::result::Result<(), String> {
    let program = program.to_str().ok_or(NOT_PLAIN)?;
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        check_login(program, args, plain_unix_program)?;
        let path = std::env::temp_dir().join(format!("instabox-{}.command", uuid::Uuid::new_v4()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)
            .map_err(|e| e.to_string())?;
        f.write_all(command_file_content(program, args).as_bytes())
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
        check_login(program, args, plain_windows_program)?;
        if crate::process::command("wt.exe")
            .arg("new-tab")
            .arg(program)
            .args(args)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        // cmd /k keeps the window open after ssh ends, so its last message stays readable.
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        std::process::Command::new("cmd.exe")
            .raw_arg(cmd_line(program, args))
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(drop)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (program, args);
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
            background: false,
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

    /// Hosts and tokens a broken or hostile API answer could hold: shell syntax, spaces,
    /// newlines, quotes, an ssh option, nothing at all.
    const HOSTILE: &[&str] = &[
        "x;open -a Calculator",
        "x$(touch /tmp/pwned)",
        "x`touch /tmp/pwned`",
        "x'y",
        "x\"y",
        "x\ntouch /tmp/pwned",
        "x y",
        "x&calc",
        "x|calc",
        "x>out",
        "-oProxyCommand=calc",
        "",
    ];

    #[test]
    fn open_in_terminal_logs_in_from_the_token_fields() {
        let t = tok("ibssh_a-B9", Duration::days(1));
        assert_eq!(
            login_args(&t).unwrap(),
            ["-p", "2222", "ibssh_a-B9@ssh.example.test"]
        );
        let mut ip = t.clone();
        ip.host = "152.236.1.51".into();
        assert_eq!(login_args(&ip).unwrap()[2], "ibssh_a-B9@152.236.1.51");
    }

    #[test]
    fn a_terminal_login_runs_the_remote_command_with_a_terminal() {
        let t = tok("ibssh_a-B9", Duration::days(1));
        let args = terminal_args(&t, "herdr").unwrap();
        assert_eq!(
            args,
            [
                "-t",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-p",
                "2222",
                "ibssh_a-B9@ssh.example.test",
                "herdr"
            ]
        );
        assert_eq!(
            command_file_content("/usr/bin/ssh", &args),
            "#!/bin/sh\nexec /usr/bin/ssh -t -o StrictHostKeyChecking=accept-new -p 2222 \
             ibssh_a-B9@ssh.example.test herdr\n"
        );
    }

    #[test]
    fn a_hostile_host_or_token_makes_no_login() {
        for bad in HOSTILE {
            let mut host = tok("ibssh_ok", Duration::days(1));
            host.host = bad.to_string();
            let token = tok(bad, Duration::days(1));
            for t in [&host, &token] {
                assert_eq!(login_args(t), Err(NOT_PLAIN.to_string()), "{bad:?}");
                assert_eq!(
                    terminal_args(t, "herdr"),
                    Err(NOT_PLAIN.to_string()),
                    "{bad:?}"
                );
            }
            let t = tok("ibssh_ok", Duration::days(1));
            assert_eq!(
                terminal_args(&t, bad),
                Err(NOT_PLAIN.to_string()),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_terminal_checks_every_argument_and_the_program_itself() {
        let ok: Vec<String> = ["-p", "2222", "ibssh_a-B9@ssh.example.test"]
            .map(String::from)
            .to_vec();
        assert!(check_login("ssh", &ok, plain_unix_program).is_ok());
        assert!(check_login("/usr/bin/ssh", &ok, plain_unix_program).is_ok());
        let windows = r"C:\Windows\System32\OpenSSH\ssh.exe";
        assert!(check_login(windows, &ok, plain_windows_program).is_ok());
        let git = r"C:\Program Files (x86)\Git\usr\bin\ssh.exe";
        assert!(check_login(git, &ok, plain_windows_program).is_ok());
        // An argument that slipped past the login's own check is refused here too.
        for bad in HOSTILE {
            let mut args = ok.clone();
            args.push(bad.to_string());
            if bad.starts_with('-') {
                continue; // a plain option, which ssh itself takes
            }
            for program in [
                plain_unix_program as fn(&str) -> bool,
                plain_windows_program,
            ] {
                assert_eq!(
                    check_login("ssh", &args, program),
                    Err(NOT_PLAIN.to_string()),
                    "{bad:?}"
                );
            }
        }
        for bad in [
            "/usr/bin/ssh;calc",
            "/tmp/x y/ssh",
            "$(calc)",
            "`calc`",
            "a\nb",
            "",
        ] {
            assert!(
                check_login(bad, &ok, plain_unix_program).is_err(),
                "{bad:?}"
            );
        }
        for bad in [
            r#"C:\x"&calc&"\ssh.exe"#,
            r"C:\%PATH%\ssh.exe",
            "C:\\a;b\\ssh.exe",
            "a\nb",
            "",
        ] {
            assert!(
                check_login(bad, &ok, plain_windows_program).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_console_window_runs_the_program_it_was_given_in_quotes() {
        let args: Vec<String> = ["-t", "-p", "2222", "ibssh_x@ssh.example.test", "herdr"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            cmd_line(r"C:\Program Files\OpenSSH\ssh.exe", &args),
            r#"/k ""C:\Program Files\OpenSSH\ssh.exe" -t -p 2222 ibssh_x@ssh.example.test herdr""#
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
        store.save("m1", Kind::Ordinary, &stored).unwrap();
        let got = token(
            &client(&server),
            &store,
            "m1",
            Kind::Ordinary,
            &crate::gate::Open,
        )
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
            .save("m1", Kind::Ordinary, &tok("ibssh_old", Duration::hours(2)))
            .unwrap();
        let got = token(
            &client(&server),
            &store,
            "m1",
            Kind::Ordinary,
            &crate::gate::Open,
        )
        .await
        .unwrap();
        assert_eq!(got.token, "ibssh_new");
        let stored = store.load("m1", Kind::Ordinary).unwrap().unwrap();
        assert_eq!(stored.token, "ibssh_new");
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
        for kind in [Kind::Ordinary, Kind::Background] {
            let err = token(&client(&server), &store, "m3", kind, &Closed)
                .await
                .unwrap_err();
            assert_eq!(err.code, "account_changed");
            assert!(store.load("m3", kind).unwrap().is_none());
        }
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
        let got = token(
            &client(&server),
            &store,
            "m2",
            Kind::Ordinary,
            &crate::gate::Open,
        )
        .await
        .unwrap();
        assert_eq!(got.token, "ibssh_new");
        assert!(store.load("m2", Kind::Ordinary).unwrap().is_some());
        assert!(store.load("m2", Kind::Background).unwrap().is_none());
    }

    /// The reply of a server that knows background tokens, for a background `t`.
    fn background_body_for(t: &SshToken) -> serde_json::Value {
        let mut body = body_for(t);
        body["background"] = true.into();
        body["backgroundActive"] = false.into();
        body
    }

    #[tokio::test]
    async fn a_background_token_is_asked_for_with_background_and_kept_apart() {
        let server = MockServer::start().await;
        let bg = tok("ibssh_bg", Duration::days(30));
        let plain = tok("ibssh_plain", Duration::days(30));
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ssh-tokens"))
            .and(body_json(serde_json::json!(
                {"name": "Instabox", "ttlSeconds": 2592000, "background": true}
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(background_body_for(&bg)))
            .expect(1)
            .mount(&server)
            .await;
        // The ordinary token's body is the one older releases sent, with no background.
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ssh-tokens"))
            .and(body_json(
                serde_json::json!({"name": "Instabox", "ttlSeconds": 2592000}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(body_for(&plain)))
            .expect(1)
            .mount(&server)
            .await;
        let store = MemorySshStore::default();
        let c = client(&server);
        // Each kind twice: the second of each is the stored one.
        for _ in 0..2 {
            let got = token(&c, &store, "m1", Kind::Background, &crate::gate::Open)
                .await
                .unwrap();
            assert_eq!((got.token.as_str(), got.background), ("ibssh_bg", true));
            assert!(!ordinary_in_place_of_background(Kind::Background, &got));
            let got = token(&c, &store, "m1", Kind::Ordinary, &crate::gate::Open)
                .await
                .unwrap();
            assert_eq!((got.token.as_str(), got.background), ("ibssh_plain", false));
        }
        let stored = |kind| store.load("m1", kind).unwrap().unwrap().token;
        assert_eq!(stored(Kind::Background), "ibssh_bg");
        assert_eq!(stored(Kind::Ordinary), "ibssh_plain");
    }

    #[tokio::test]
    async fn a_server_without_background_tokens_makes_an_ordinary_one_that_is_used_in_its_place() {
        let server = MockServer::start().await;
        let fresh = tok("ibssh_new", Duration::days(30));
        // An older server ignores `background` and answers without it.
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ssh-tokens"))
            .and(body_json(serde_json::json!(
                {"name": "Instabox", "ttlSeconds": 2592000, "background": true}
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(body_for(&fresh)))
            .expect(1)
            .mount(&server)
            .await;
        let store = MemorySshStore::default();
        let c = client(&server);
        let got = token(&c, &store, "m1", Kind::Background, &crate::gate::Open)
            .await
            .unwrap();
        assert_eq!((got.token.as_str(), got.background), ("ibssh_new", false));
        assert!(ordinary_in_place_of_background(Kind::Background, &got));
        assert!(!ordinary_in_place_of_background(Kind::Ordinary, &got));
        // It is kept as the background token and reused by the same rule, not asked for again.
        let again = token(&c, &store, "m1", Kind::Background, &crate::gate::Open)
            .await
            .unwrap();
        assert_eq!(again, got);
        assert_eq!(store.load("m1", Kind::Ordinary).unwrap(), None);
    }

    #[tokio::test]
    async fn the_token_an_older_release_stored_stays_the_ordinary_one() {
        let server = MockServer::start().await;
        let bg = tok("ibssh_bg", Duration::days(30));
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ssh-tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(background_body_for(&bg)))
            .expect(1)
            .mount(&server)
            .await;
        // An older release kept its one token in the ordinary entry, as JSON without
        // `background`.
        let old: SshToken =
            serde_json::from_value(body_for(&tok("ibssh_old", Duration::days(20)))).unwrap();
        assert!(!old.background);
        let store = MemorySshStore::default();
        store.save("m1", Kind::Ordinary, &old).unwrap();
        let c = client(&server);
        let got = token(&c, &store, "m1", Kind::Background, &crate::gate::Open)
            .await
            .unwrap();
        assert_eq!(
            got.token, "ibssh_bg",
            "a new background token, not the old one"
        );
        let got = token(&c, &store, "m1", Kind::Ordinary, &crate::gate::Open)
            .await
            .unwrap();
        assert_eq!(got, old, "the old token stays the ordinary one");
    }
}
