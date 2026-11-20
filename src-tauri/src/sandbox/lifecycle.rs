//! Sandbox actions behind the UI's buttons: pause, resume, delete, resize,
//! auto sleep and reinstall the ADE. Each mutating call uses one Idempotency-Key.

use crate::ade::{Ade, SETUP_EXIT_BUSY, parse_probe};
use crate::api::{Action, Client, ExecRequest, PatchResult, UpdateMachineRequest};
use crate::sandbox::connect::{SETUP_TIMEOUT_SECS, setup_failure};
use crate::sandbox::sync::{SyncOutcome, sync};
use crate::sandbox::view::marked_ade;
use crate::sandbox::{
    SandboxError, WAKE_WAIT, cooldown_or_api, ensure_running, new_key, retry_transient, wait_op,
};

const FREE_AUTO_SLEEP_SECS: i64 = 1800;
const QUERY_TIMEOUT_SECS: u32 = 30;
const STOP_TIMEOUT_SECS: u32 = 60;

/// Suspends the sandbox and waits until it is suspended.
pub async fn pause(client: &Client, id: &str) -> Result<(), SandboxError> {
    let key = new_key();
    let op = match retry_transient(|| client.action(id, Action::Suspend, &key)).await {
        Ok(op) => op,
        Err(e) => return Err(map_cooldown(client, id, e).await),
    };
    wait_op(client, &op, WAKE_WAIT).await?;
    Ok(())
}

/// Turns a 409 `machine_cooldown` into `Cooldown`, re-reading the machine so the
/// minutes come from its `cooldownUntil`. Any other error becomes `Api`.
async fn map_cooldown(client: &Client, id: &str, e: crate::api::Error) -> SandboxError {
    let machine = if e.code == "machine_cooldown" {
        client.machine(id).await.ok()
    } else {
        None
    };
    cooldown_or_api(e, machine.as_ref())
}

/// Wakes (or starts) the sandbox. When it is connected to `ade`, also brings the ADE in step
/// with the desktop and returns that decision. `None` means not connected to `ade`.
pub async fn resume(
    client: &Client,
    ade: Ade,
    id: &str,
    desktop_version: &str,
) -> Result<Option<SyncOutcome>, SandboxError> {
    let machine = ensure_running(client, id).await?;
    if marked_ade(&machine.metadata) != Some(ade) {
        return Ok(None);
    }
    sync(client, ade, id, desktop_version).await.map(Some)
}

/// Deletes the sandbox and waits for the operation. The client sends X-Confirm-Delete.
pub async fn delete(client: &Client, id: &str) -> Result<(), SandboxError> {
    let key = new_key();
    let op = match retry_transient(|| client.delete(id, &key)).await {
        Ok(op) => op,
        Err(e) => return Err(map_cooldown(client, id, e).await),
    };
    match wait_op(client, &op, WAKE_WAIT).await {
        // A finished delete may leave no machine to read back: a 404 means it is gone.
        Err(SandboxError::Api(e)) if e.status == 404 => Ok(()),
        other => other.map(|_| ()),
    }
}

/// Changes vCPUs and memory, restarting the sandbox now. Waits for the operation.
pub async fn resize(
    client: &Client,
    id: &str,
    vcpus: u32,
    mem_mib: u32,
) -> Result<(), SandboxError> {
    let req = UpdateMachineRequest {
        vcpus: Some(vcpus),
        mem_mib: Some(mem_mib),
        apply: Some("restart".to_string()),
        ..Default::default()
    };
    patch_and_wait(client, id, &req).await
}

/// Sets the idle auto sleep in seconds (0 = off). A free sandbox is fixed at 30
/// minutes, so any other value is refused here without a request.
pub async fn set_auto_sleep(client: &Client, id: &str, seconds: i64) -> Result<(), SandboxError> {
    let machine = client.machine(id).await?;
    if machine.free && seconds != FREE_AUTO_SLEEP_SECS {
        return Err(SandboxError::FixedOnFree);
    }
    let req = UpdateMachineRequest {
        auto_suspend_after_seconds: Some(seconds),
        ..Default::default()
    };
    patch_and_wait(client, id, &req).await
}

