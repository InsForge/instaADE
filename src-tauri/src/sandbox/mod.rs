pub mod connect;
pub mod lifecycle;
pub mod sync;
pub mod view;

use std::fmt;
use std::time::Duration;

use chrono::Utc;
use tokio::time::Instant;

use crate::ade::Ade;
use crate::api::{self, Action, Client, Machine, Operation};

/// Why a create, wake or connect did not finish. `Display` is written for people.
#[derive(Debug, Clone, PartialEq)]
pub enum SandboxError {
    Cooldown {
        minutes: i64,
    },
    FreeLimit,
    /// The ADE is busy on the sandbox, so its setup waits.
    AdeBusy(Ade),
    Setup {
        ade: Ade,
        stderr_tail: String,
    },
    Timeout,
    FixedOnFree,
    /// The sandbox runs a newer version of the ADE than the desktop; the app never moves it
    /// back.
    NewerThanDesktop(Ade),
    /// The ADE installs nothing on the sandbox, so there is nothing to set up again.
    NoReinstall(Ade),
    Api(api::Error),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxError::Cooldown { minutes } => {
                write!(f, "Cooling down, ready in about {minutes} min")
            }
            SandboxError::FreeLimit => write!(f, "The free plan has one sandbox"),
            SandboxError::AdeBusy(ade) => write!(
                f,
                "{} is busy on this sandbox. Try again when the agent is idle.",
                ade.short_name()
            ),
            SandboxError::Setup { ade, stderr_tail } => {
                write!(f, "Setting up {} failed:\n{stderr_tail}", ade.short_name())
            }
            SandboxError::Timeout => write!(f, "The sandbox took too long to get ready"),
            SandboxError::FixedOnFree => {
                write!(f, "Auto sleep is fixed at 30 minutes on the free plan")
            }
            SandboxError::NewerThanDesktop(ade) => write!(
                f,
                "This sandbox runs a newer {} than this computer. Update {} here first.",
                ade.short_name(),
                ade.name()
            ),
            SandboxError::NoReinstall(ade) => {
                write!(
                    f,
                    "{} installs nothing on the sandbox to reinstall",
                    ade.name()
                )
            }
            SandboxError::Api(e) => write!(f, "{}", e.message),
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<api::Error> for SandboxError {
    fn from(e: api::Error) -> Self {
        SandboxError::Api(e)
    }
}

// Short under test so retry tests do not wait.
const RETRY_PAUSE: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_secs(2)
};
const MUTATE_ATTEMPTS: usize = 3;
pub const CREATE_WAIT: Duration = Duration::from_secs(600);
pub const WAKE_WAIT: Duration = Duration::from_secs(300);

pub(crate) fn new_key() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn local_err(code: &str, message: String) -> SandboxError {
    SandboxError::Api(api::Error {
        status: 0,
        code: code.to_string(),
        message,
        retryable: false,
        retry_after: None,
    })
}

/// True for failures worth waiting out: a network error or a retryable HTTP error.
/// A failed operation (status 0, not a network error) is final, and so is a cooldown: the
/// API marks it retryable, but it lasts minutes and maps to `Cooldown` at once.
fn is_transient(e: &api::Error) -> bool {
    if e.code == "machine_cooldown" {
        return false;
    }
    e.code == "network" || (e.status != 0 && (e.retryable || e.status >= 500))
}

fn minutes_from_seconds(secs: i64) -> i64 {
    ((secs + 59) / 60).max(1)
}

/// Runs a mutating call up to 3 times, 2 s apart, on a transient error. The closure
/// must reuse the same Idempotency-Key on every attempt. Other errors return at once.
pub(crate) async fn retry_transient<T, F, Fut>(mut call: F) -> api::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = api::Result<T>>,
{
    let mut attempt = 1;
    loop {
        match call().await {
            Err(e) if attempt < MUTATE_ATTEMPTS && is_transient(&e) => {
                attempt += 1;
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            other => return other,
        }
    }
}

/// Waits for `op`. Retries the same operation every 2 s on a transient error
/// and gives up with `Timeout` after `overall`.
pub async fn wait_op(
    client: &Client,
    op: &Operation,
    overall: Duration,
) -> Result<Machine, SandboxError> {
    let deadline = Instant::now() + overall;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, client.wait(op)).await {
            Err(_) => return Err(SandboxError::Timeout),
            Ok(Ok(m)) => return Ok(m),
            Ok(Err(e)) if is_transient(&e) => {
                if Instant::now() + RETRY_PAUSE >= deadline {
                    return Err(SandboxError::Timeout);
                }
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            Ok(Err(e)) => return Err(cooldown_or_api(e, None)),
        }
    }
}

