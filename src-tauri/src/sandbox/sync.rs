//! Keeps a connected sandbox's ADE in step with the desktop's.

use semver::Version;

use crate::ade::{Ade, ProbeAnswer, SETUP_EXIT_BUSY, parse_probe};
use crate::api::{Client, ExecRequest, ExecResult};
use crate::sandbox::SandboxError;
use crate::sandbox::connect::{SETUP_TIMEOUT_SECS, setup_failure};

const QUERY_TIMEOUT_SECS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Versions match (or the desktop version is unreadable). Nothing to do.
    UpToDate,
    /// The sandbox is behind and the ADE is idle. Update it.
    Update,
    /// The sandbox is behind but the ADE is busy. Try again later.
    Defer,
    /// The sandbox is ahead of the desktop. Never move it back.
    Older,
}

/// Decides by semver order (a prerelease sorts before its release). A missing or
/// unparsable remote version means Update. An unparsable desktop version means
/// UpToDate: the bridge never acts on doubt.
pub fn decide(desktop: &str, remote: Option<&str>, probe: &ProbeAnswer) -> Decision {
    let Ok(desktop) = Version::parse(desktop) else {
        return Decision::UpToDate;
    };
    match remote.and_then(|r| Version::parse(r).ok()) {
        Some(r) if r == desktop => Decision::UpToDate,
        Some(r) if r > desktop => Decision::Older,
        _ if probe.busy => Decision::Defer,
        _ => Decision::Update,
    }
}

/// What a sync found and did. `remote_version` is what the sandbox reported before any update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub decision: Decision,
    pub remote_version: Option<String>,
}

async fn run(client: &Client, id: &str, cmd: &str, secs: u32) -> Result<ExecResult, SandboxError> {
    Ok(client
        .exec(
            id,
            &ExecRequest {
                cmd: cmd.to_string(),
                timeout_seconds: Some(secs),
            },
        )
        .await?)
}

/// What `refresh` left on the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refresh {
    /// The sandbox runs this build's setup, or has no ADE version to refresh.
    Current,
    /// The ADE is busy, so its restart waits. The next sync tries again.
    Deferred,
}

/// Brings the ADE's setup on the sandbox up to this build's when `found`, the output of its
/// version script, shows an older one, at the version the sandbox has. Nothing runs for a
/// current setup, or when no version was found (only a setup installs one).
pub(crate) async fn refresh(
    client: &Client,
    ade: Ade,
    machine_id: &str,
    found: &str,
) -> Result<Refresh, SandboxError> {
    let Some(script) = ade.refresh_script() else {
        return Ok(Refresh::Current);
    };
    if ade.setup_current(found) || ade.parse_version(found).is_none() {
        return Ok(Refresh::Current);
    }
    let r = run(client, machine_id, &script, SETUP_TIMEOUT_SECS).await?;
    match r.exit_code {
        _ if r.timed_out => Err(setup_failure(ade, &r, SETUP_TIMEOUT_SECS)),
        0 => Ok(Refresh::Current),
        SETUP_EXIT_BUSY => Ok(Refresh::Deferred),
        _ => Err(setup_failure(ade, &r, SETUP_TIMEOUT_SECS)),
    }
}

/// Brings the sandbox's ADE up to `desktop_version` when it is behind and idle, and its setup
/// up to this build's when only that is behind (`refresh`).
/// The sandbox must be running and connected: this does not wake it. Callers run
/// it on connect and on user request, never on a timer, because every call execs
/// on the sandbox and would keep it awake. A busy ADE gives `Ok(Defer)`, not an error. An ADE
/// with nothing installed on the sandbox (cmux) is always up to date, with no exec.
pub async fn sync(
    client: &Client,
    ade: Ade,
    machine_id: &str,
    desktop_version: &str,
) -> Result<SyncOutcome, SandboxError> {
    sync_before_pairing(client, ade, machine_id, desktop_version)
        .await
        .0
}

