//! Create a sandbox (or take an existing one), set an ADE up on it and mark it connected.
//! A pairing ADE (T3) is installed, paired and its port published; a link ADE (cmux) gets its
//! prep command, and the app opens its link afterwards. An SSH host ADE (Orca) gets only the
//! mark, and the app writes its host afterwards. A host ADE (Paradev) is only checked: once
//! its host runs, the sandbox is marked and gets its link.

use crate::ade::{Ade, Host, HostState, Open, SETUP_EXIT_BUSY};
use crate::api::{
    Client, CreateMachineRequest, ExecRequest, ExecResult, MachineSpec, UpdateMachineRequest,
};
use crate::sandbox::sync::refresh;
use crate::sandbox::view::mark;
use crate::sandbox::{
    CREATE_WAIT, SandboxError, ensure_running, new_key, retry_transient, wait_op,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Standard,
    Snapshot(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSandbox {
    pub name: String,
    pub source: Source,
    pub spec: MachineSpec,
    pub auto_suspend_after_seconds: Option<i64>,
    pub org_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Creating,
    /// The machine id, right after create is accepted. Keep it to retry with `connect_existing`.
    Created(String),
    Waking,
    Installing(String),
    Pairing,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    pub machine_id: String,
    /// The pairing link to open. None for a link ADE: the app makes its link with an SSH
    /// token (`Open::Link`). For a host ADE (`Open::Host`), the link that opens the sandbox
    /// once its host runs; None while the user still has to set the host up in the app.
    pub link: Option<String>,
    /// The ADE version the sandbox runs: the desktop's after a setup, or its own (newer, or
    /// older with the update pending). None for an ADE that installs nothing there.
    pub version: Option<String>,
    /// The sandbox kept its older version because the ADE was busy: offer the update later.
    pub update_pending: bool,
}

const STDERR_LINES: usize = 20;
pub(crate) const SETUP_TIMEOUT_SECS: u32 = 600;
const PAIR_TIMEOUT_SECS: u32 = 60;
const PREP_TIMEOUT_SECS: u32 = 60;
const HOST_TIMEOUT_SECS: u32 = 30;

pub(crate) fn tail_lines(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n")
}

/// The `Setup` error of a failed exec, never empty: "Timed out after N s" when the exec
/// timed out, then the last lines of stderr; with neither, the exit code.
pub(crate) fn setup_failure(ade: Ade, r: &ExecResult, timeout_secs: u32) -> SandboxError {
    let mut lines = Vec::new();
    if r.timed_out {
        lines.push(format!("Timed out after {timeout_secs} s"));
    }
    let tail = tail_lines(&r.stderr);
    if !tail.trim().is_empty() {
        lines.push(tail);
    }
    if lines.is_empty() {
        lines.push(format!("The command exited with code {}", r.exit_code));
    }
    SandboxError::Setup {
        ade,
        stderr_tail: lines.join("\n"),
    }
}

/// Refuses a create on a free org that already holds its machine limit.
/// Reads the limit from the API; nothing is created.
async fn check_free_limit(client: &Client, org_id: Option<&str>) -> Result<(), SandboxError> {
    let (id, tier) = match org_id {
        Some(id) => {
            let orgs = client.orgs().await?;
            let org = orgs.into_iter().find(|o| o.id == id);
            (id.to_string(), org.map(|o| o.tier).unwrap_or_default())
        }
        None => match client.me().await?.org {
            Some(o) => (o.id, o.tier),
            None => return Ok(()),
        },
    };
    if tier != "free" {
        return Ok(());
    }
    let usage = client.usage(org_id).await?;
    let Some(max) = usage.limits.max_machines else {
        return Ok(());
    };
    let owned = client
        .machines()
        .await?
        .iter()
        .filter(|m| m.org_id == id)
        .count() as i64;
    if owned >= max {
        return Err(SandboxError::FreeLimit);
    }
    Ok(())
}

pub async fn create_and_connect(
    client: &Client,
    ade: Ade,
    req: &NewSandbox,
    desktop_version: &str,
    progress: impl Fn(Step),
) -> Result<Connected, SandboxError> {
    let id = create(client, req, &progress).await?;
    install_and_pair(client, ade, &id, desktop_version, &progress).await
}

/// Creates a sandbox with no ADE and waits until it runs. Returns its id.
pub async fn create(
    client: &Client,
    req: &NewSandbox,
    progress: &impl Fn(Step),
) -> Result<String, SandboxError> {
    check_free_limit(client, req.org_id.as_deref()).await?;
    progress(Step::Creating);
    let body = CreateMachineRequest {
        name: req.name.clone(),
        template_id: match req.source {
            Source::Standard => Some("agent-browser".to_string()),
            Source::Snapshot(_) => None,
        },
        snapshot_id: match &req.source {
            Source::Snapshot(id) => Some(id.clone()),
            Source::Standard => None,
        },
        vcpus: Some(req.spec.vcpus),
        mem_mib: Some(req.spec.mem_mib),
        metadata: None,
        auto_suspend_after_seconds: req.auto_suspend_after_seconds,
        org_id: req.org_id.clone(),
    };
    let key = new_key();
    let op = retry_transient(|| client.create(&body, &key)).await?;
    progress(Step::Created(op.machine_id.clone()));
    let machine = wait_op(client, &op, CREATE_WAIT).await?;
    Ok(machine.id)
}

pub async fn connect_existing(
    client: &Client,
    ade: Ade,
    machine_id: &str,
    desktop_version: &str,
    progress: impl Fn(Step),
) -> Result<Connected, SandboxError> {
    if client.machine(machine_id).await?.state != "running" {
        progress(Step::Waking);
    }
    ensure_running(client, machine_id).await?;
    install_and_pair(client, ade, machine_id, desktop_version, &progress).await
}

/// Writes `ade` into the sandbox's mark, next to any ADE already there.
async fn mark_connected(client: &Client, ade: Ade, id: &str) -> Result<(), SandboxError> {
    let mut metadata = client.machine(id).await?.metadata;
    mark(&mut metadata, ade);
    let update = UpdateMachineRequest {
        metadata: Some(metadata),
        ..Default::default()
    };
    let key = new_key();
    retry_transient(|| client.patch(id, &update, &key)).await?;
    Ok(())
}

async fn install_and_pair(
    client: &Client,
    ade: Ade,
    id: &str,
    desktop_version: &str,
    progress: &impl Fn(Step),
) -> Result<Connected, SandboxError> {
    if let Open::Link(l) = ade.open() {
        return prepare(client, ade, Some(l.prep), id, desktop_version, progress).await;
    }
    // An SSH host ADE (Orca) needs nothing on the sandbox: the app writes the host afterwards.
    if let Open::SshHost(_) = ade.open() {
        return prepare(client, ade, None, id, desktop_version, progress).await;
    }
    if let Open::Host(h) = ade.open() {
        return check_host(client, ade, h, id, progress).await;
    }
    // Never move a sandbox to an older version: when it is ahead of the desktop, only pair.
    let found = match ade.version_script() {
        Some(script) => {
            client
                .exec(
                    id,
                    &ExecRequest {
                        cmd: script.to_string(),
                        timeout_seconds: Some(30),
                    },
                )
                .await?
                .stdout
        }
        None => String::new(),
    };
    let remote = ade.parse_version(&found);
    let ahead = remote_is_newer(desktop_version, remote.as_deref());
    let mut update_pending = false;
    if !ahead {
        progress(Step::Installing(desktop_version.to_string()));
        let setup = client
            .exec(
                id,
                &ExecRequest {
                    cmd: ade.setup_script(desktop_version),
                    timeout_seconds: Some(SETUP_TIMEOUT_SECS),
                },
            )
            .await?;
        match setup.exit_code {
            _ if setup.timed_out => return Err(setup_failure(ade, &setup, SETUP_TIMEOUT_SECS)),
            0 => {}
            // The ADE runs and is busy, so its update waits. Pairing needs no restart: pair
            // with the running one and offer the update later.
            SETUP_EXIT_BUSY if remote.is_some() => update_pending = true,
            SETUP_EXIT_BUSY => return Err(SandboxError::AdeBusy(ade)),
            _ => return Err(setup_failure(ade, &setup, SETUP_TIMEOUT_SECS)),
        }
    } else {
        // A newer version stays, but gets this build's setup. When the ADE is busy, the next
        // sync (Open, resume) tries again; Update would refuse a sandbox that is ahead.
        refresh(client, ade, id, &found).await?;
    }
    progress(Step::Pairing);
    let link = pair_link(client, ade, id).await?;
    // The mark goes last: only a fully set up sandbox counts as connected.
    mark_connected(client, ade, id).await?;
    progress(Step::Done);
    Ok(Connected {
        machine_id: id.to_string(),
        link: Some(link),
        version: Some(match remote {
            Some(r) if ahead || update_pending => r,
            _ => desktop_version.to_string(),
        }),
        update_pending,
    })
}

/// Sets a link ADE up: one run of its prep command, if it has one, then the mark. Nothing is
/// installed, so there is no version to compare and no link yet: the app makes it with an SSH
/// token. An SSH host ADE (Orca) has no prep command: it only gets the mark.
async fn prepare(
    client: &Client,
    ade: Ade,
    prep: Option<&str>,
    id: &str,
    desktop_version: &str,
    progress: &impl Fn(Step),
) -> Result<Connected, SandboxError> {
    progress(Step::Installing(desktop_version.to_string()));
    if let Some(prep) = prep {
        let r = client
            .exec(
                id,
                &ExecRequest {
                    cmd: prep.to_string(),
                    timeout_seconds: Some(PREP_TIMEOUT_SECS),
                },
            )
            .await?;
        if r.timed_out || r.exit_code != 0 {
            return Err(setup_failure(ade, &r, PREP_TIMEOUT_SECS));
        }
    }
    mark_connected(client, ade, id).await?;
    progress(Step::Done);
    Ok(Connected {
        machine_id: id.to_string(),
        link: None,
        version: None,
        update_pending: false,
    })
}

/// Checks a host ADE on the running sandbox, with one exec. When its host runs, the sandbox
/// gets the ADE in its mark and the link that opens it. Else nothing changes, and the user sets
/// the host up in the app (`link` is None). InstaBox installs nothing for it either way.
async fn check_host(
    client: &Client,
    ade: Ade,
    h: Host,
    id: &str,
    progress: &impl Fn(Step),
) -> Result<Connected, SandboxError> {
    let state = host_state(client, ade, h, id).await?;
    let link = if state.running {
        let link = link_of(ade, h, &state)?;
        // Only a host that runs counts: the mark says the setup is done.
        mark_connected(client, ade, id).await?;
        Some(link)
    } else {
        None
    };
    progress(Step::Done);
    Ok(Connected {
        machine_id: id.to_string(),
        link,
        version: None,
        update_pending: false,
    })
}

/// Runs a host ADE's script on the running sandbox and reads what it found.
async fn host_state(
    client: &Client,
    ade: Ade,
    h: Host,
    id: &str,
) -> Result<HostState, SandboxError> {
    let r = client
        .exec(
            id,
            &ExecRequest {
                cmd: h.script.to_string(),
                timeout_seconds: Some(HOST_TIMEOUT_SECS),
            },
        )
        .await?;
    if r.timed_out || r.exit_code != 0 {
        return Err(setup_failure(ade, &r, HOST_TIMEOUT_SECS));
    }
    Ok((h.state)(&r.stdout))
}

/// The host ADE's link for the sandbox, from its machine id.
fn link_of(ade: Ade, h: Host, state: &HostState) -> Result<String, SandboxError> {
    let id = state
        .machine_id
        .as_deref()
        .ok_or_else(|| SandboxError::Setup {
            ade,
            stderr_tail: "The sandbox has no valid /etc/machine-id".to_string(),
        })?;
    Ok((h.link)(id))
}

/// The link that opens running sandbox `id`, which has its host set up, in a host ADE: one
/// exec reads its machine id. Nothing changes on the sandbox or in its mark.
pub async fn host_link(client: &Client, ade: Ade, id: &str) -> Result<String, SandboxError> {
    let Open::Host(h) = ade.open() else {
        return Err(SandboxError::Setup {
            ade,
            stderr_tail: format!("{} does not run sandboxes as hosts", ade.name()),
        });
    };
    let state = host_state(client, ade, h, id).await?;
    link_of(ade, h, &state)
}

/// True when the sandbox reports a version newer than the desktop's. Unreadable means no.
fn remote_is_newer(desktop: &str, remote: Option<&str>) -> bool {
    match (
        semver::Version::parse(desktop),
        remote.map(semver::Version::parse),
    ) {
        (Ok(d), Some(Ok(r))) => r > d,
        _ => false,
    }
}

/// A fresh pairing link: one run of the ADE's pair script, then the published port. The
/// sandbox must be running.
pub async fn pair_link(client: &Client, ade: Ade, id: &str) -> Result<String, SandboxError> {
    let Open::Pair(p) = ade.open() else {
        return Err(SandboxError::Setup {
            ade,
            stderr_tail: format!("{} does not pair", ade.name()),
        });
    };
    let pair = client
        .exec(
            id,
            &ExecRequest {
                cmd: p.script.to_string(),
                timeout_seconds: Some(PAIR_TIMEOUT_SECS),
            },
        )
        .await?;
    let code = if pair.exit_code == 0 && !pair.timed_out {
        (p.code)(&pair.stdout)
    } else {
        None
    };
    let Some(code) = code else {
        if pair.timed_out || !pair.stderr.trim().is_empty() {
            return Err(setup_failure(ade, &pair, PAIR_TIMEOUT_SECS));
        }
        return Err(SandboxError::Setup {
            ade,
            stderr_tail: format!("{} did not return a pairing code", ade.short_name()),
        });
    };
    let port = client.publish_port(id, p.port).await?;
    Ok((p.link)(&port.url, &code))
}

/// Pairs the ADE's desktop app again with a connected sandbox: wakes it, makes a new
/// pairing link. No setup and no metadata change.
pub async fn pair_again(client: &Client, ade: Ade, id: &str) -> Result<String, SandboxError> {
    ensure_running(client, id).await?;
    pair_link(client, ade, id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Credentials;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use url::Url;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    fn client(server: &MockServer) -> Client {
        Client::new(Url::parse(&server.uri()).unwrap(), Arc::new(Creds))
    }

    fn machine(state: &str, metadata: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": "m1", "name": "n", "state": state, "revision": 1,
            "template": {"id": "agent-browser", "version": 1, "digest": "d"},
            "spec": {"vcpus": 2, "memMiB": 4096, "persistentDiskGiB": 10},
            "runtime": {"ready": true}, "autoSuspendAfterSeconds": 1800,
            "maxRunMinutes": -1, "runStartedAt": null, "cooldownUntil": null,
            "metadata": metadata, "orgId": "o1", "free": false, "operationId": null
        })
    }

    fn op(state: &str, m: Option<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({"id": "op1", "machineId": "m1", "type": "create",
            "state": state, "error": null, "machine": m})
    }

    fn err(status: u16, code: &str, extra: serde_json::Value) -> ResponseTemplate {
        let mut e = serde_json::json!({"code": code, "message": "A readable message",
            "retryable": false});
        if let (Some(o), Some(x)) = (e.as_object_mut(), extra.as_object()) {
            o.extend(x.clone());
        }
        ResponseTemplate::new(status).set_body_json(serde_json::json!({ "error": e }))
    }

    async fn mock(server: &MockServer, m: &str, p: &str, resp: ResponseTemplate) {
        Mock::given(method(m))
            .and(path(p))
            .respond_with(resp)
            .mount(server)
            .await;
    }

    async fn me(server: &MockServer, tier: &str) {
        mock(
            server,
            "GET",
            "/v1/me",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "userId": 1, "login": "me", "org": {"id": "o1", "tier": tier, "kind": "personal"}
            })),
        )
        .await;
    }

    /// Mocks for the happy tail: ops, exec (setup then pair), port, read machine, patch.
    async fn tail(server: &MockServer, setup: ResponseTemplate, metadata: serde_json::Value) {
        mock(
            server,
            "GET",
            "/v1/operations/op1",
            ResponseTemplate::new(200)
                .set_body_json(op("succeeded", Some(machine("running", metadata.clone())))),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains("\"cmd\":\"t3 --version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"stdout": "", "stderr": "", "exitCode": 0}
            )))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains("9.9.9"))
            .respond_with(setup)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains("t3 pair"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"stdout": "Pairing\nToken: abc123\n", "stderr": "", "exitCode": 0}
            )))
            .mount(server)
            .await;
        mock(
            server,
            "POST",
            "/v1/machines/m1/ports",
            ResponseTemplate::new(201).set_body_json(serde_json::json!(
                {"id": "p1", "port": 3774, "url": "https://m1.example.test", "access": "public"}
            )),
        )
        .await;
        mock(
            server,
            "GET",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("running", metadata)),
        )
        .await;
        mock(
            server,
            "PATCH",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("running", serde_json::json!({}))),
        )
        .await;
    }

    fn ok_setup() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({"stdout": "", "stderr": "", "exitCode": 0}))
    }

    fn req() -> NewSandbox {
        NewSandbox {
            name: "box".into(),
            source: Source::Standard,
            spec: MachineSpec {
                vcpus: 2,
                mem_mib: 4096,
            },
            auto_suspend_after_seconds: Some(1800),
            org_id: None,
        }
    }

    async fn calls(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| format!("{} {}", r.method, r.url.path()))
            .collect()
    }

    fn pos(calls: &[String], needle: &str) -> usize {
        calls
            .iter()
            .position(|c| c == needle)
            .unwrap_or_else(|| panic!("no call {needle} in {calls:?}"))
    }

    #[tokio::test]
    async fn creates_then_installs_then_pairs() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        tail(&server, ok_setup(), serde_json::json!({})).await;
        let steps = Mutex::new(Vec::new());
        let out = create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |s| {
            steps.lock().unwrap().push(s)
        })
        .await
        .unwrap();
        assert_eq!(out.machine_id, "m1");
        let link = out.link.unwrap();
        assert!(link.starts_with("t3code://pair?pairingUrl="));
        assert!(link.contains("abc123"));
        assert_eq!(out.version.as_deref(), Some("9.9.9"));
        let c = calls(&server).await;
        let order = [
            "POST /v1/machines",
            "GET /v1/operations/op1",
            "POST /v1/machines/m1/exec",
            "POST /v1/machines/m1/ports",
            "PATCH /v1/machines/m1",
        ];
        let idx: Vec<usize> = order.iter().map(|n| pos(&c, n)).collect();
        assert!(idx.windows(2).all(|w| w[0] < w[1]), "{c:?}");
        let reqs = server.received_requests().await.unwrap();
        let create = reqs
            .iter()
            .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v1/machines")
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&create.body).unwrap();
        assert_eq!(body["templateId"], "agent-browser");
        assert!(body.get("metadata").is_none());
        assert!(create.headers.get("Idempotency-Key").is_some());
        let setup = reqs
            .iter()
            .find(|r| String::from_utf8_lossy(&r.body).contains("9.9.9"))
            .unwrap();
        let sb: serde_json::Value = serde_json::from_slice(&setup.body).unwrap();
        assert_eq!(sb["timeoutSeconds"], 600);
        assert_eq!(
            *steps.lock().unwrap(),
            vec![
                Step::Creating,
                Step::Created("m1".into()),
                Step::Installing("9.9.9".into()),
                Step::Pairing,
                Step::Done
            ]
        );
    }

    /// Mounts the exec mocks of an already-running sandbox whose T3 reports `remote`.
    async fn running_with_remote(
        server: &MockServer,
        remote: &str,
        setup: ResponseTemplate,
        setup_times: u64,
    ) {
        mock(
            server,
            "GET",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("running", serde_json::json!({}))),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains("\"cmd\":\"t3 --version"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"stdout": remote, "stderr": "", "exitCode": 0}),
                ),
            )
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains("9.9.9"))
            .respond_with(setup)
            .expect(setup_times)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains("t3 pair"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"stdout": "Token: abc123\n", "stderr": "", "exitCode": 0}
            )))
            .mount(server)
            .await;
        mock(
            server,
            "POST",
            "/v1/machines/m1/ports",
            ResponseTemplate::new(201).set_body_json(serde_json::json!(
                {"id": "p1", "port": 3774, "url": "https://m1.example.test", "access": "public"}
            )),
        )
        .await;
        mock(
            server,
            "PATCH",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("running", serde_json::json!({}))),
        )
        .await;
    }

    /// The start of the refresh exec's body: the setup without its version part.
    const REFRESH: &str = r#""cmd":"set -eu\nsudo install -d"#;

    async fn refresh_mock(server: &MockServer, answer: ResponseTemplate, times: u64) {
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains(REFRESH))
            .respond_with(answer)
            // Before the pair mock: the setup's comments name `t3 pair` too.
            .with_priority(1)
            .expect(times)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn newer_remote_t3_skips_setup_and_still_pairs() {
        let server = MockServer::start().await;
        let current = format!("t3 v99.0.0\n{}\n", crate::ade::t3::setup_line());
        running_with_remote(&server, &current, ok_setup(), 0).await;
        refresh_mock(&server, ok_setup(), 0).await;
        let out = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap();
        assert!(out.link.unwrap().starts_with("t3code://pair?pairingUrl="));
        // Dropping the server checks the expect(0) on the setup and refresh execs.
        drop(server);
    }

    #[tokio::test]
    async fn newer_remote_t3_with_an_older_setup_is_refreshed_then_paired() {
        let server = MockServer::start().await;
        running_with_remote(&server, "t3 v99.0.0\n", ok_setup(), 0).await;
        refresh_mock(&server, ok_setup(), 1).await;
        let out = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap();
        assert!(out.link.unwrap().starts_with("t3code://pair?pairingUrl="));
        assert_eq!(
            (out.version.as_deref(), out.update_pending),
            (Some("99.0.0"), false),
            "keeps its newer T3"
        );
        let bodies: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/exec"))
            .map(|r| String::from_utf8_lossy(&r.body).to_string())
            .collect();
        let refresh = bodies.iter().position(|b| b.contains(REFRESH)).unwrap();
        let pair = bodies
            .iter()
            .position(|b| b.contains(r#""cmd":"t3 pair"#))
            .unwrap();
        assert!(refresh < pair, "{bodies:?}");
        drop(server);
    }

    #[tokio::test]
    async fn a_busy_refresh_of_a_newer_t3_still_pairs_and_offers_no_update() {
        let server = MockServer::start().await;
        running_with_remote(&server, "t3 v99.0.0\n", ok_setup(), 0).await;
        refresh_mock(
            &server,
            exec_answer(3, "T3 is busy, update deferred", false),
            1,
        )
        .await;
        let out = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap();
        assert!(out.link.unwrap().starts_with("t3code://pair?pairingUrl="));
        // Update would refuse to move a newer T3 back: the next sync refreshes it.
        assert_eq!(
            (out.version.as_deref(), out.update_pending),
            (Some("99.0.0"), false)
        );
        let c = calls(&server).await;
        assert!(c.iter().any(|x| x == "PATCH /v1/machines/m1"), "{c:?}");
        drop(server);
    }

    #[tokio::test]
    async fn a_failed_refresh_stops_the_connect() {
        let server = MockServer::start().await;
        running_with_remote(&server, "t3 v99.0.0\n", ok_setup(), 0).await;
        refresh_mock(&server, exec_answer(1, "boom", false), 1).await;
        let e = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap_err();
        assert_eq!(
            e,
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail: "boom".into()
            }
        );
        let c = calls(&server).await;
        assert!(!c.iter().any(|x| x.starts_with("PATCH")), "{c:?}");
        drop(server);
    }

    #[tokio::test]
    async fn older_remote_t3_runs_setup() {
        let server = MockServer::start().await;
        running_with_remote(&server, "t3 v9.0.0\n", ok_setup(), 1).await;
        let out = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap();
        assert_eq!(
            (out.version.as_deref(), out.update_pending),
            (Some("9.9.9"), false)
        );
        drop(server);
    }

    fn exec_answer(code: i32, stderr: &str, timed_out: bool) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!(
            {"stdout": "", "stderr": stderr, "exitCode": code, "timedOut": timed_out}
        ))
    }

    #[tokio::test]
    async fn a_busy_t3_that_runs_already_is_paired_and_its_update_waits() {
        let server = MockServer::start().await;
        running_with_remote(&server, "t3 v9.0.0\n", exec_answer(3, "busy", false), 1).await;
        let steps = Mutex::new(Vec::new());
        let out = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |s| {
            steps.lock().unwrap().push(s)
        })
        .await
        .unwrap();
        assert!(out.link.unwrap().starts_with("t3code://pair?pairingUrl="));
        assert_eq!(
            out.version.as_deref(),
            Some("9.0.0"),
            "keeps its own T3 for now"
        );
        assert!(out.update_pending);
        assert!(steps.lock().unwrap().contains(&Step::Done));
        let c = calls(&server).await;
        assert!(
            c.iter().any(|x| x == "PATCH /v1/machines/m1"),
            "marked: {c:?}"
        );
        drop(server);
    }

    #[tokio::test]
    async fn a_timed_out_setup_says_so() {
        let server = MockServer::start().await;
        running_with_remote(&server, "", exec_answer(124, "", true), 1).await;
        let e = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap_err();
        assert_eq!(
            e,
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail: "Timed out after 600 s".into()
            }
        );
        let c = calls(&server).await;
        assert!(!c.iter().any(|x| x.starts_with("PATCH")), "{c:?}");
    }

    #[test]
    fn a_setup_failure_message_is_never_empty() {
        let r = |code, stderr: &str, timed_out| ExecResult {
            stdout: String::new(),
            stderr: stderr.into(),
            exit_code: code,
            timed_out,
        };
        let msg = |e: SandboxError| match e {
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail,
            } => stderr_tail,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            msg(setup_failure(Ade::T3, &r(124, "curl: slow\n", true), 600)),
            "Timed out after 600 s\ncurl: slow"
        );
        assert_eq!(
            msg(setup_failure(Ade::T3, &r(1, "boom", false), 600)),
            "boom"
        );
        assert_eq!(
            msg(setup_failure(Ade::T3, &r(7, " \n", false), 600)),
            "The command exited with code 7"
        );
    }

    #[tokio::test]
    async fn pair_again_makes_no_setup_and_no_patch() {
        let server = MockServer::start().await;
        running_with_remote(&server, "t3 v9.9.9\n", ok_setup(), 0).await;
        let link = pair_again(&client(&server), Ade::T3, "m1").await.unwrap();
        assert!(link.starts_with("t3code://pair?pairingUrl="));
        let c = calls(&server).await;
        assert!(!c.iter().any(|x| x.starts_with("PATCH")), "{c:?}");
        let reqs = server.received_requests().await.unwrap();
        let execs = reqs
            .iter()
            .filter(|r| r.url.path().ends_with("/exec"))
            .count();
        assert_eq!(execs, 1, "only t3 pair");
        drop(server);
    }

    #[tokio::test]
    async fn resumes_a_paused_sandbox_first() {
        let server = MockServer::start().await;
        // First read says suspended; connect must resume before any exec.
        Mock::given(method("GET"))
            .and(path("/v1/machines/m1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(machine("suspended", serde_json::json!({}))),
            )
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        mock(
            &server,
            "POST",
            "/v1/machines/m1/resume",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        tail(&server, ok_setup(), serde_json::json!({})).await;
        let out = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap();
        assert_eq!(out.machine_id, "m1");
        let c = calls(&server).await;
        assert!(pos(&c, "POST /v1/machines/m1/resume") < pos(&c, "POST /v1/machines/m1/exec"));
        let reqs = server.received_requests().await.unwrap();
        let resume = reqs
            .iter()
            .find(|r| r.url.path().ends_with("/resume"))
            .unwrap();
        assert!(resume.headers.get("Idempotency-Key").is_some());
    }

    #[tokio::test]
    async fn setup_failure_keeps_sandbox_and_no_mark() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        let stderr: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        tail(
            &server,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"stdout": "", "stderr": stderr, "exitCode": 1})),
            serde_json::json!({}),
        )
        .await;
        let e = create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |_| {})
            .await
            .unwrap_err();
        match e {
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail,
            } => {
                let lines: Vec<&str> = stderr_tail.lines().collect();
                assert_eq!(lines.len(), 20);
                assert_eq!(lines[0], "line 11");
                assert_eq!(lines[19], "line 30");
            }
            other => panic!("wrong error: {other:?}"),
        }
        let c = calls(&server).await;
        assert!(!c.iter().any(|x| x.starts_with("PATCH")), "{c:?}");
        assert!(!c.iter().any(|x| x.ends_with("/ports")), "{c:?}");
        assert!(!c.iter().any(|x| x.starts_with("DELETE")), "{c:?}");
    }

    #[tokio::test]
    async fn busy_setup_exit_is_t3_busy() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        tail(
            &server,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"stdout": "", "stderr": "busy", "exitCode": 3})),
            serde_json::json!({}),
        )
        .await;
        let e = create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |_| {})
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::AdeBusy(Ade::T3));
    }

    #[tokio::test]
    async fn free_limit_is_readable() {
        let server = MockServer::start().await;
        me(&server, "free").await;
        mock(
            &server,
            "GET",
            "/v1/usage",
            ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"limits": {"maxMachines": 1}, "org": {"id": "o1", "tier": "free"}}
            )),
        )
        .await;
        mock(
            &server,
            "GET",
            "/v1/machines",
            ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"machines": [machine("suspended", serde_json::json!({}))], "nextCursor": null}
            )),
        )
        .await;
        let e = create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |_| {})
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::FreeLimit);
        assert_eq!(e.to_string(), "The free plan has one sandbox");
        let c = calls(&server).await;
        assert!(!c.contains(&"POST /v1/machines".to_string()), "{c:?}");
    }

    #[tokio::test]
    async fn free_org_under_limit_creates() {
        let server = MockServer::start().await;
        me(&server, "free").await;
        mock(
            &server,
            "GET",
            "/v1/usage",
            ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"limits": {"maxMachines": 1}, "org": {"id": "o1", "tier": "free"}}
            )),
        )
        .await;
        mock(
            &server,
            "GET",
            "/v1/machines",
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"machines": [], "nextCursor": null})),
        )
        .await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        tail(&server, ok_setup(), serde_json::json!({})).await;
        create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |_| {})
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cooldown_is_readable() {
        let server = MockServer::start().await;
        let until = chrono::Utc::now() + chrono::Duration::seconds(7 * 60 - 10);
        let mut m = machine("suspended", serde_json::json!({}));
        m["cooldownUntil"] = serde_json::json!(until.to_rfc3339());
        mock(
            &server,
            "GET",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(m),
        )
        .await;
        mock(
            &server,
            "POST",
            "/v1/machines/m1/resume",
            err(
                409,
                "machine_cooldown",
                serde_json::json!({"retryable": true}),
            ),
        )
        .await;
        let e = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::Cooldown { minutes: 7 });
        assert_eq!(e.to_string(), "Cooling down, ready in about 7 min");
        let c = calls(&server).await;
        assert!(!c.iter().any(|x| x.ends_with("/exec")), "{c:?}");
    }

    #[tokio::test]
    async fn cooldown_falls_back_to_retry_after() {
        let server = MockServer::start().await;
        mock(
            &server,
            "GET",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("stopped", serde_json::json!({}))),
        )
        .await;
        mock(
            &server,
            "POST",
            "/v1/machines/m1/start",
            err(
                409,
                "machine_cooldown",
                serde_json::json!({"retryable": true, "retryAfterSeconds": 61}),
            ),
        )
        .await;
        let e = connect_existing(&client(&server), Ade::T3, "m1", "9.9.9", |_| {})
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::Cooldown { minutes: 2 });
    }

    #[tokio::test]
    async fn metadata_merge_keeps_other_keys() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        tail(&server, ok_setup(), serde_json::json!({"a": "1"})).await;
        create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |_| {})
            .await
            .unwrap();
        let reqs = server.received_requests().await.unwrap();
        let patch = reqs.iter().find(|r| r.method.as_str() == "PATCH").unwrap();
        let body: serde_json::Value = serde_json::from_slice(&patch.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"metadata": {"a": "1", "instabox-bridge": "t3", "instabox-ades": "t3"}})
        );
        assert!(patch.headers.get("Idempotency-Key").is_some());
    }

    #[tokio::test]
    async fn snapshot_source_sends_snapshot_id() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        tail(&server, ok_setup(), serde_json::json!({})).await;
        let mut r = req();
        r.source = Source::Snapshot("snap1".into());
        create_and_connect(&client(&server), Ade::T3, &r, "9.9.9", |_| {})
            .await
            .unwrap();
        let reqs = server.received_requests().await.unwrap();
        let create = reqs
            .iter()
            .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v1/machines")
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&create.body).unwrap();
        assert_eq!(body["snapshotId"], "snap1");
        assert!(body.get("templateId").is_none());
    }

    #[tokio::test]
    async fn wait_op_retries_a_network_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/operations/op1"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!(
                {"error": {"code": "unavailable", "message": "m", "retryable": true}})))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        mock(
            &server,
            "GET",
            "/v1/operations/op1",
            ResponseTemplate::new(200).set_body_json(op(
                "succeeded",
                Some(machine("running", serde_json::json!({}))),
            )),
        )
        .await;
        let o: crate::api::Operation = serde_json::from_value(op("queued", None)).unwrap();
        let m = crate::sandbox::wait_op(&client(&server), &o, std::time::Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(m.id, "m1");
    }

    #[tokio::test]
    async fn create_retries_503_with_same_key() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        Mock::given(method("POST"))
            .and(path("/v1/machines"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!(
                {"error": {"code": "unavailable", "message": "m", "retryable": false}})))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        tail(&server, ok_setup(), serde_json::json!({})).await;
        create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |_| {})
            .await
            .unwrap();
        let reqs = server.received_requests().await.unwrap();
        let posts: Vec<_> = reqs
            .iter()
            .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/v1/machines")
            .collect();
        assert_eq!(posts.len(), 2);
        let k0 = posts[0].headers.get("Idempotency-Key").unwrap();
        assert_eq!(k0, posts[1].headers.get("Idempotency-Key").unwrap());
    }

    #[tokio::test]
    async fn created_step_comes_before_any_exec() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        tail(
            &server,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"stdout": "", "stderr": "boom", "exitCode": 1})),
            serde_json::json!({}),
        )
        .await;
        let seen = Mutex::new(Vec::new());
        let _ = create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |s| {
            if let Step::Created(id) = &s {
                seen.lock().unwrap().push(id.clone());
            }
        })
        .await;
        assert_eq!(*seen.lock().unwrap(), vec!["m1".to_string()]);
    }

    /// A running sandbox with `metadata`, whose exec answers `prep` with `code`.
    async fn running_for_prep(server: &MockServer, metadata: serde_json::Value, code: i32) {
        mock(
            server,
            "GET",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("running", metadata)),
        )
        .await;
        let stderr = if code == 0 { "" } else { "chmod: denied" };
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains("chmod go-w ~/.local ~/.local/state"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"stdout": "", "stderr": stderr, "exitCode": code}),
            ))
            .expect(1)
            .mount(server)
            .await;
        mock(
            server,
            "PATCH",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("running", serde_json::json!({}))),
        )
        .await;
    }

    #[tokio::test]
    async fn cmux_runs_its_prep_and_adds_itself_to_the_mark() {
        let server = MockServer::start().await;
        running_for_prep(
            &server,
            serde_json::json!({"a": "1", "instabox-bridge": "t3"}),
            0,
        )
        .await;
        let steps = Mutex::new(Vec::new());
        let out = connect_existing(&client(&server), Ade::Cmux, "m1", "0.64.25", |s| {
            steps.lock().unwrap().push(s)
        })
        .await
        .unwrap();
        assert_eq!(
            out,
            Connected {
                machine_id: "m1".into(),
                link: None,
                version: None,
                update_pending: false,
            }
        );
        assert_eq!(
            *steps.lock().unwrap(),
            vec![Step::Installing("0.64.25".into()), Step::Done]
        );
        let reqs = server.received_requests().await.unwrap();
        // One exec (the prep), no port, no pairing; the mark keeps T3 and adds cmux.
        let execs: Vec<_> = reqs
            .iter()
            .filter(|r| r.url.path().ends_with("/exec"))
            .collect();
        assert_eq!(execs.len(), 1);
        let prep: serde_json::Value = serde_json::from_slice(&execs[0].body).unwrap();
        assert_eq!(
            prep["cmd"],
            "mkdir -p ~/.local/state && chmod go-w ~/.local ~/.local/state"
        );
        assert!(!reqs.iter().any(|r| r.url.path().ends_with("/ports")));
        let patch = reqs.iter().find(|r| r.method.as_str() == "PATCH").unwrap();
        let body: serde_json::Value = serde_json::from_slice(&patch.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"metadata": {"a": "1", "instabox-bridge": "t3", "instabox-ades": "t3,cmux"}})
        );
        drop(server);
    }

    #[tokio::test]
    async fn a_failed_cmux_prep_leaves_no_mark() {
        let server = MockServer::start().await;
        running_for_prep(&server, serde_json::json!({}), 1).await;
        let e = connect_existing(&client(&server), Ade::Cmux, "m1", "0.64.25", |_| {})
            .await
            .unwrap_err();
        assert_eq!(
            e,
            SandboxError::Setup {
                ade: Ade::Cmux,
                stderr_tail: "chmod: denied".into()
            }
        );
        let c = calls(&server).await;
        assert!(!c.iter().any(|x| x.starts_with("PATCH")), "{c:?}");
    }

    /// A running sandbox with `metadata`, whose exec answers Paradev's host check with `stdout`
    /// and `code`.
    async fn running_for_host(
        server: &MockServer,
        metadata: serde_json::Value,
        stdout: &str,
        code: i32,
    ) {
        mock(
            server,
            "GET",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("running", metadata)),
        )
        .await;
        let stderr = if code == 0 { "" } else { "bash: broken" };
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains(
                "systemctl --user is-active superset-host",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"stdout": stdout, "stderr": stderr, "exitCode": code}),
            ))
            .expect(1)
            .mount(server)
            .await;
        mock(
            server,
            "PATCH",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(machine("running", serde_json::json!({}))),
        )
        .await;
    }

    const HOST_RUNS: &str = "service=active\nmachineid=0123456789abcdef0123456789abcdef\n";

    #[tokio::test]
    async fn paradev_marks_a_sandbox_only_once_its_host_runs() {
        let server = MockServer::start().await;
        running_for_host(
            &server,
            serde_json::json!({"a": "1", "instabox-bridge": "t3"}),
            HOST_RUNS,
            0,
        )
        .await;
        let steps = Mutex::new(Vec::new());
        let out = connect_existing(&client(&server), Ade::Paradev, "m1", "2.0.11", |s| {
            steps.lock().unwrap().push(s)
        })
        .await
        .unwrap();
        // The link opens a new workspace on the host whose id Paradev derives from the
        // machine id.
        assert_eq!(
            out,
            Connected {
                machine_id: "m1".into(),
                link: Some("superset://new-workspace?host=977b9d5310b351a8e85d4a4b333d5481".into()),
                version: None,
                update_pending: false,
            }
        );
        assert_eq!(*steps.lock().unwrap(), vec![Step::Done]);
        let reqs = server.received_requests().await.unwrap();
        // One exec (the check, which changes nothing), no port, no pairing.
        let execs: Vec<_> = reqs
            .iter()
            .filter(|r| r.url.path().ends_with("/exec"))
            .collect();
        assert_eq!(execs.len(), 1);
        let check: serde_json::Value = serde_json::from_slice(&execs[0].body).unwrap();
        assert_eq!(check["cmd"], crate::ade::paradev::HOST_SCRIPT);
        assert!(!reqs.iter().any(|r| r.url.path().ends_with("/ports")));
        let patch = reqs.iter().find(|r| r.method.as_str() == "PATCH").unwrap();
        let body: serde_json::Value = serde_json::from_slice(&patch.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"metadata": {"a": "1", "instabox-bridge": "t3", "instabox-ades": "t3,paradev"}})
        );
        drop(server);
    }

    #[tokio::test]
    async fn a_sandbox_whose_paradev_host_does_not_run_is_left_unmarked() {
        for stdout in [
            "service=inactive\nmachineid=0123456789abcdef0123456789abcdef\n",
            "service=activating\nmachineid=0123456789abcdef0123456789abcdef\n",
            "service=\nmachineid=\n",
        ] {
            let server = MockServer::start().await;
            running_for_host(&server, serde_json::json!({}), stdout, 0).await;
            let out = connect_existing(&client(&server), Ade::Paradev, "m1", "2.0.11", |_| {})
                .await
                .unwrap();
            assert_eq!(
                out.link, None,
                "{stdout}: the user sets the host up in Paradev"
            );
            let c = calls(&server).await;
            assert!(!c.iter().any(|x| x.starts_with("PATCH")), "{stdout}: {c:?}");
            drop(server);
        }
    }

    #[tokio::test]
    async fn a_failed_paradev_check_or_a_running_host_without_machine_id_marks_nothing() {
        let server = MockServer::start().await;
        running_for_host(&server, serde_json::json!({}), "", 2).await;
        let e = connect_existing(&client(&server), Ade::Paradev, "m1", "2.0.11", |_| {})
            .await
            .unwrap_err();
        assert_eq!(
            e,
            SandboxError::Setup {
                ade: Ade::Paradev,
                stderr_tail: "bash: broken".into()
            }
        );
        assert!(!calls(&server).await.iter().any(|x| x.starts_with("PATCH")));

        let server = MockServer::start().await;
        running_for_host(
            &server,
            serde_json::json!({}),
            "service=active\nmachineid=uninitialized\n",
            0,
        )
        .await;
        let e = connect_existing(&client(&server), Ade::Paradev, "m1", "2.0.11", |_| {})
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "Setting up Paradev failed:\nThe sandbox has no valid /etc/machine-id"
        );
        assert!(!calls(&server).await.iter().any(|x| x.starts_with("PATCH")));
    }

    #[tokio::test]
    async fn the_paradev_link_of_a_marked_sandbox_reads_only_its_machine_id() {
        let server = MockServer::start().await;
        let md = serde_json::json!({"instabox-bridge": "paradev", "instabox-ades": "paradev"});
        running_for_host(&server, md, HOST_RUNS, 0).await;
        let link = host_link(&client(&server), Ade::Paradev, "m1")
            .await
            .unwrap();
        assert_eq!(
            link,
            "superset://new-workspace?host=977b9d5310b351a8e85d4a4b333d5481"
        );
        let c = calls(&server).await;
        assert_eq!(c, ["POST /v1/machines/m1/exec"], "no mark, no wake");
        assert!(host_link(&client(&server), Ade::Cmux, "m1").await.is_err());
        drop(server);
    }

    #[tokio::test]
    async fn a_plain_create_installs_nothing_and_marks_nothing() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        mock(
            &server,
            "POST",
            "/v1/machines",
            ResponseTemplate::new(202).set_body_json(op("queued", None)),
        )
        .await;
        mock(
            &server,
            "GET",
            "/v1/operations/op1",
            ResponseTemplate::new(200).set_body_json(op(
                "succeeded",
                Some(machine("running", serde_json::json!({}))),
            )),
        )
        .await;
        let steps = Mutex::new(Vec::new());
        let id = create(&client(&server), &req(), &|s| steps.lock().unwrap().push(s))
            .await
            .unwrap();
        assert_eq!(id, "m1");
        assert_eq!(
            *steps.lock().unwrap(),
            vec![Step::Creating, Step::Created("m1".into())]
        );
        let c = calls(&server).await;
        assert!(
            !c.iter()
                .any(|x| x.ends_with("/exec") || x.starts_with("PATCH")),
            "{c:?}"
        );
    }

    #[tokio::test]
    async fn non_transient_create_error_is_not_retried() {
        let server = MockServer::start().await;
        me(&server, "pro").await;
        Mock::given(method("POST"))
            .and(path("/v1/machines"))
            .respond_with(err(400, "bad_request", serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let e = create_and_connect(&client(&server), Ade::T3, &req(), "9.9.9", |_| {})
            .await
            .unwrap_err();
        assert!(matches!(e, SandboxError::Api(_)));
    }
}
