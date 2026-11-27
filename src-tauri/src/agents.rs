// Parts of this file (agent sign-in on a sandbox: the vendor login runs on the sandbox and its
// localhost callback port is forwarded over SSH) are adapted from t3boat,
// https://github.com/paukraft/t3boat, under the MIT License:
//
// MIT License
//
// Copyright (c) 2026 Pau Kraft
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
//
//! Signing Claude Code or Codex in on a sandbox with the vendor's own browser login. The login
//! runs on the sandbox and its localhost callback port is forwarded to this computer over SSH,
//! so the browser's redirect lands on the sandbox. The credential is created on the sandbox
//! and stays there. This computer's own logins are never touched.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::api::{Client, ExecRequest, ExecResult};
use crate::gate::{Gate, guarded};
use crate::sandbox::ensure_running;
use crate::ssh::{self, SshTokenStore};

const EXEC_TIMEOUT_SECS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    fn login(self) -> &'static str {
        match self {
            Agent::Claude => "claude auth login",
            Agent::Codex => "codex login",
        }
    }

    /// Exits 0 once signed in.
    fn status(self) -> &'static str {
        match self {
            Agent::Claude => "claude auth status >/dev/null 2>&1",
            Agent::Codex => "codex login status >/dev/null 2>&1",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
        }
    }
}

/// Waits and limits of the flow. Short under test so flow tests stay fast.
#[derive(Debug, Clone, Copy)]
struct Timing {
    url_poll: Duration,
    url_timeout: Duration,
    status_poll: Duration,
    login_timeout: Duration,
}

const TIMING: Timing = if cfg!(test) {
    Timing {
        url_poll: Duration::from_millis(10),
        url_timeout: Duration::from_millis(500),
        status_poll: Duration::from_millis(10),
        login_timeout: Duration::from_millis(500),
    }
} else {
    // Every poll is an API exec, so the URL poll is 1 s, not faster.
    Timing {
        url_poll: Duration::from_secs(1),
        url_timeout: Duration::from_secs(30),
        status_poll: Duration::from_secs(3),
        login_timeout: Duration::from_secs(600),
    }
};

/// A running local port forward. Dropping it ends the forward.
pub trait Forward: Send {
    /// The exit code once the forward has ended on its own (-1 when killed by a signal).
    fn exited(&mut self) -> Option<i32>;
}

impl Forward for tokio::process::Child {
    fn exited(&mut self) -> Option<i32> {
        match self.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            Ok(None) => None,
            Err(_) => Some(-1),
        }
    }
}

/// Starts the local port forward.
pub trait ForwardSpawner: Send + Sync {
    fn spawn(&self, port: u16, args: &[String]) -> std::io::Result<Box<dyn Forward>>;
}

/// Runs the local `ssh` binary. The child is killed when the guard drops.
pub struct SshSpawner;

impl ForwardSpawner for SshSpawner {
    fn spawn(&self, _port: u16, args: &[String]) -> std::io::Result<Box<dyn Forward>> {
        let child = tokio::process::Command::from(crate::process::command("ssh"))
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        Ok(Box::new(child))
    }
}

/// The shell command that starts the login detached and prints its temp dir as the last line.
/// The CLI hands the sign-in URL to `$BROWSER`; the script there records it in `$d/url`.
/// `setsid -f` keeps the login alive after the exec returns.
fn start_command(agent: Agent) -> String {
    format!(
        "d=$(mktemp -d); printf '#!/bin/sh\\necho \"$1\" > %s/url\\n' \"$d\" > \"$d/browser\"; \
         chmod +x \"$d/browser\"; \
         BROWSER=\"$d/browser\" setsid -f nohup script -qfec '{}' \"$d/log\" >/dev/null 2>&1; \
         echo \"$d\"",
        agent.login()
    )
}

async fn exec(client: &Client, id: &str, cmd: String) -> Result<ExecResult> {
    let req = ExecRequest {
        cmd,
        timeout_seconds: Some(EXEC_TIMEOUT_SECS),
    };
    Ok(client.exec(id, &req).await?)
}

/// True when the agent's own status command exits 0. Runs one exec; do not call on a timer.
pub async fn signed_in(client: &Client, id: &str, agent: Agent) -> bool {
    exec(client, id, agent.status().to_string())
        .await
        .is_ok_and(|r| r.exit_code == 0)
}

