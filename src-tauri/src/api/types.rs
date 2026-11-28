//! Wire types for the instabox HTTP API. camelCase JSON, unknown fields ignored.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TemplateRef {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineSpec {
    pub vcpus: u32,
    #[serde(rename = "memMiB")]
    pub mem_mib: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub state: String,
    /// What the sandbox is headed for: `suspended` or `stopped` while a pause or stop is queued
    /// or under way.
    #[serde(default)]
    pub desired_state: Option<String>,
    /// Moves on with every lifecycle change (a pause, a resume).
    #[serde(default)]
    pub revision: Option<i64>,
    pub template: TemplateRef,
    pub spec: MachineSpec,
    #[serde(default)]
    pub auto_suspend_after_seconds: Option<i64>,
    #[serde(default)]
    pub max_run_minutes: i64,
    #[serde(default)]
    pub run_started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cooldown_until: Option<DateTime<Utc>>,
    /// When instabox paused the sandbox for idleness; none when its owner paused it.
    #[serde(default)]
    pub auto_suspended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub org_id: String,
    #[serde(default)]
    pub free: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

/// Error object as the API returns it, inside `{"error": ...}` and in a failed operation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub retry_after_seconds: Option<u64>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    #[serde(default)]
    pub machine_id: String,
    #[serde(rename = "type", default)]
    pub r#type: String,
    pub state: String,
    #[serde(default)]
    pub error: Option<ApiErrorBody>,
    #[serde(default)]
    pub machine: Option<Machine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRef {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tier: String,
    /// "personal" or "workspace".
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub always_on_allowed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub user_id: i64,
    pub login: String,
    #[serde(default)]
    pub allowed_specs: Vec<MachineSpec>,
    #[serde(default)]
    pub org: Option<OrgRef>,
    #[serde(default)]
    pub fixed_auto_suspend_seconds: Option<i64>,
    #[serde(default)]
    pub idle_choices_seconds: Vec<i64>,
    #[serde(default)]
    pub always_on_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Org {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub personal: bool,
    #[serde(default)]
    pub allowed_specs: Vec<MachineSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Template {
    pub id: String,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub creatable: bool,
    #[serde(default)]
    pub latest: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    pub template: TemplateRef,
    pub status: String,
    /// The org that owns the snapshot, when the API says. Unknown shows in every org.
    #[serde(default, rename = "orgId")]
    pub org_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMachineRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u32>,
    #[serde(rename = "memMiB", skip_serializing_if = "Option::is_none")]
    pub mem_mib: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_suspend_after_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMachineRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u32>,
    #[serde(rename = "memMiB", skip_serializing_if = "Option::is_none")]
    pub mem_mib: Option<u32>,
    /// Replaces the whole map. Read, change, write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_suspend_after_seconds: Option<i64>,
    /// `"restart"` applies a resize now and answers with an operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecRequest {
    pub cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    pub exit_code: i32,
    #[serde(default)]
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PublicPort {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshToken {
    pub host: String,
    pub port: u16,
    pub token: String,
    pub command: String,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub host_key_fingerprint: Option<String>,
    /// A background token (`ssh::Kind::Background`). False in the reply of a server that does
    /// not know background tokens, which made an ordinary one.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CreatedApiKey {
    pub key: String,
}

/// Sign-in session tokens. Stored in the keychain as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub access_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_token: String,
    pub refresh_expires_at: DateTime<Utc>,
}

/// Answer to `POST /v1/auth/device`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    /// Seconds between polls.
    pub interval: u64,
    /// Seconds until the device code expires.
    pub expires_in: u64,
}

impl DeviceStart {
    /// The URL to open in the browser.
    pub fn browser_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or(&self.verification_uri)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimits {
    #[serde(default)]
    pub max_machines: Option<i64>,
}

/// Answer to `GET /v1/usage`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Usage {
    #[serde(default)]
    pub limits: UsageLimits,
    #[serde(default)]
    pub org: Option<OrgRef>,
}