async fn patch_and_wait(
    client: &Client,
    id: &str,
    req: &UpdateMachineRequest,
) -> Result<(), SandboxError> {
    let key = new_key();
    match retry_transient(|| client.patch(id, req, &key)).await {
        Ok(PatchResult::Machine(_)) => Ok(()),
        Ok(PatchResult::Operation(op)) => wait_op(client, &op, WAKE_WAIT).await.map(|_| ()),
        Err(e) => Err(map_cooldown(client, id, e).await),
    }
}

async fn run(
    client: &Client,
    id: &str,
    cmd: &str,
    secs: u32,
) -> Result<crate::api::ExecResult, SandboxError> {
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

/// Runs the ADE's setup script again at `desktop_version`, whatever the sandbox has now.
/// Never while the ADE is busy. Wakes the sandbox first.
pub async fn reinstall(
    client: &Client,
    ade: Ade,
    id: &str,
    desktop_version: &str,
) -> Result<(), SandboxError> {
    ensure_running(client, id).await?;
    // Never move a sandbox to an older version, so this stops before the probe, stop and setup.
    let found = run(client, id, ade.version_script(), QUERY_TIMEOUT_SECS).await?;
    if let (Ok(desktop), Some(Ok(remote))) = (
        semver::Version::parse(desktop_version),
        ade.parse_version(&found.stdout)
            .map(|v| semver::Version::parse(&v)),
    ) && remote > desktop
    {
        return Err(SandboxError::NewerThanDesktop(ade));
    }
    let probe = run(client, id, &ade.probe().path(), QUERY_TIMEOUT_SECS).await?;
    if parse_probe(&probe.stdout).busy {
        return Err(SandboxError::AdeBusy(ade));
    }
    run(client, id, ade.stop_script(), STOP_TIMEOUT_SECS).await?;
    let setup = run(
        client,
        id,
        &ade.setup_script(desktop_version),
        SETUP_TIMEOUT_SECS,
    )
    .await?;
    match setup.exit_code {
        _ if setup.timed_out => Err(setup_failure(ade, &setup, SETUP_TIMEOUT_SECS)),
        0 => Ok(()),
        SETUP_EXIT_BUSY => Err(SandboxError::AdeBusy(ade)),
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
    use wiremock::matchers::{body_json, body_string_contains, header_exists, method, path};
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

    fn machine(state: &str, free: bool, metadata: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": "m1", "name": "n", "state": state, "revision": 1,
            "template": {"id": "agent-browser", "version": 1, "digest": "d"},
            "spec": {"vcpus": 2, "memMiB": 4096, "persistentDiskGiB": 10},
            "runtime": {"ready": true}, "autoSuspendAfterSeconds": 1800,
            "maxRunMinutes": -1, "runStartedAt": null, "cooldownUntil": null,
            "metadata": metadata, "orgId": "o1", "free": free, "operationId": null
        })
    }

    fn machine_resp(state: &str, free: bool, metadata: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(machine(state, free, metadata))
    }

    fn op_done() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "op1", "machineId": "m1", "type": "x", "state": "succeeded",
            "error": null, "machine": machine("running", false, serde_json::json!({}))
        }))
    }

    fn accepted() -> ResponseTemplate {
        ResponseTemplate::new(202).set_body_json(serde_json::json!({
            "id": "op1", "machineId": "m1", "type": "x", "state": "queued", "error": null
        }))
    }

    async fn mount(server: &MockServer, m: &str, p: &str, resp: ResponseTemplate) {
        Mock::given(method(m))
            .and(path(p))
            .respond_with(resp)
            .mount(server)
            .await;
    }

    async fn mount_machine(server: &MockServer, state: &str, free: bool, md: serde_json::Value) {
        mount(
            server,
            "GET",
            "/v1/machines/m1",
            machine_resp(state, free, md),
        )
        .await;
    }

    async fn polled_op(server: &MockServer) -> bool {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path() == "/v1/operations/op1")
    }

    /// A 409 cooldown on `verb path`, with a machine GET whose cooldownUntil is
    /// 11.5 minutes ahead (rounds up to 12 whatever the test's small delay).
    async fn cooldown_server(verb: &str, p: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method(verb))
            .and(path(p))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": {"code": "machine_cooldown", "message": "m",
                    "retryable": true, "retryAfterSeconds": 60}
            })))
            // A cooldown is not retried: one call, then Cooldown.
            .expect(1)
            .mount(&server)
            .await;
        let mut m = machine("suspended", false, serde_json::json!({}));
        m["cooldownUntil"] =
            serde_json::json!((chrono::Utc::now() + chrono::Duration::seconds(690)).to_rfc3339());
        mount(
            &server,
            "GET",
            "/v1/machines/m1",
            ResponseTemplate::new(200).set_body_json(m),
        )
        .await;
        server
    }

    #[tokio::test]
    async fn pause_cooldown_uses_machine_cooldown_until() {
        let server = cooldown_server("POST", "/v1/machines/m1/suspend").await;
        let e = pause(&client(&server), "m1").await.unwrap_err();
        assert_eq!(e, SandboxError::Cooldown { minutes: 12 });
    }

    #[tokio::test]
    async fn delete_cooldown_uses_machine_cooldown_until() {
        let server = cooldown_server("DELETE", "/v1/machines/m1").await;
        let e = delete(&client(&server), "m1").await.unwrap_err();
        assert_eq!(e, SandboxError::Cooldown { minutes: 12 });
    }

    #[tokio::test]
    async fn resize_cooldown_uses_machine_cooldown_until() {
        let server = cooldown_server("PATCH", "/v1/machines/m1").await;
        let e = resize(&client(&server), "m1", 2, 4096).await.unwrap_err();
        assert_eq!(e, SandboxError::Cooldown { minutes: 12 });
    }

    #[tokio::test]
    async fn pause_posts_suspend_with_key_and_waits() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/suspend"))
            .and(header_exists("Idempotency-Key"))
            .respond_with(accepted())
            .expect(1)
            .mount(&server)
            .await;
        mount(&server, "GET", "/v1/operations/op1", op_done()).await;
        pause(&client(&server), "m1").await.unwrap();
        assert!(polled_op(&server).await);
    }

    #[tokio::test]
    async fn delete_sends_confirm_and_key_and_waits() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/machines/m1"))
            .and(header_exists("Idempotency-Key"))
            .and(header_exists("X-Confirm-Delete"))
            .respond_with(accepted())
            .expect(1)
            .mount(&server)
            .await;
        mount(&server, "GET", "/v1/operations/op1", op_done()).await;
        delete(&client(&server), "m1").await.unwrap();
        assert!(polled_op(&server).await);
    }

    #[tokio::test]
    async fn delete_done_without_machine_is_ok_when_machine_is_gone() {
        let server = MockServer::start().await;
        mount(&server, "DELETE", "/v1/machines/m1", accepted()).await;
        mount(
            &server,
            "GET",
            "/v1/operations/op1",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "op1", "machineId": "m1", "type": "delete", "state": "succeeded",
                "error": null, "machine": null
            })),
        )
        .await;
        mount(
            &server,
            "GET",
            "/v1/machines/m1",
            ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": "not_found", "message": "machine not found", "retryable": false}
            })),
        )
        .await;
        delete(&client(&server), "m1").await.unwrap();
    }

    #[tokio::test]
    async fn delete_failed_operation_is_an_error() {
        let server = MockServer::start().await;
        mount(&server, "DELETE", "/v1/machines/m1", accepted()).await;
        mount(
            &server,
            "GET",
            "/v1/operations/op1",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "op1", "machineId": "m1", "type": "delete", "state": "failed",
                "error": {"code": "internal", "message": "boom", "retryable": false},
                "machine": null
            })),
        )
        .await;
        let e = delete(&client(&server), "m1").await.unwrap_err();
        assert!(matches!(e, SandboxError::Api(ref a) if a.code == "internal"));
    }

    #[tokio::test]
    async fn resize_patch_body_is_exact() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/machines/m1"))
            .and(header_exists("Idempotency-Key"))
            .and(body_json(
                serde_json::json!({"vcpus": 2, "memMiB": 4096, "apply": "restart"}),
            ))
            .respond_with(accepted())
            .expect(1)
            .mount(&server)
            .await;
        mount(&server, "GET", "/v1/operations/op1", op_done()).await;
        resize(&client(&server), "m1", 2, 4096).await.unwrap();
        assert!(polled_op(&server).await);
    }

    #[tokio::test]
    async fn free_auto_sleep_other_than_30_min_makes_no_patch() {
        let server = MockServer::start().await;
        mount_machine(&server, "running", true, serde_json::json!({})).await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let e = set_auto_sleep(&client(&server), "m1", 600)
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::FixedOnFree);
        assert_eq!(
            e.to_string(),
            "Auto sleep is fixed at 30 minutes on the free plan"
        );
    }

    #[tokio::test]
    async fn paid_auto_sleep_patches_seconds_and_zero_is_off() {
        for secs in [600, 0] {
            let server = MockServer::start().await;
            mount_machine(&server, "running", false, serde_json::json!({})).await;
            Mock::given(method("PATCH"))
                .and(path("/v1/machines/m1"))
                .and(header_exists("Idempotency-Key"))
                .and(body_json(
                    serde_json::json!({"autoSuspendAfterSeconds": secs}),
                ))
                .respond_with(machine_resp("running", false, serde_json::json!({})))
                .expect(1)
                .mount(&server)
                .await;
            set_auto_sleep(&client(&server), "m1", secs).await.unwrap();
        }
    }

    #[tokio::test]
    async fn free_auto_sleep_1800_is_sent() {
        let server = MockServer::start().await;
        mount_machine(&server, "running", true, serde_json::json!({})).await;
        Mock::given(method("PATCH"))
            .respond_with(machine_resp("running", true, serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        set_auto_sleep(&client(&server), "m1", 1800).await.unwrap();
    }

    async fn exec_mock(
        server: &MockServer,
        has: &str,
        stdout: &str,
        stderr: &str,
        code: i32,
        n: u64,
    ) {
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains(has))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"stdout": stdout, "stderr": stderr, "exitCode": code}
            )))
            .expect(n)
            .mount(server)
            .await;
    }

    const VERSION: &str = "t3 --version";
    const PROBE: &str = "\"cmd\":\"/usr/local/lib/instabox/activity.d/t3\"";
    const STOP: &str = "systemctl stop t3-instabox";
    const SETUP: &str = "v=0.1.11";

    #[tokio::test]
    async fn resume_connected_runs_version_check() {
        let server = MockServer::start().await;
        let md = serde_json::json!({"instabox-bridge": "t3"});
        mount_machine(&server, "suspended", false, md.clone()).await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/resume"))
            .and(header_exists("Idempotency-Key"))
            .respond_with(accepted())
            .expect(1)
            .mount(&server)
            .await;
        // The finished operation carries the connected machine.
        mount(
            &server,
            "GET",
            "/v1/operations/op1",
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "op1", "machineId": "m1", "type": "resume", "state": "succeeded",
                "error": null, "machine": machine("running", false, md)
            })),
        )
        .await;
        exec_mock(&server, VERSION, "0.1.11\n", "", 0, 1).await;
        let d = resume(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        assert_eq!(
            d.unwrap().decision,
            crate::sandbox::sync::Decision::UpToDate
        );
    }

    #[tokio::test]
    async fn resume_unconnected_makes_no_exec() {
        let server = MockServer::start().await;
        mount_machine(&server, "running", false, serde_json::json!({})).await;
        exec_mock(&server, "", "", "", 0, 0).await;
        let d = resume(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        assert_eq!(d, None);
    }

    #[tokio::test]
    async fn reinstall_busy_probe_makes_no_stop_or_setup() {
        let server = MockServer::start().await;
        mount_machine(&server, "running", false, serde_json::json!({})).await;
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
        exec_mock(&server, STOP, "", "", 0, 0).await;
        exec_mock(&server, SETUP, "", "", 0, 0).await;
        let e = reinstall(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::AdeBusy(Ade::T3));
    }

    #[tokio::test]
    async fn reinstall_unparsable_probe_is_busy() {
        let server = MockServer::start().await;
        mount_machine(&server, "running", false, serde_json::json!({})).await;
        exec_mock(&server, VERSION, "", "", 0, 1).await;
        exec_mock(&server, PROBE, "garbage", "", 0, 1).await;
        exec_mock(&server, STOP, "", "", 0, 0).await;
        exec_mock(&server, SETUP, "", "", 0, 0).await;
        let e = reinstall(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::AdeBusy(Ade::T3));
    }

    async fn reinstall_server(setup_code: i32, stderr: &str) -> MockServer {
        let server = MockServer::start().await;
        mount_machine(&server, "running", false, serde_json::json!({})).await;
        exec_mock(&server, VERSION, "0.1.10\n", "", 0, 1).await;
        exec_mock(&server, PROBE, "{\"busy\":false}\n", "", 0, 1).await;
        exec_mock(&server, STOP, "", "", 0, 1).await;
        exec_mock(&server, SETUP, "", stderr, setup_code, 1).await;
        server
    }

    #[tokio::test]
    async fn reinstall_stops_then_runs_setup_in_order() {
        let server = reinstall_server(0, "").await;
        reinstall(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap();
        let bodies: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/exec"))
            .map(|r| String::from_utf8_lossy(&r.body).to_string())
            .collect();
        assert_eq!(bodies.len(), 4);
        assert!(bodies[0].contains(VERSION));
        assert!(bodies[1].contains("activity.d/t3"));
        assert!(bodies[2].contains(STOP) && bodies[2].contains("reset-failed"));
        assert!(bodies[2].contains("\"timeoutSeconds\":60"));
        assert!(bodies[3].contains(SETUP) && bodies[3].contains("\"timeoutSeconds\":600"));
    }

    #[tokio::test]
    async fn reinstall_refuses_a_sandbox_with_a_newer_t3() {
        let server = MockServer::start().await;
        mount_machine(&server, "running", false, serde_json::json!({})).await;
        exec_mock(&server, VERSION, "t3 v0.1.12\n", "", 0, 1).await;
        exec_mock(&server, PROBE, "", "", 0, 0).await;
        exec_mock(&server, STOP, "", "", 0, 0).await;
        exec_mock(&server, SETUP, "", "", 0, 0).await;
        let e = reinstall(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::NewerThanDesktop(Ade::T3));
        assert_eq!(
            e.to_string(),
            "This sandbox runs a newer T3 than this computer. Update T3 Code here first."
        );
        drop(server);
    }

    #[tokio::test]
    async fn reinstall_setup_busy_exit_is_t3_busy() {
        let server = reinstall_server(3, "busy").await;
        let e = reinstall(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        assert_eq!(e, SandboxError::AdeBusy(Ade::T3));
    }

    #[tokio::test]
    async fn reinstall_timed_out_setup_says_so() {
        let server = MockServer::start().await;
        mount_machine(&server, "running", false, serde_json::json!({})).await;
        exec_mock(&server, VERSION, "0.1.10\n", "", 0, 1).await;
        exec_mock(&server, PROBE, "{\"busy\":false}\n", "", 0, 1).await;
        exec_mock(&server, STOP, "", "", 0, 1).await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains(SETUP))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"stdout": "", "stderr": "curl: (28) slow", "exitCode": 124, "timedOut": true}
            )))
            .expect(1)
            .mount(&server)
            .await;
        let e = reinstall(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        assert_eq!(
            e,
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail: "Timed out after 600 s\ncurl: (28) slow".into()
            }
        );
    }

    #[tokio::test]
    async fn reinstall_setup_failure_carries_last_20_stderr_lines() {
        let stderr = (1..=30)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let server = reinstall_server(1, &stderr).await;
        let e = reinstall(&client(&server), Ade::T3, "m1", "0.1.11")
            .await
            .unwrap_err();
        let SandboxError::Setup {
            ade: Ade::T3,
            stderr_tail,
        } = e
        else {
            panic!("wrong error");
        };
        assert_eq!(stderr_tail.lines().count(), 20);
        assert!(stderr_tail.starts_with("l11") && stderr_tail.ends_with("l30"));
    }
}