/// `sync`, and whether a pairing may follow it (Open pairs right after). The pairing must wait
/// when the sandbox's setup is older than this build's, the sync could not bring it up to date,
/// and the pairing needs this build's setup (`Ade::pairing_needs_setup`), since the ADE's
/// server would refuse it. It then fails with `AdeBusy` when the ADE was busy, else with the
/// sync's error. In every other case the pairing goes ahead, even after a failed sync.
pub async fn sync_before_pairing(
    client: &Client,
    ade: Ade,
    machine_id: &str,
    desktop_version: &str,
) -> (Result<SyncOutcome, SandboxError>, Result<(), SandboxError>) {
    let (Some(version_script), Some(_)) = (ade.version_script(), ade.probe()) else {
        return (Ok(nothing_installed()), Ok(()));
    };
    let found = match run(client, machine_id, version_script, QUERY_TIMEOUT_SECS).await {
        Ok(out) => out.stdout,
        Err(e) => return (Err(e), Ok(())),
    };
    sync_found(client, ade, machine_id, desktop_version, &found).await
}

/// Before a pairing that sets nothing up (Pair again): the sync of `sync_before_pairing`, but
/// only when the sandbox's setup is older than this build's and the pairing needs it. None:
/// the pairing goes ahead as it is, with no sync, also when the version script failed.
pub async fn sync_if_pairing_needs_it(
    client: &Client,
    ade: Ade,
    machine_id: &str,
    desktop_version: &str,
) -> Option<(Result<SyncOutcome, SandboxError>, Result<(), SandboxError>)> {
    let script = ade.version_script()?;
    let found = run(client, machine_id, script, QUERY_TIMEOUT_SECS)
        .await
        .ok()?
        .stdout;
    let remote = ade.parse_version(&found);
    if ade.setup_current(&found) || !ade.pairing_needs_setup(desktop_version, remote.as_deref()) {
        return None;
    }
    Some(sync_found(client, ade, machine_id, desktop_version, &found).await)
}

fn nothing_installed() -> SyncOutcome {
    SyncOutcome {
        decision: Decision::UpToDate,
        remote_version: None,
    }
}

/// `sync_before_pairing` once the version script printed `found`.
async fn sync_found(
    client: &Client,
    ade: Ade,
    machine_id: &str,
    desktop_version: &str,
    found: &str,
) -> (Result<SyncOutcome, SandboxError>, Result<(), SandboxError>) {
    let Some(probe) = ade.probe() else {
        return (Ok(nothing_installed()), Ok(()));
    };
    let remote = ade.parse_version(found);
    let r = bring_in_step(
        client,
        ade,
        &probe.path(),
        machine_id,
        desktop_version,
        found,
    )
    .await;
    let still_old = !ade.setup_current(found) && !matches!(r, Ok((_, true)));
    let pairing = if still_old && ade.pairing_needs_setup(desktop_version, remote.as_deref()) {
        Err(match &r {
            Err(e) => e.clone(),
            Ok(_) => SandboxError::AdeBusy(ade),
        })
    } else {
        Ok(())
    };
    (r.map(|(outcome, _)| outcome), pairing)
}