/// A 409 `machine_cooldown` becomes `Cooldown`. Minutes come from the machine's
/// `cooldownUntil`, else from the error's retry-after, rounded up.
fn cooldown_or_api(e: api::Error, machine: Option<&Machine>) -> SandboxError {
    if e.code != "machine_cooldown" {
        return SandboxError::Api(e);
    }
    let secs = machine
        .and_then(|m| m.cooldown_until)
        .map(|t| (t - Utc::now()).num_seconds())
        .filter(|s| *s > 0)
        .or(e.retry_after.map(|s| s as i64));
    SandboxError::Cooldown {
        minutes: secs.map_or(1, minutes_from_seconds),
    }
}

/// Polls the machine every 2 s until it is running.
async fn wait_state_running(client: &Client, id: &str) -> Result<Machine, SandboxError> {
    let deadline = Instant::now() + WAKE_WAIT;
    loop {
        let m = client.machine(id).await?;
        match m.state.as_str() {
            "running" => return Ok(m),
            "creating" | "starting" | "resuming" => {}
            other => {
                return Err(local_err(
                    "machine_not_running",
                    format!("The sandbox is {other}"),
                ));
            }
        }
        if Instant::now() + RETRY_PAUSE >= deadline {
            return Err(SandboxError::Timeout);
        }
        tokio::time::sleep(RETRY_PAUSE).await;
    }
}

/// Returns the machine once it is running. Wakes a suspended machine, starts a
/// stopped one, and waits out one that is already on its way up.
pub async fn ensure_running(client: &Client, id: &str) -> Result<Machine, SandboxError> {
    let machine = client.machine(id).await?;
    let action = match machine.state.as_str() {
        "running" => return Ok(machine),
        "suspended" | "suspending" => Action::Resume,
        "stopped" | "stopping" => Action::Start,
        "creating" | "starting" | "resuming" => return wait_state_running(client, id).await,
        other => {
            return Err(local_err(
                "machine_not_running",
                format!("The sandbox is {other}"),
            ));
        }
    };
    let key = new_key();
    let op = match retry_transient(|| client.action(id, action, &key)).await {
        Ok(op) => op,
        Err(e) if e.code == "machine_cooldown" => {
            let fresh = client.machine(id).await.ok();
            return Err(cooldown_or_api(e, fresh.as_ref().or(Some(&machine))));
        }
        Err(e) => return Err(e.into()),
    };
    wait_op(client, &op, WAKE_WAIT).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(status: u16, code: &str, retryable: bool) -> api::Error {
        api::Error {
            status,
            code: code.into(),
            message: "m".into(),
            retryable,
            retry_after: Some(60),
        }
    }

    #[test]
    fn t3_messages_read_word_for_word() {
        assert_eq!(
            SandboxError::AdeBusy(Ade::T3).to_string(),
            "T3 is busy on this sandbox. Try again when the agent is idle."
        );
        assert_eq!(
            SandboxError::Setup {
                ade: Ade::T3,
                stderr_tail: "boom".into()
            }
            .to_string(),
            "Setting up T3 failed:\nboom"
        );
        assert_eq!(
            SandboxError::NewerThanDesktop(Ade::T3).to_string(),
            "This sandbox runs a newer T3 than this computer. Update T3 Code here first."
        );
    }

    #[test]
    fn a_cooldown_is_never_transient() {
        assert!(!is_transient(&err(409, "machine_cooldown", true)));
        assert!(is_transient(&err(503, "no_capacity", false)));
        assert!(is_transient(&err(429, "rate_limited", true)));
        assert!(is_transient(&err(0, "network", true)));
        assert!(!is_transient(&err(0, "operation_failed", false)));
        assert!(!is_transient(&err(400, "bad_request", false)));
    }
}