/// Signs `agent` in on the sandbox: the user finishes the login in this computer's browser.
pub async fn sign_in(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    agent: Agent,
    open_url: impl Fn(&str) -> Result<()>,
    gate: &dyn Gate,
) -> Result<()> {
    sign_in_with(
        client,
        store,
        id,
        agent,
        open_url,
        gate,
        &SshSpawner,
        TIMING,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn sign_in_with(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    agent: Agent,
    open_url: impl Fn(&str) -> Result<()>,
    gate: &dyn Gate,
    spawner: &dyn ForwardSpawner,
    timing: Timing,
) -> Result<()> {
    ensure_running(client, id).await?;
    let out = exec(client, id, start_command(agent)).await?;
    let dir = out
        .stdout
        .lines()
        .last()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if dir.is_empty() {
        bail!("The {} login did not start on the sandbox", agent.label());
    }
    let result = finish(
        client, store, id, agent, &open_url, gate, spawner, timing, &dir,
    )
    .await;
    // Best effort: drop the temp dir and stop a login nobody finished. The bracket keeps
    // pkill from matching this command's own shell.
    let login = agent.login();
    let stop = format!(
        "rm -rf '{dir}'; pkill -f '[{}]{}'",
        &login[..1],
        &login[1..]
    );
    let _ = exec(client, id, stop).await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn finish(
    client: &Client,
    store: &dyn SshTokenStore,
    id: &str,
    agent: Agent,
    open_url: &impl Fn(&str) -> Result<()>,
    gate: &dyn Gate,
    spawner: &dyn ForwardSpawner,
    timing: Timing,
    dir: &str,
) -> Result<()> {
    let started = Instant::now();
    let url = loop {
        let out = exec(client, id, format!("cat '{dir}/url' 2>/dev/null")).await?;
        if let Some(url) = out
            .stdout
            .split_whitespace()
            .find(|w| w.starts_with("https://"))
        {
            break url.to_owned();
        }
        if started.elapsed() > timing.url_timeout {
            bail!("The {} login did not start on the sandbox", agent.label());
        }
        tokio::time::sleep(timing.url_poll).await;
    };
    let port = callback_port(&url).ok_or_else(|| anyhow!("This login has no local callback"))?;
    // The ordinary token: the forward is a login the user started, and it ends with it.
    let token = ssh::token(client, store, id, ssh::Kind::Ordinary, gate).await?;
    // Dropped on every exit from here on, which ends the forward.
    let mut forward = guarded(gate, || {
        spawner.spawn(port, &ssh::forward_args(&token, port))
    })
    .ok_or_else(account_changed)?
    .with_context(|| format!("Could not forward port {port} for the login"))?;
    guarded(gate, || open_url(&url)).ok_or_else(account_changed)??;
    let waiting = Instant::now();
    loop {
        tokio::time::sleep(timing.status_poll).await;
        if exec(client, id, agent.status().to_string())
            .await?
            .exit_code
            == 0
        {
            return Ok(());
        }
        // ssh exits at once when the port is busy or the login is refused.
        if forward.exited().is_some() {
            bail!("Could not forward port {port} to the sandbox. Is another app using it?");
        }
        if waiting.elapsed() > timing.login_timeout {
            bail!("Sign-in timed out");
        }
    }
}

fn account_changed() -> anyhow::Error {
    anyhow!("The account changed. Nothing was done.")
}

/// The port of a `redirect_uri=http://localhost:<port>/...` (or 127.0.0.1) parameter.
pub fn callback_port(url: &str) -> Option<u16> {
    let url = url::Url::parse(url).ok()?;
    let redirect = url.query_pairs().find(|(k, _)| k == "redirect_uri")?.1;
    let redirect = url::Url::parse(&redirect).ok()?;
    matches!(redirect.host_str()?, "localhost" | "127.0.0.1").then_some(())?;
    redirect.port()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Credentials;
    use crate::ssh::MemorySshStore;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const CLAUDE_URL: &str = "https://claude.com/cai/oauth/authorize?code=true&redirect_uri=http%3A%2F%2Flocalhost%3A43327%2Fcallback&state=x";

    #[test]
    fn finds_the_local_callback_port() {
        let codex = "https://auth.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback";
        let manual = "https://claude.com/cai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback";
        assert_eq!(callback_port(CLAUDE_URL), Some(43327));
        assert_eq!(callback_port(codex), Some(1455));
        assert_eq!(callback_port(manual), None);
    }

    #[test]
    fn start_command_mentions_the_pieces() {
        let cmd = start_command(Agent::Claude);
        assert!(cmd.contains("BROWSER="));
        assert!(cmd.contains("setsid -f"));
        assert!(cmd.contains("'claude auth login'"));
        assert!(cmd.trim_end().ends_with("echo \"$d\""));
        assert!(start_command(Agent::Codex).contains("'codex login'"));
    }

    /// Runs the real command with stub `setsid`, `script` and `claude` first on PATH.
    #[cfg(unix)]
    #[test]
    fn start_command_runs_and_records_the_url() {
        use std::os::unix::fs::PermissionsExt;
        let url = "https://example.test/x?redirect_uri=http%3A%2F%2Flocalhost%3A43327%2Fcb";
        let stubs = tempfile::tempdir().unwrap();
        let write = |name: &str, body: &str| {
            let f = stubs.path().join(name);
            std::fs::write(&f, body).unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write(
            "setsid",
            "#!/bin/sh\n[ \"$1\" = \"-f\" ] && shift\n\"$@\" &\n",
        );
        write("script", "#!/bin/sh\nexec sh -c \"$2\"\n");
        write("claude", &format!("#!/bin/sh\n\"$BROWSER\" '{url}'\n"));
        let path = format!(
            "{}:{}",
            stubs.path().display(),
            std::env::var("PATH").unwrap()
        );
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(start_command(Agent::Claude))
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        let dir = std::path::PathBuf::from(stdout.lines().last().unwrap().trim());
        assert!(dir.is_dir());
        let mode = std::fs::metadata(dir.join("browser"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "browser is not executable");
        // The login runs detached, so wait briefly for it to call the browser script.
        let mut got = String::new();
        for _ in 0..100 {
            got = std::fs::read_to_string(dir.join("url")).unwrap_or_default();
            if !got.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got.trim(), url);
    }

    #[test]
    fn agent_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Agent::Claude).unwrap(), "\"claude\"");
        assert_eq!(serde_json::to_string(&Agent::Codex).unwrap(), "\"codex\"");
    }

    struct Creds;

    #[async_trait]
    impl Credentials for Creds {
        async fn bearer(&self) -> crate::api::Result<String> {
            Ok("iba_x".into())
        }
        async fn refreshed(&self) -> crate::api::Result<String> {
            Ok("iba_new".into())
        }
    }

    fn client(server: &MockServer) -> Client {
        Client::new(Url::parse(&server.uri()).unwrap(), Arc::new(Creds))
    }

    #[derive(Default)]
    struct FakeSpawner {
        ports: Mutex<Vec<u16>>,
        args: Mutex<Vec<String>>,
        dropped: Arc<AtomicUsize>,
        dies: bool,
        fails: bool,
        log: Log,
    }

    type Log = Arc<Mutex<Vec<String>>>;

    struct Guard {
        dropped: Arc<AtomicUsize>,
        dies: bool,
    }
    impl Forward for Guard {
        fn exited(&mut self) -> Option<i32> {
            self.dies.then_some(255)
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl ForwardSpawner for FakeSpawner {
        fn spawn(&self, port: u16, args: &[String]) -> std::io::Result<Box<dyn Forward>> {
            self.log.lock().unwrap().push("SPAWN".into());
            if self.fails {
                return Err(std::io::Error::other("no ssh"));
            }
            self.ports.lock().unwrap().push(port);
            *self.args.lock().unwrap() = args.to_vec();
            Ok(Box::new(Guard {
                dropped: self.dropped.clone(),
                dies: self.dies,
            }))
        }
    }

    fn running_machine() -> serde_json::Value {
        serde_json::json!({
            "id": "m1", "name": "n", "state": "running", "revision": 1,
            "template": {"id": "t", "version": 1, "digest": "d"},
            "spec": {"vcpus": 2, "memMiB": 4096, "persistentDiskGiB": 10},
            "runtime": {"ready": true},
            "autoSuspendAfterSeconds": null, "maxRunMinutes": -1,
            "runStartedAt": null, "cooldownUntil": null,
            "metadata": {}, "orgId": "o1", "free": false, "operationId": null
        })
    }

    fn exec_out(stdout: &str, code: i32) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!(
            {"stdout": stdout, "stderr": "", "exitCode": code, "timedOut": false}))
    }

    async fn mount_common(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/v1/machines/m1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(running_machine()))
            .mount(server)
            .await;
        // Only the ordinary token's body: the login's forward never asks for a background one.
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ssh-tokens"))
            .and(body_json(
                serde_json::json!({"name": "Instabox", "ttlSeconds": 2592000}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!(
                {"id":"i","name":"n","host":"ssh.example.test","port":2222,"token":"ibssh_x",
                "command":"ssh","expiresAt":"2099-01-01T00:00:00Z"})))
            .mount(server)
            .await;
    }

    /// Answers each exec by its command: start, `cat url`, status, or cleanup.
    fn exec_responder(
        url: &'static str,
        url_after: usize,
        status_fail: usize,
        cmds: Arc<Mutex<Vec<String>>>,
    ) -> impl Fn(&Request) -> ResponseTemplate + Send + Sync + 'static {
        let cats = AtomicUsize::new(0);
        let statuses = AtomicUsize::new(0);
        move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let cmd = body["cmd"].as_str().unwrap().to_string();
            cmds.lock().unwrap().push(cmd.clone());
            if cmd.starts_with("d=$(mktemp") {
                exec_out("noise\n/tmp/tmp.abc\n", 0)
            } else if cmd.starts_with("cat '/tmp/tmp.abc/url'") {
                let n = cats.fetch_add(1, Ordering::SeqCst) + 1;
                exec_out(if n >= url_after { url } else { "" }, 0)
            } else if cmd == "claude auth status >/dev/null 2>&1" {
                let n = statuses.fetch_add(1, Ordering::SeqCst);
                exec_out("", if n < status_fail { 1 } else { 0 })
            } else {
                exec_out("", 0)
            }
        }
    }

    /// The log as short tags, in order.
    fn tags(log: &Log) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .map(|c| {
                if c.starts_with("d=$(mktemp") {
                    "start"
                } else if c.starts_with("cat ") {
                    "cat"
                } else if c.contains("auth status") {
                    "status"
                } else if c.starts_with("rm -rf") {
                    "cleanup"
                } else {
                    c.as_str()
                }
                .to_string()
            })
            .collect()
    }

    struct Setup {
        url: &'static str,
        url_after: usize,
        status_fail: usize,
        open_fails: bool,
    }

    impl Default for Setup {
        fn default() -> Self {
            Setup {
                url: CLAUDE_URL,
                url_after: 1,
                status_fail: 0,
                open_fails: false,
            }
        }
    }

    /// Runs the flow against a mock sandbox. Events (execs, SPAWN, OPEN) land in `log`.
    async fn flow(setup: Setup, spawner: &FakeSpawner, log: &Log) -> Result<()> {
        flow_gated(setup, spawner, log, &crate::gate::Open).await
    }

    async fn flow_gated(
        setup: Setup,
        spawner: &FakeSpawner,
        log: &Log,
        gate: &dyn Gate,
    ) -> Result<()> {
        let server = MockServer::start().await;
        mount_common(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .respond_with(exec_responder(
                setup.url,
                setup.url_after,
                setup.status_fail,
                log.clone(),
            ))
            .mount(&server)
            .await;
        sign_in_with(
            &client(&server),
            &MemorySshStore::default(),
            "m1",
            Agent::Claude,
            |u| {
                log.lock().unwrap().push(format!("OPEN {u}"));
                if setup.open_fails {
                    bail!("no browser");
                }
                Ok(())
            },
            gate,
            spawner,
            TIMING,
        )
        .await
    }

    fn fake(log: &Log) -> FakeSpawner {
        FakeSpawner {
            log: log.clone(),
            ..Default::default()
        }
    }

    struct ClosedGate;
    impl Gate for ClosedGate {
        fn run(&self, _: &mut dyn FnMut()) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn a_closed_gate_opens_no_browser_and_forwards_no_port() {
        let log = Log::default();
        let spawner = fake(&log);
        let err = flow_gated(Setup::default(), &spawner, &log, &ClosedGate)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("account changed"), "{err}");
        assert_eq!(opens(&tags(&log)), 0, "no browser");
        assert!(spawner.ports.lock().unwrap().is_empty(), "no forward");
    }

    fn opens(t: &[String]) -> usize {
        t.iter().filter(|x| x.starts_with("OPEN ")).count()
    }

    fn count(t: &[String], tag: &str) -> usize {
        t.iter().filter(|x| *x == tag).count()
    }

    #[tokio::test]
    async fn signs_in_through_a_forwarded_callback_port() {
        let log = Log::default();
        let spawner = fake(&log);
        flow(
            Setup {
                url_after: 2,
                status_fail: 1,
                ..Default::default()
            },
            &spawner,
            &log,
        )
        .await
        .unwrap();
        assert_eq!(*spawner.ports.lock().unwrap(), [43327]);
        assert!(
            spawner
                .args
                .lock()
                .unwrap()
                .contains(&"43327:127.0.0.1:43327".to_string())
        );
        assert_eq!(spawner.dropped.load(Ordering::SeqCst), 1);
        // Forward first, then open the browser, and only then ask for the status.
        assert_eq!(
            tags(&log),
            [
                "start".to_string(),
                "cat".into(),
                "cat".into(),
                "SPAWN".into(),
                format!("OPEN {CLAUDE_URL}"),
                "status".into(),
                "status".into(),
                "cleanup".into(),
            ]
        );
        assert!(
            log.lock()
                .unwrap()
                .last()
                .unwrap()
                .starts_with("rm -rf '/tmp/tmp.abc'")
        );
    }

    #[tokio::test]
    async fn times_out_and_ends_the_forward() {
        let log = Log::default();
        let spawner = fake(&log);
        let err = flow(
            Setup {
                status_fail: usize::MAX,
                ..Default::default()
            },
            &spawner,
            &log,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Sign-in timed out");
        assert_eq!(spawner.dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_dead_forward_stops_the_wait() {
        let log = Log::default();
        let spawner = FakeSpawner {
            dies: true,
            ..fake(&log)
        };
        let err = flow(
            Setup {
                status_fail: usize::MAX,
                ..Default::default()
            },
            &spawner,
            &log,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Could not forward port 43327 to the sandbox. Is another app using it?"
        );
        let t = tags(&log);
        assert_eq!(opens(&t), 1);
        assert_eq!(count(&t, "status"), 1);
        assert_eq!(spawner.dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gives_up_when_the_login_never_prints_a_url() {
        let log = Log::default();
        let spawner = fake(&log);
        let err = flow(
            Setup {
                url_after: usize::MAX,
                ..Default::default()
            },
            &spawner,
            &log,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "The Claude Code login did not start on the sandbox"
        );
        let t = tags(&log);
        assert_eq!(count(&t, "SPAWN"), 0);
        assert!(count(&t, "cat") > 1);
        // The temp dir is still cleaned up.
        assert_eq!(t.last().unwrap(), "cleanup");
    }

    #[tokio::test]
    async fn refuses_a_login_without_a_local_callback() {
        let log = Log::default();
        let spawner = fake(&log);
        let manual = "https://claude.com/cai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback";
        let err = flow(
            Setup {
                url: manual,
                ..Default::default()
            },
            &spawner,
            &log,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "This login has no local callback");
        let t = tags(&log);
        assert_eq!(count(&t, "SPAWN"), 0);
        assert_eq!(opens(&t), 0);
    }

    #[tokio::test]
    async fn a_failed_open_ends_the_forward_without_a_status_check() {
        let log = Log::default();
        let spawner = fake(&log);
        let err = flow(
            Setup {
                open_fails: true,
                ..Default::default()
            },
            &spawner,
            &log,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "no browser");
        assert_eq!(spawner.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(count(&tags(&log), "status"), 0);
    }

    #[tokio::test]
    async fn a_failed_spawn_stops_before_the_browser_opens() {
        let log = Log::default();
        let spawner = FakeSpawner {
            fails: true,
            ..fake(&log)
        };
        let err = flow(Setup::default(), &spawner, &log).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Could not forward port 43327 for the login"
        );
        let t = tags(&log);
        assert_eq!(opens(&t), 0);
        assert_eq!(count(&t, "status"), 0);
        assert_eq!(spawner.dropped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn signed_in_reads_the_status_exit_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .respond_with(exec_responder(
                CLAUDE_URL,
                1,
                1,
                Arc::new(Mutex::new(Vec::new())),
            ))
            .mount(&server)
            .await;
        let c = client(&server);
        assert!(!signed_in(&c, "m1", Agent::Claude).await);
        assert!(signed_in(&c, "m1", Agent::Claude).await);
    }
}
