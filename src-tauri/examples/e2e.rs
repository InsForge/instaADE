//! Live end-to-end run against the instabox API.
//!
//! Creates one Standard sandbox (default name `bridge-e2e-1`), connects it,
//! checks the T3 setup and every sandbox action, then deletes every sandbox of the
//! key's own org whose name starts with `bridge-e2e-`, also when a step fails.
//!
//! Needs `INSTABOX_API_KEY` (an `ibk_` key, read from the environment only) and
//! T3 Code on this computer. `INSTABOX_API_URL` overrides the API base.
//!
//!     cargo run --example e2e [-- bridge-e2e-<n>]

use std::fmt::Display;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use instabox_bridge_lib::ade::Ade;
use instabox_bridge_lib::api::{Client, ExecRequest, Machine, MachineSpec};
use instabox_bridge_lib::app::api_base;
use instabox_bridge_lib::auth::{MemoryStore, Session};
use instabox_bridge_lib::sandbox::connect::{self, NewSandbox, Source, Step};
use instabox_bridge_lib::sandbox::lifecycle;
use instabox_bridge_lib::sandbox::sync::Decision;
use instabox_bridge_lib::sandbox::view::view;
use instabox_bridge_lib::ssh::{self, MemorySshStore};

const PREFIX: &str = "bridge-e2e-";
const ADE: Ade = Ade::T3;
const DEFAULT_NAME: &str = "bridge-e2e-1";
const START: MachineSpec = MachineSpec {
    vcpus: 2,
    mem_mib: 4096,
};
const RESIZED: MachineSpec = MachineSpec {
    vcpus: 4,
    mem_mib: 8192,
};
const START_SLEEP: i64 = 600;
const NEW_SLEEP: i64 = 900;
const SSH_TIMEOUT: Duration = Duration::from_secs(60);
const CLEANUP_WAIT: Duration = Duration::from_secs(300);

#[derive(Default)]
struct Checks {
    pass: AtomicU32,
    fail: AtomicU32,
}

impl Checks {
    fn record(&self, name: &str, ok: bool, took: Duration, detail: impl Display) {
        let counter = if ok { &self.pass } else { &self.fail };
        counter.fetch_add(1, Ordering::SeqCst);
        let word = if ok { "PASS" } else { "FAIL" };
        println!("{word} {name} ({:.1}s) {detail}", took.as_secs_f64());
    }

    fn check(&self, name: &str, ok: bool, t: Instant, detail: impl Display) {
        self.record(name, ok, t.elapsed(), detail);
    }

    /// Records a FAIL for an error and turns it into the run's stop reason.
    fn must<T, E: Display>(&self, name: &str, t: Instant, r: Result<T, E>) -> Result<T, String> {
        r.map_err(|e| {
            self.record(name, false, t.elapsed(), &e);
            format!("{name}: {e}")
        })
    }
}

fn not_checked(name: &str, why: &str) {
    println!("NOT CHECKED {name} ({why})");
}