/// The rest of a sync once the version script printed `found`. Its flag: a setup or refresh of
/// this sync finished, so the sandbox runs this build's setup now.
async fn bring_in_step(
    client: &Client,
    ade: Ade,
    probe: &str,
    machine_id: &str,
    desktop_version: &str,
    found: &str,
) -> Result<(SyncOutcome, bool), SandboxError> {
    let remote = ade.parse_version(found);
    // Equal or newer: no reason to ask the probe.
    let idle = ProbeAnswer {
        busy: false,
        reason: String::new(),
    };
    let first = decide(desktop_version, remote.as_deref(), &idle);
    let outcome = |decision| SyncOutcome {
        decision,
        remote_version: remote.clone(),
    };
    if matches!(first, Decision::UpToDate | Decision::Older) {
        let refreshed = refresh(client, ade, machine_id, found).await?;
        // A deferred refresh of a sandbox at the desktop's version waits like an update (the
        // app offers Update, which sets that version up again). One that is ahead stays ahead:
        // Update would refuse to move it back, so only the next sync tries again.
        let at_desktop = remote.as_deref() == Some(desktop_version);
        let decision = match (first, refreshed) {
            (Decision::UpToDate, Refresh::Deferred) if at_desktop => Decision::Defer,
            _ => first,
        };
        return Ok((outcome(decision), refreshed == Refresh::Current));
    }
    let out = run(client, machine_id, probe, QUERY_TIMEOUT_SECS).await?;
    let decision = decide(
        desktop_version,
        remote.as_deref(),
        &parse_probe(&out.stdout),
    );
    if decision != Decision::Update {
        return Ok((outcome(decision), false));
    }
    let setup = run(
        client,
        machine_id,
        &ade.setup_script(desktop_version),
        SETUP_TIMEOUT_SECS,
    )
    .await?;
    match setup.exit_code {
        _ if setup.timed_out => Err(setup_failure(ade, &setup, SETUP_TIMEOUT_SECS)),
        0 => Ok((outcome(Decision::Update), true)),
        SETUP_EXIT_BUSY => Ok((outcome(Decision::Defer), false)),
        _ => Err(setup_failure(ade, &setup, SETUP_TIMEOUT_SECS)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Credentials;
    use async_trait::async_trait;
    use std::sync::Arc;
    use url::Url;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn probe(busy: bool) -> ProbeAnswer {
        ProbeAnswer {
            busy,
            reason: String::new(),
        }
    }

    #[test]
    fn decide_table() {
        use Decision::*;
        let idle = probe(false);
        let busy = probe(true);
        assert_eq!(decide("0.1.10", Some("0.1.10"), &idle), UpToDate);
        assert_eq!(decide("0.1.10", Some("0.1.10"), &busy), UpToDate);
        assert_eq!(decide("0.1.10", Some("0.1.9"), &idle), Update);
        assert_eq!(decide("0.1.10", Some("0.1.9"), &busy), Defer);
        assert_eq!(decide("0.1.10", Some("0.1.11"), &idle), Older);
        assert_eq!(decide("0.1.10", None, &idle), Update);
        assert_eq!(decide("0.1.10", None, &busy), Defer);
        // Unparsable remote counts as missing; unparsable desktop never acts.
        assert_eq!(decide("0.1.10", Some("junk"), &idle), Update);
        assert_eq!(decide("junk", Some("0.1.9"), &idle), UpToDate);
    }

    #[test]
    fn prerelease_sorts_before_its_release() {
        use Decision::*;
        let idle = probe(false);
        let nightly = "0.1.11-nightly.20261119.4918";
        assert_eq!(decide("0.1.10", Some(nightly), &idle), Older);
        assert_eq!(decide("0.1.11", Some(nightly), &idle), Update);
        assert_eq!(decide(nightly, Some("0.1.10"), &idle), Update);
        assert_eq!(decide(nightly, Some("0.1.11"), &idle), Older);
        assert_eq!(decide(nightly, Some(nightly), &idle), UpToDate);
    }

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

    /// Mounts an exec mock that must be hit exactly `times` times.
    async fn exec_mock(
        server: &MockServer,
        body_has: &str,
        stdout: &str,
        stderr: &str,
        code: i32,
        times: u64,
    ) {
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains(body_has))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"stdout": stdout, "stderr": stderr, "exitCode": code}
            )))
            .expect(times)
            .mount(server)
            .await;
    }

    // Distinct body markers: the version script, the probe command, the pinned setup version,
    // the refresh (the setup without its version part).
    const VERSION: &str = "t3 --version";
    const PROBE: &str = "\"cmd\":\"/usr/local/lib/instabox/activity.d/t3\"";
    const SETUP: &str = "v=0.1.11";
    const REFRESH: &str = r#""cmd":"set -eu\nprobe="#;

    /// What the version script prints for T3 `v` started with this build's setup.
    fn current(v: &str) -> String {
        format!("t3 v{v}\n{}\n", crate::ade::t3::setup_line())
    }

    #[tokio::test]
    async fn cmux_is_always_up_to_date_without_an_exec() {
        let server = MockServer::start().await;
        let d = sync(&client(&server), Ade::Cmux, "m1", "0.64.25")
            .await
            .unwrap();
        assert_eq!(d.decision, Decision::UpToDate);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn equal_versions_make_one_exec() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, &current("0.1.11"), "", 0, 1).await;
        exec_mock(&server, PROBE, "", "", 0, 0).await;
        exec_mock(&server, SETUP, "", "", 0, 0).await;
        exec_mock(&server, REFRESH, "", "", 0, 0).await;
        let d = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        assert_eq!(d.decision, Decision::UpToDate);
    }

    #[tokio::test]
    async fn newer_remote_makes_one_exec() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, &current("0.1.12"), "", 0, 1).await;
        exec_mock(&server, PROBE, "", "", 0, 0).await;
        exec_mock(&server, SETUP, "", "", 0, 0).await;
        exec_mock(&server, REFRESH, "", "", 0, 0).await;
        let d = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        assert_eq!(d.decision, Decision::Older);
    }

    #[tokio::test]
    async fn equal_versions_with_an_older_setup_get_a_refresh() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "t3 v0.1.11\n", "", 0, 1).await;
        exec_mock(&server, PROBE, "", "", 0, 0).await;
        exec_mock(&server, SETUP, "", "", 0, 0).await;
        exec_mock(&server, REFRESH, "", "", 0, 1).await;
        let d = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        assert_eq!(d.decision, Decision::UpToDate);
        let reqs = server.received_requests().await.unwrap();
        let refresh = reqs
            .iter()
            .find(|r| String::from_utf8_lossy(&r.body).contains(REFRESH))
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&refresh.body).unwrap();
        assert_eq!(body["cmd"], crate::ade::t3::refresh_script());
        assert_eq!(body["timeoutSeconds"], 600);
    }

    #[tokio::test]
    async fn a_busy_refresh_at_the_desktop_version_defers_like_an_update() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "t3 v0.1.11\n", "", 0, 1).await;
        exec_mock(&server, REFRESH, "", "T3 is busy, update deferred", 3, 1).await;
        let d = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        // Defer offers Update, and every later sync runs the refresh again until it is done.
        assert_eq!(d.decision, Decision::Defer);
        assert_eq!(d.remote_version.as_deref(), Some("0.1.11"));
    }

    #[tokio::test]
    async fn a_newer_remote_with_an_older_setup_is_refreshed_and_stays_older() {
        for (code, stderr) in [(0, ""), (3, "T3 is busy, update deferred")] {
            let server = MockServer::start().await;
            exec_mock(&server, VERSION, "t3 v0.1.12\n", "", 0, 1).await;
            exec_mock(&server, PROBE, "", "", 0, 0).await;
            exec_mock(&server, SETUP, "", "", 0, 0).await;
            exec_mock(&server, REFRESH, "", stderr, code, 1).await;
            let d = sync(&client(&server), Ade::T3, "m1", "0.1.11")
                .await
                .unwrap();
            // Busy or not, never Defer: its Update would refuse to move T3 back.
            assert_eq!(d.decision, Decision::Older, "exit {code}");
        }
    }

    #[tokio::test]
    async fn a_failed_refresh_is_an_error_with_stderr_tail() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "t3 v0.1.11\n", "", 0, 1).await;
        exec_mock(&server, REFRESH, "", "line1\nT3 server did not start", 1, 1).await;
        let e = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        assert_eq!(
            e,
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail: "line1\nT3 server did not start".into()
            }
        );
    }

    /// `sync_before_pairing` with T3 Code `desktop` against the mocks mounted on `server`.
    async fn before_pairing(
        server: &MockServer,
        desktop: &str,
    ) -> (Result<SyncOutcome, SandboxError>, Result<(), SandboxError>) {
        sync_before_pairing(&client(server), Ade::T3, "m1", desktop).await
    }

    const BUSY: &str = "T3 is busy, update deferred";

    #[tokio::test]
    async fn a_pairing_waits_for_a_busy_refresh_of_an_old_setup_with_dpop() {
        // At the desktop's version, and ahead of it.
        for (found, decision) in [
            ("t3 v0.1.12\n", Decision::Defer),
            ("t3 v0.1.13\n", Decision::Older),
        ] {
            let server = MockServer::start().await;
            exec_mock(&server, VERSION, found, "", 0, 1).await;
            exec_mock(&server, REFRESH, "", BUSY, 3, 1).await;
            let (out, pairing) = before_pairing(&server, "0.1.12").await;
            assert_eq!(out.unwrap().decision, decision, "{found}");
            assert_eq!(pairing, Err(SandboxError::AdeBusy(Ade::T3)), "{found}");
        }
    }

    #[tokio::test]
    async fn a_pairing_waits_for_a_busy_update_of_an_old_setup_with_dpop() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "t3 v0.1.12\n", "", 0, 1).await;
        exec_mock(&server, PROBE, "{\"busy\":true}\n", "", 0, 1).await;
        let (out, pairing) = before_pairing(&server, "0.1.13").await;
        assert_eq!(out.unwrap().decision, Decision::Defer);
        assert_eq!(pairing, Err(SandboxError::AdeBusy(Ade::T3)));
    }

    #[tokio::test]
    async fn a_pairing_waits_on_a_failed_refresh_of_an_old_setup_with_dpop() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "t3 v0.1.12\n", "", 0, 1).await;
        exec_mock(&server, REFRESH, "", "T3 server did not start", 1, 1).await;
        let (out, pairing) = before_pairing(&server, "0.1.12").await;
        let e = SandboxError::Setup {
            ade: Ade::T3,
            stderr_tail: "T3 server did not start".into(),
        };
        assert_eq!(out.unwrap_err(), e);
        assert_eq!(pairing, Err(e));
    }

    #[tokio::test]
    async fn a_pairing_goes_ahead_when_the_server_takes_it() {
        // A busy update of a sandbox that runs this build's setup already.
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, &current("0.1.12"), "", 0, 1).await;
        exec_mock(&server, PROBE, "{\"busy\":true}\n", "", 0, 1).await;
        let (out, pairing) = before_pairing(&server, "0.1.13").await;
        assert_eq!((out.unwrap().decision, pairing), (Decision::Defer, Ok(())));
        // A busy refresh where neither side uses DPoP (T3 Code 0.1.10).
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "t3 v0.1.10\n", "", 0, 1).await;
        exec_mock(&server, REFRESH, "", BUSY, 3, 1).await;
        let (out, pairing) = before_pairing(&server, "0.1.10").await;
        assert_eq!((out.unwrap().decision, pairing), (Decision::Defer, Ok(())));
        // A refresh that finished.
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "t3 v0.1.12\n", "", 0, 1).await;
        exec_mock(&server, REFRESH, "", "", 0, 1).await;
        let (out, pairing) = before_pairing(&server, "0.1.12").await;
        assert_eq!(
            (out.unwrap().decision, pairing),
            (Decision::UpToDate, Ok(()))
        );
        // A version script that failed: nothing known, so the pairing goes ahead as before.
        let server = MockServer::start().await;
        let (out, pairing) = before_pairing(&server, "0.1.12").await;
        assert!(out.is_err());
        assert_eq!(pairing, Ok(()));
    }

    #[tokio::test]
    async fn no_t3_and_an_unreadable_desktop_version_refresh_nothing() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "", "", 0, 1).await;
        exec_mock(&server, REFRESH, "", "", 0, 0).await;
        let d = sync(&client(&server), Ade::T3, "m1", "junk").await.unwrap();
        assert_eq!(d.decision, Decision::UpToDate);
    }

    #[tokio::test]
    async fn older_and_idle_runs_setup() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "0.1.10\n", "", 0, 1).await;
        exec_mock(
            &server,
            PROBE,
            "{\"busy\":false,\"reason\":\"idle\"}\n",
            "",
            0,
            1,
        )
        .await;
        exec_mock(&server, SETUP, "", "", 0, 1).await;
        let d = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        assert_eq!(d.decision, Decision::Update);
    }

    #[tokio::test]
    async fn older_and_busy_defers_without_setup() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "0.1.10\n", "", 0, 1).await;
        exec_mock(
            &server,
            PROBE,
            "{\"busy\":true,\"reason\":\"run\"}\n",
            "",
            0,
            1,
        )
        .await;
        exec_mock(&server, SETUP, "", "", 0, 0).await;
        let d = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        assert_eq!(d.decision, Decision::Defer);
        assert_eq!(d.remote_version.as_deref(), Some("0.1.10"));
    }

    #[tokio::test]
    async fn setup_exit_busy_is_defer() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "0.1.10\n", "", 0, 1).await;
        exec_mock(&server, PROBE, "{\"busy\":false}\n", "", 0, 1).await;
        exec_mock(&server, SETUP, "", "T3 is busy, update deferred", 3, 1).await;
        let d = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        assert_eq!(d.decision, Decision::Defer);
    }

    #[tokio::test]
    async fn a_timed_out_setup_is_an_error_that_says_so() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "0.1.10\n", "", 0, 1).await;
        exec_mock(&server, PROBE, "{\"busy\":false}\n", "", 0, 1).await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains(SETUP))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"stdout": "", "stderr": "", "exitCode": 124, "timedOut": true}
            )))
            .expect(1)
            .mount(&server)
            .await;
        let e = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        assert_eq!(
            e,
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail: "Timed out after 600 s".into()
            }
        );
    }

    #[tokio::test]
    async fn setup_failure_is_an_error_with_stderr_tail() {
        let server = MockServer::start().await;
        exec_mock(&server, VERSION, "", "", 0, 1).await;
        exec_mock(&server, PROBE, "{\"busy\":false}\n", "", 0, 1).await;
        exec_mock(&server, SETUP, "", "line1\nboom", 1, 1).await;
        let e = sync(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        assert_eq!(
            e,
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail: "line1\nboom".into()
            }
        );
    }
}