async fn sh(client: &Client, id: &str, cmd: &str) -> Result<(i32, String), String> {
    let r = client
        .exec(
            id,
            &ExecRequest {
                cmd: cmd.to_string(),
                timeout_seconds: Some(60),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok((r.exit_code, r.stdout.trim().to_string()))
}

#[tokio::main]
async fn main() {
    let Some(key) = std::env::var("INSTABOX_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
    else {
        eprintln!("INSTABOX_API_KEY is not set");
        std::process::exit(2);
    };
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_NAME.to_string());
    if !name.starts_with(PREFIX) {
        eprintln!("the sandbox name must start with {PREFIX}");
        std::process::exit(2);
    }
    let base = api_base(std::env::var("INSTABOX_API_URL").ok());
    // An API key session never touches the keychain; the store stays empty.
    let session = Session::load_from(&base, Some(key), Arc::new(MemoryStore::default()))
        .expect("an API key always gives a session");
    let client = Client::new(base, Arc::new(session));
    let checks = Arc::new(Checks::default());

    // Cleanup touches only the key's own org: the run creates its sandbox there, and a
    // teammate's sandbox in a shared org is never deleted, whatever its name.
    let org = match client.me().await {
        Ok(me) => match me.org {
            Some(o) => o.id,
            None => {
                eprintln!("the API key has no org");
                std::process::exit(2);
            }
        },
        Err(e) => {
            eprintln!("cannot read /v1/me: {e}");
            std::process::exit(2);
        }
    };

    if let Err(e) = remove_leftovers(&client, &org, "before the run").await {
        eprintln!("cannot list sandboxes: {e}");
        std::process::exit(2);
    }

    // Run the flow in a task so a panic still reaches the cleanup below.
    let flow = tokio::spawn(run(client.clone(), checks.clone(), name));
    match flow.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => println!("STOPPED {e}"),
        Err(e) => {
            checks.record("run", false, Duration::ZERO, format!("panicked: {e}"));
        }
    }
    not_checked(
        "T3 Code shows the pairing dialog",
        "needs a person at T3 Code",
    );
    not_checked(
        "probe answers busy during a T3 run",
        "needs an agent run, which needs Claude or Codex sign-in by a person",
    );
    not_checked(
        "server trusts the probe for idle suspend",
        "needs the instabox server to trust the probe on this template",
    );
    not_checked("SSH Open in Terminal", "opens Terminal for a person");

    let t = Instant::now();
    let left = remove_leftovers(&client, &org, "after the run").await;
    checks.check(
        "cleanup: no bridge-e2e-* sandbox left",
        left.as_ref().is_ok_and(|l| l.is_empty()),
        t,
        match &left {
            Ok(l) => format!("remaining {PREFIX}* sandboxes: {}", l.len()),
            Err(e) => format!("cannot list: {e}"),
        },
    );
    let (pass, fail) = (
        checks.pass.load(Ordering::SeqCst),
        checks.fail.load(Ordering::SeqCst),
    );
    println!("{pass} passed, {fail} failed");
    std::process::exit(if fail == 0 { 0 } else { 1 });
}

/// A sandbox this run may delete: named `bridge-e2e-*` and in the key's own org.
fn ours(m: &Machine, org: &str) -> bool {
    m.name.starts_with(PREFIX) && m.org_id == org
}

/// Deletes every `bridge-e2e-*` sandbox of `org` and waits until the list shows none.
/// Returns the names still listed at the end.
async fn remove_leftovers(client: &Client, org: &str, when: &str) -> Result<Vec<String>, String> {
    let deadline = Instant::now() + CLEANUP_WAIT;
    loop {
        let list = client.machines().await.map_err(|e| e.to_string())?;
        let mine: Vec<&Machine> = list.iter().filter(|m| ours(m, org)).collect();
        if mine.is_empty() || Instant::now() >= deadline {
            let names: Vec<String> = mine
                .iter()
                .map(|m| format!("{} ({})", m.name, m.state))
                .collect();
            if !names.is_empty() {
                println!("cleanup {when}: still listed: {}", names.join(", "));
            }
            return Ok(names);
        }
        for m in mine {
            if matches!(m.state.as_str(), "deleting" | "deleted") {
                continue;
            }
            println!("cleanup {when}: deleting {} ({})", m.name, m.state);
            if let Err(e) = lifecycle::delete(client, &m.id).await {
                println!("cleanup {when}: delete {} failed: {e}", m.name);
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Seconds between the first two recorded steps that match.
fn between(steps: &[(Step, Instant)], from: fn(&Step) -> bool, to: fn(&Step) -> bool) -> String {
    let a = steps.iter().find(|(s, _)| from(s)).map(|(_, t)| *t);
    let b = steps.iter().find(|(s, _)| to(s)).map(|(_, t)| *t);
    match (a, b) {
        (Some(a), Some(b)) => format!("{:.1}s", (b - a).as_secs_f64()),
        _ => "-".to_string(),
    }
}

async fn run(client: Client, c: Arc<Checks>, sandbox: String) -> Result<(), String> {
    // 1. Session, account and the desktop T3.
    let t = Instant::now();
    let name = "GET /v1/me and /v1/orgs with the API key";
    let me = c.must(name, t, client.me().await)?;
    let orgs = c.must(name, t, client.orgs().await)?;
    let tier = me.org.as_ref().map_or("-", |o| o.tier.as_str());
    let specs: Vec<String> = me
        .allowed_specs
        .iter()
        .map(|s| format!("{}/{}", s.vcpus, s.mem_mib))
        .collect();
    c.check(
        name,
        true,
        t,
        format!(
            "personal org tier {tier}, {} orgs, allowed specs [{}]",
            orgs.len(),
            specs.join(", ")
        ),
    );

    let t = Instant::now();
    let name = "desktop T3 version (ade::t3)";
    let cli = c.must(name, t, ADE.locate().ok_or("T3 CLI not found"))?;
    let desktop_v = c.must(name, t, ADE.local_version(&cli))?;
    c.check(name, true, t, format!("t3 {desktop_v}"));

    // 2. Create and connect.
    let steps: Mutex<Vec<(Step, Instant)>> = Mutex::new(Vec::new());
    let req = NewSandbox {
        name: sandbox.clone(),
        source: Source::Standard,
        spec: START,
        auto_suspend_after_seconds: Some(START_SLEEP),
        org_id: None,
    };
    let t = Instant::now();
    let label = format!("create_and_connect Standard {sandbox}");
    let name = label.as_str();
    let res = connect::create_and_connect(&client, ADE, &req, &desktop_v, |s| {
        steps.lock().unwrap().push((s, Instant::now()))
    })
    .await;
    let steps = steps.into_inner().unwrap();
    let timings = format!(
        "create {}, install {}, pair+mark {}",
        between(
            &steps,
            |s| matches!(s, Step::Creating),
            |s| matches!(s, Step::Installing(_) | Step::Pairing)
        ),
        between(
            &steps,
            |s| matches!(s, Step::Installing(_)),
            |s| matches!(s, Step::Pairing)
        ),
        between(
            &steps,
            |s| matches!(s, Step::Pairing),
            |s| matches!(s, Step::Done)
        ),
    );
    let connected = c.must(name, t, res.map_err(|e| format!("{e} [{timings}]")))?;
    let link_ok = connected
        .link
        .as_deref()
        .is_some_and(|l| l.starts_with("t3code://pair"));
    let version = connected.version.clone().unwrap_or_default();
    c.check(
        name,
        link_ok && version == desktop_v,
        t,
        format!("link starts with t3code://pair: {link_ok}; t3 {version}; {timings}"),
    );
    let id = connected.machine_id.clone();

    let t = Instant::now();
    let name = "machine after create (running, spec, auto sleep, mark)";
    let m = c.must(name, t, client.machine(&id).await)?;
    let marked = view(&m, chrono::Utc::now()).connected;
    c.check(
        name,
        m.state == "running"
            && m.spec == START
            && m.auto_suspend_after_seconds == Some(START_SLEEP)
            && marked,
        t,
        format!(
            "state {}, {}/{}, auto sleep {:?}, connected mark {marked}",
            m.state, m.spec.vcpus, m.spec.mem_mib, m.auto_suspend_after_seconds,
        ),
    );

    // 3. What the setup left on the sandbox.
    exec_checks(&client, &c, &id, &desktop_v, "after create").await;

    // 4. Pause and resume.
    let t = Instant::now();
    let name = "lifecycle::pause";
    c.must(name, t, lifecycle::pause(&client, &id).await)?;
    let took = t.elapsed();
    let m = c.must(name, t, client.machine(&id).await)?;
    c.record(
        name,
        m.state == "suspended",
        took,
        format!("state {}", m.state),
    );

    let t = Instant::now();
    let name = "lifecycle::resume + version check";
    let outcome = c.must(
        name,
        t,
        lifecycle::resume(&client, ADE, &id, &desktop_v).await,
    )?;
    let took = t.elapsed();
    let m = c.must(name, t, client.machine(&id).await)?;
    c.record(
        name,
        m.state == "running" && outcome.as_ref().map(|o| o.decision) == Some(Decision::UpToDate),
        took,
        format!(
            "state {}, decision {:?}, remote {:?}",
            m.state,
            outcome.as_ref().map(|o| o.decision),
            outcome.as_ref().and_then(|o| o.remote_version.clone())
        ),
    );

    // 5. Resize and auto sleep.
    let t = Instant::now();
    let name = "lifecycle::resize 4 vCPU / 8192 MiB";
    c.must(
        name,
        t,
        lifecycle::resize(&client, &id, RESIZED.vcpus, RESIZED.mem_mib).await,
    )?;
    let took = t.elapsed();
    let m = c.must(name, t, client.machine(&id).await)?;
    c.record(
        name,
        m.spec == RESIZED,
        took,
        format!(
            "spec {}/{}, state {}",
            m.spec.vcpus, m.spec.mem_mib, m.state
        ),
    );

    let t = Instant::now();
    let name = "T3 service active after the resize restart";
    let (_, out) = c.must(
        name,
        t,
        sh(&client, &id, "systemctl is-active t3-instabox").await,
    )?;
    c.check(name, out == "active", t, &out);

    let t = Instant::now();
    let name = "lifecycle::set_auto_sleep 900";
    c.must(
        name,
        t,
        lifecycle::set_auto_sleep(&client, &id, NEW_SLEEP).await,
    )?;
    let m = c.must(name, t, client.machine(&id).await)?;
    c.check(
        name,
        m.auto_suspend_after_seconds == Some(NEW_SLEEP),
        t,
        format!("auto sleep {:?}", m.auto_suspend_after_seconds),
    );

    // 6. Reinstall.
    let t = Instant::now();
    let name = "lifecycle::reinstall";
    c.must(
        name,
        t,
        lifecycle::reinstall(&client, ADE, &id, &desktop_v).await,
    )?;
    let took = t.elapsed();
    let (_, out) = c.must(
        name,
        t,
        sh(&client, &id, "systemctl is-active t3-instabox").await,
    )?;
    c.record(name, out == "active", took, format!("service {out}"));
    exec_checks(&client, &c, &id, &desktop_v, "after reinstall").await;

    // 7. SSH.
    ssh_check(&client, &c, &id).await;

    // 8. Delete.
    let t = Instant::now();
    let name = "lifecycle::delete";
    c.must(name, t, lifecycle::delete(&client, &id).await)?;
    let took = t.elapsed();
    let list = c.must(name, t, client.machines().await)?;
    let still = list.iter().find(|m| m.id == id);
    c.record(
        name,
        still.is_none(),
        took,
        match still {
            None => "gone from the list".to_string(),
            Some(m) => format!("still listed, state {}", m.state),
        },
    );
    Ok(())
}

async fn exec_checks(client: &Client, c: &Checks, id: &str, desktop_v: &str, when: &str) {
    let t = Instant::now();
    let name = format!("t3 --version = desktop ({when})");
    match sh(client, id, "t3 --version").await {
        Ok((code, out)) => {
            let v = ADE.parse_version(&out);
            c.check(
                &name,
                code == 0 && v.as_deref() == Some(desktop_v),
                t,
                format!("exit {code}, {out}"),
            );
        }
        Err(e) => c.check(&name, false, t, e),
    }

    let t = Instant::now();
    let probe = ADE.probe().expect("T3 has a probe").path();
    let name = format!("probe file mode 755 ({when})");
    match sh(client, id, &format!("stat -c '%a %U' {probe}")).await {
        Ok((code, out)) => c.check(
            &name,
            code == 0 && out.starts_with("755 "),
            t,
            format!("exit {code}, {out}"),
        ),
        Err(e) => c.check(&name, false, t, e),
    }

    let t = Instant::now();
    let name = format!("probe prints busy false ({when})");
    match sh(client, id, &probe).await {
        Ok((code, out)) => c.check(
            &name,
            code == 0 && out.contains("\"busy\": false"),
            t,
            format!("exit {code}, {out}"),
        ),
        Err(e) => c.check(&name, false, t, e),
    }

    let t = Instant::now();
    let name = format!("systemctl is-active t3-instabox ({when})");
    match sh(client, id, "systemctl is-active t3-instabox").await {
        Ok((_, out)) => c.check(&name, out == "active", t, &out),
        Err(e) => c.check(&name, false, t, e),
    }

    let t = Instant::now();
    let name = format!("T3 answers on 127.0.0.1:3774 ({when})");
    match sh(
        client,
        id,
        "curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:3774/",
    )
    .await
    {
        Ok((_, out)) => c.check(
            &name,
            !out.is_empty() && out != "000",
            t,
            format!("HTTP {out}"),
        ),
        Err(e) => c.check(&name, false, t, e),
    }
}

/// Gets an SSH token and runs the server's login command with `true` as the
/// remote command. Prints only the exit code: the command holds the token.
async fn ssh_check(client: &Client, c: &Checks, id: &str) {
    let t = Instant::now();
    let name = "ssh::token + server command runs `true`";
    let store = MemorySshStore::default();
    let (kind, open) = (ssh::Kind::Ordinary, &instabox_bridge_lib::gate::Open);
    let tok = match ssh::token(client, &store, id, kind, open).await {
        Ok(tok) => tok,
        Err(e) => return c.check(name, false, t, e),
    };
    let reused = ssh::token(client, &store, id, kind, open)
        .await
        .is_ok_and(|again| again.token == tok.token);
    let cmd = ssh::display_command(&tok);
    let mut words = cmd.split_whitespace();
    if words.next() != Some("ssh") {
        return c.check(name, false, t, "the server command does not start with ssh");
    }
    let known_hosts =
        std::env::temp_dir().join(format!("bridge-e2e-known-hosts-{}", std::process::id()));
    let mut args: Vec<String> = [
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ConnectTimeout=20",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push("-o".to_string());
    args.push(format!("UserKnownHostsFile={}", known_hosts.display()));
    args.extend(words.map(str::to_string));
    args.push("true".to_string());
    let run = tokio::process::Command::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(SSH_TIMEOUT, run).await;
    let _ = std::fs::remove_file(&known_hosts);
    match out {
        Ok(Ok(o)) => {
            let code = o.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&o.stderr).replace(&tok.token, "<token>");
            let last = stderr.lines().last().unwrap_or("").trim().to_string();
            c.check(
                name,
                code == 0,
                t,
                format!(
                    "exit {code}; stored token reused: {reused}; host key fingerprint given: {}{}",
                    tok.host_key_fingerprint.is_some(),
                    if code == 0 {
                        String::new()
                    } else {
                        format!("; stderr: {last}")
                    }
                ),
            );
        }
        Ok(Err(e)) => c.check(name, false, t, format!("cannot run ssh: {e}")),
        Err(_) => c.check(name, false, t, "ssh timed out"),
    }
}
