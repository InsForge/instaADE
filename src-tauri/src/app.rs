//! The app's state and the logic around it.
//!
//! Plain functions decide what the UI shows (org filtering, plan, sizes, sleep choices,
//! which sandboxes to sync). `Core` holds the state, talks to the API and the other
//! modules, and reports every change through one emit callback. The Tauri glue stays in
//! `lib.rs`, `commands.rs` and `tray.rs`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::agents::{self, Agent};
use crate::api::{Client, Credentials, Machine, MachineSpec, Me, Org, Snapshot, Template};
use crate::auth::{self, Provider, Session};
use crate::connected::ConnectedStore;
use crate::errors::ErrorView;
use crate::mcp::{self, KeyStore, McpClient};
use crate::sandbox::connect::{Connected, NewSandbox, Source, Step};
use crate::sandbox::sync::{Decision, SyncOutcome};
use crate::sandbox::view::{
    self, SandboxView, SizeChoice, SleepChoice, ViewState, is_linux_template,
};
use crate::sandbox::{self, SandboxError, connect, lifecycle};
use crate::ssh::{self, SshTokenStore};
use crate::t3::desktop;

const DEFAULT_API: &str = "https://api.instabox.sh";
const RELEASES_API: &str = "https://api.github.com/repos/InsForge/instabox-bridge/releases/latest";
/// Opens the T3 Code app without a pairing link.
const T3_APP_URL: &str = "t3code://";
const RELEASES_PAGE: &str = "https://github.com/InsForge/instabox-bridge/releases/latest";
const VISIBLE_REFRESH: Duration = Duration::from_secs(5);
const HIDDEN_REFRESH: Duration = Duration::from_secs(30);
/// Account data and the desktop T3 version are read this often.
pub const FULL_REFRESH: Duration = Duration::from_secs(300);

/// Called with the whole state after every change.
pub type Emitter = Arc<dyn Fn(&AppState) + Send + Sync>;
/// Opens a URL in the default handler (browser, or T3 Code for `t3code://`).
pub type Opener = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgView {
    pub id: String,
    pub name: String,
    pub tier: String,
    pub personal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotView {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    /// `None`: the `claude` CLI is not installed.
    pub claude_code: Option<bool>,
    pub codex: Option<bool>,
}

/// Where a create or connect is. `kind` is `create` (a new sandbox) or `connect` (one that
/// exists). `step` is one of `connecting`, `creating`, `waking`, `installing`, `pairing`,
/// `done`. `machine_id` is set once the sandbox exists, so the UI's Retry can call
/// `connect_sandbox` with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressView {
    pub kind: String,
    pub step: String,
    pub machine_id: Option<String>,
    pub error: Option<ErrorView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub signed_in: bool,
    pub login: Option<String>,
    /// The code to match on the sign-in page, while a sign-in waits.
    pub sign_in_code: Option<String>,
    pub orgs: Vec<OrgView>,
    pub org_id: Option<String>,
    pub tier: String,
    /// The desktop T3 Code's version.
    pub t3: Option<String>,
    pub sandboxes: Vec<SandboxView>,
    pub sizes: Vec<SizeChoice>,
    pub sleep_choices: Vec<SleepChoice>,
    pub snapshots: Vec<SnapshotView>,
    pub mcp: McpStatus,
    pub progress: Option<ProgressView>,
    pub error: Option<ErrorView>,
    pub start_at_login: bool,
    pub update: Option<UpdateInfo>,
}

/// What the last refresh read from the API and this computer.
#[derive(Debug, Clone, Default)]
pub struct Data {
    pub me: Option<Me>,
    pub orgs: Vec<Org>,
    pub templates: Vec<Template>,
    pub machines: Vec<Machine>,
    pub snapshots: Vec<Snapshot>,
    pub desktop_running: bool,
    /// The T3 version this app last saw on each sandbox.
    pub t3_versions: HashMap<String, String>,
    /// Sandboxes whose T3 is behind the desktop's and could not be updated (busy or failed).
    pub update_pending: HashSet<String>,
    /// Sandboxes whose T3 is newer than the desktop's: this computer's T3 Code is behind.
    pub desktop_behind: HashSet<String>,
    /// Sandboxes the signed-in user connected from this computer. Only these get commands
    /// without a user action.
    pub connected: HashSet<String>,
    /// The user `connected` was loaded for. `None` until the account is known.
    pub connected_user: Option<i64>,
}

impl Data {
    /// The sandbox runs `version` now.
    pub fn set_version(&mut self, id: &str, version: &str) {
        self.t3_versions.insert(id.to_string(), version.to_string());
        self.update_pending.remove(id);
        self.desktop_behind.remove(id);
    }

    /// The user whose connected set applies: the one it was loaded for, else the account's.
    pub fn user(&self) -> Option<i64> {
        self.connected_user
            .or_else(|| self.me.as_ref().map(|m| m.user_id))
    }

    /// Takes the connected set read from disk for `user`. A set already loaded for another
    /// user is replaced; ids remembered before the first load are kept.
    pub fn use_connected(&mut self, user: i64, set: HashSet<String>) {
        if self.connected_user == Some(user) {
            return;
        }
        if self.connected_user.is_some() {
            self.connected.clear();
        }
        self.connected.extend(set);
        self.connected_user = Some(user);
    }

    /// Records a sync. `None` is a sync that failed. No retry follows: each one is an exec.
    pub fn record_sync(&mut self, id: &str, desktop: &str, outcome: Option<&SyncOutcome>) {
        let Some(o) = outcome else {
            self.update_pending.insert(id.to_string());
            return;
        };
        match o.decision {
            Decision::Update => self.set_version(id, desktop),
            Decision::UpToDate => {
                let v = o.remote_version.as_deref().unwrap_or(desktop).to_string();
                self.set_version(id, &v);
            }
            Decision::Defer => {
                if let Some(v) = &o.remote_version {
                    self.t3_versions.insert(id.to_string(), v.clone());
                }
                self.desktop_behind.remove(id);
                self.update_pending.insert(id.to_string());
            }
            // The sandbox is ahead: never move it back, and do not offer [Update].
            Decision::Older => {
                if let Some(v) = &o.remote_version {
                    self.t3_versions.insert(id.to_string(), v.clone());
                }
                self.update_pending.remove(id);
                self.desktop_behind.insert(id.to_string());
            }
        }
    }
}

// ---- pure logic -----------------------------------------------------------------------

/// The API base: `INSTABOX_API_URL` when it is a valid URL, else production.
pub fn api_base(env: Option<String>) -> Url {
    env.and_then(|v| Url::parse(v.trim()).ok())
        .unwrap_or_else(|| Url::parse(DEFAULT_API).expect("default API URL is valid"))
}

/// How long to wait between machine refreshes.
pub fn refresh_interval(window_visible: bool) -> Duration {
    if window_visible {
        VISIBLE_REFRESH
    } else {
        HIDDEN_REFRESH
    }
}

/// The orgs to show: `/v1/orgs`, or the one org in `/v1/me` when that list is empty.
pub fn effective_orgs(me: &Me, orgs: &[Org]) -> Vec<Org> {
    if !orgs.is_empty() {
        return orgs.to_vec();
    }
    me.org
        .iter()
        .map(|o| Org {
            id: o.id.clone(),
            name: o.name.clone().unwrap_or_default(),
            tier: o.tier.clone(),
            kind: o.kind.clone(),
            personal: o.kind == "personal",
            allowed_specs: Vec::new(),
        })
        .collect()
}

/// Keeps `current` while it is still an org of the user; else the personal org from `me`.
pub fn resolve_org_id(current: Option<&str>, me: &Me, orgs: &[Org]) -> Option<String> {
    if let Some(c) = current
        && orgs.iter().any(|o| o.id == c)
    {
        return Some(c.to_string());
    }
    me.org
        .as_ref()
        .map(|o| o.id.clone())
        .or_else(|| orgs.first().map(|o| o.id.clone()))
}

/// Sizes the selected org allows: its own list, else the user's.
pub fn sizes_for(orgs: &[Org], org_id: Option<&str>, me: &Me) -> Vec<SizeChoice> {
    let own = orgs
        .iter()
        .find(|o| Some(o.id.as_str()) == org_id)
        .map(|o| o.allowed_specs.as_slice())
        .filter(|s| !s.is_empty());
    view::size_choices(own.unwrap_or(&me.allowed_specs))
}

fn template_is_linux(templates: &[Template], id: &str) -> bool {
    templates
        .iter()
        .find(|t| t.id == id)
        .is_none_or(is_linux_template)
}

/// Machines of the selected org on a Linux template. An unknown template shows.
pub fn visible_machines<'a>(
    machines: &'a [Machine],
    templates: &[Template],
    org_id: Option<&str>,
) -> Vec<&'a Machine> {
    machines
        .iter()
        .filter(|m| org_id.is_none_or(|o| m.org_id == o))
        .filter(|m| template_is_linux(templates, &m.template.id))
        .collect()
}

/// The sandbox rows. `T3 Code connected` needs a connected, running sandbox and a
/// running desktop app.
pub fn sandbox_views(d: &Data, org_id: Option<&str>, now: DateTime<Utc>) -> Vec<SandboxView> {
    visible_machines(&d.machines, &d.templates, org_id)
        .into_iter()
        .map(|m| {
            let mut v = view::view(m, now);
            v.t3_version = d.t3_versions.get(&m.id).cloned();
            v.update_pending = d.update_pending.contains(&m.id);
            v.desktop_behind = d.desktop_behind.contains(&m.id);
            v.connected = d.connected.contains(&m.id);
            v.t3_code_connected = v.connected && v.state == ViewState::Running && d.desktop_running;
            v
        })
        .collect()
}

/// Ready snapshots of a Linux template in the selected org. Unknown template or org shows.
pub fn snapshot_views(
    snapshots: &[Snapshot],
    templates: &[Template],
    org_id: Option<&str>,
) -> Vec<SnapshotView> {
    snapshots
        .iter()
        .filter(|s| s.status == "ready")
        .filter(|s| template_is_linux(templates, &s.template.id))
        .filter(|s| match (&s.org_id, org_id) {
            (Some(own), Some(sel)) => own == sel,
            _ => true,
        })
        .map(|s| SnapshotView {
            id: s.id.clone(),
            name: s.name.clone(),
        })
        .collect()
}

/// True when the desktop T3 version moved from one known version to another.
/// The first read is not a change.
pub fn version_changed(prev: Option<&str>, new: Option<&str>) -> bool {
    matches!((prev, new), (Some(p), Some(n)) if p != n)
}

/// The sandboxes to sync after a desktop version change: running and connected from this
/// computer, in any of the user's orgs. A sandbox someone else connected is left alone.
pub fn sync_targets(d: &Data) -> Vec<String> {
    d.machines
        .iter()
        .filter(|m| view::view_state(&m.state) == ViewState::Running)
        .filter(|m| d.connected.contains(&m.id))
        .filter(|m| template_is_linux(&d.templates, &m.template.id))
        .map(|m| m.id.clone())
        .collect()
}

/// The MCP status shown in settings.
pub fn mcp_status(
    claude_installed: bool,
    claude_configured: bool,
    codex_configured: bool,
) -> McpStatus {
    McpStatus {
        claude_code: claude_installed.then_some(claude_configured),
        codex: Some(codex_configured),
    }
}

/// The progress after `step`. The kind and, once known, the machine id stay.
pub fn progress_after(prev: &ProgressView, step: &Step) -> ProgressView {
    let machine_id = prev.machine_id.clone();
    let (name, machine_id) = match step {
        Step::Creating => ("creating", machine_id),
        Step::Created(id) => ("creating", Some(id.clone())),
        Step::Waking => ("waking", machine_id),
        Step::Installing(_) => ("installing", machine_id),
        Step::Pairing => ("pairing", machine_id),
        Step::Done => ("done", machine_id),
    };
    ProgressView {
        kind: prev.kind.clone(),
        step: name.to_string(),
        machine_id,
        error: None,
    }
}

/// A failed progress ends once its sandbox is gone (deleted here or elsewhere), so its Retry
/// can never point at a sandbox that no longer exists. A running one is left to its flow.
pub fn prune_progress(p: Option<ProgressView>, machines: &[Machine]) -> Option<ProgressView> {
    let listed = |id: &str| {
        machines
            .iter()
            .any(|m| m.id == id && !matches!(m.state.as_str(), "deleting" | "deleted"))
    };
    p.filter(|p| match (&p.error, &p.machine_id) {
        (Some(_), Some(id)) => listed(id),
        _ => true,
    })
}

/// A failed connect. With a machine id the progress stays, with the error, so Retry can
/// connect that sandbox. Without one there is nothing to retry and the progress ends.
pub fn fail_progress(prev: Option<&ProgressView>, error: &ErrorView) -> Option<ProgressView> {
    prev.filter(|p| p.machine_id.is_some())
        .map(|p| ProgressView {
            error: Some(error.clone()),
            ..p.clone()
        })
}

/// The release in a GitHub `releases/latest` body, when it is newer than `current`.
pub fn newer_release(current: &str, body: &str) -> Option<UpdateInfo> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        #[serde(default)]
        html_url: String,
    }
    let r: Release = serde_json::from_str(body).ok()?;
    let tag = r.tag_name.trim_start_matches('v');
    let newer = semver::Version::parse(tag).ok()? > semver::Version::parse(current).ok()?;
    newer.then(|| UpdateInfo {
        version: tag.to_string(),
        url: if r.html_url.starts_with("https://github.com/") {
            r.html_url
        } else {
            RELEASES_PAGE.to_string()
        },
    })
}

/// URLs the UI may ask the app to open.
pub fn allowed_url(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "https" | "t3code"))
}

pub fn parse_provider(s: &str) -> Option<Provider> {
    match s {
        "github" => Some(Provider::GitHub),
        "google" => Some(Provider::Google),
        _ => None,
    }
}

pub fn parse_mcp_client(s: &str) -> Option<McpClient> {
    match s {
        "claudeCode" => Some(McpClient::ClaudeCode),
        "codex" => Some(McpClient::Codex),
        _ => None,
    }
}

impl AppState {
    /// Fills the account and sandbox fields from `d`. Keeps the selected org while it is valid.
    pub fn apply(&mut self, d: &Data, now: DateTime<Utc>) {
        let Some(me) = &d.me else { return };
        let orgs = effective_orgs(me, &d.orgs);
        let org_id = resolve_org_id(self.org_id.as_deref(), me, &orgs);
        let tier = orgs
            .iter()
            .find(|o| Some(&o.id) == org_id.as_ref())
            .map(|o| o.tier.clone())
            .unwrap_or_default();
        self.login = Some(me.login.clone());
        self.orgs = orgs
            .iter()
            .map(|o| OrgView {
                id: o.id.clone(),
                name: o.name.clone(),
                tier: o.tier.clone(),
                personal: o.personal,
            })
            .collect();
        self.sizes = sizes_for(&orgs, org_id.as_deref(), me);
        self.sleep_choices = view::sleep_choices(me, tier == "free");
        self.sandboxes = sandbox_views(d, org_id.as_deref(), now);
        self.snapshots = snapshot_views(&d.snapshots, &d.templates, org_id.as_deref());
        self.tier = tier;
        self.org_id = org_id;
    }

    /// The state after sign-out: what belongs to the account is gone.
    pub fn signed_out(&self) -> AppState {
        AppState {
            t3: self.t3.clone(),
            start_at_login: self.start_at_login,
            update: self.update.clone(),
            ..AppState::default()
        }
    }
}

// ---- Core -----------------------------------------------------------------------------

struct Inner {
    state: AppState,
    data: Data,
    session: Option<Arc<Session>>,
    client: Option<Client>,
    /// Counts account changes (sign-out, a new session). A flow that started under an
    /// older count must not write its result.
    account: u64,
}

impl Inner {
    fn rebuild(&mut self) {
        self.state.apply(&self.data, Utc::now());
    }

    /// Records the desktop T3 version just read. A move from one known version to another
    /// gives the sandboxes to bring along.
    fn set_desktop_version(&mut self, t3: Option<String>) -> Option<SyncJob> {
        let job = version_changed(self.state.t3.as_deref(), t3.as_deref())
            .then(|| t3.clone())
            .flatten()
            .map(|v| SyncJob {
                version: v,
                ids: sync_targets(&self.data),
                account: self.account,
            });
        self.state.t3 = t3;
        job
    }

    /// Ends the progress, and the copy of its error in `state.error`.
    fn clear_progress(&mut self) {
        if let Some(p) = self.state.progress.take()
            && p.error.is_some()
            && p.error == self.state.error
        {
            self.state.error = None;
        }
    }

    /// Mirrors the session's own flags into the state.
    fn session_flags(&mut self) {
        let Some(s) = &self.session else { return };
        if s.is_signed_out() {
            self.session = None;
            self.client = None;
            self.state = self.state.signed_out();
            self.data = Data::default();
            return;
        }
        if let Some(e) = s.store_error() {
            self.state.error = Some(ErrorView::other(format!(
                "Could not save your sign-in to the keychain: {e}"
            )));
        }
    }
}

/// A desktop version change: the version to bring these sandboxes to.
pub struct SyncJob {
    pub version: String,
    pub ids: Vec<String>,
    /// The account count when the job was made; a job for an older account is dropped.
    account: u64,
}

pub struct CreateArgs {
    pub name: String,
    pub snapshot_id: Option<String>,
    pub vcpus: u32,
    pub mem_mib: u32,
    pub auto_suspend_after_seconds: Option<i64>,
}

pub struct Core {
    base: Url,
    keys: Arc<dyn KeyStore>,
    ssh: Arc<dyn SshTokenStore>,
    connected: Arc<dyn ConnectedStore>,
    open: Opener,
    emit: Emitter,
    home: PathBuf,
    inner: Mutex<Inner>,
}

async fn detect_t3() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let cli = desktop::locate()?;
        desktop::version(&cli).ok()
    })
    .await
    .ok()
    .flatten()
}

fn anyhow_view(e: &anyhow::Error) -> ErrorView {
    ErrorView::other(format!("{e:#}"))
}

type Res<T> = Result<T, ErrorView>;

impl Core {
    pub fn new(
        base: Url,
        keys: Arc<dyn KeyStore>,
        ssh: Arc<dyn SshTokenStore>,
        connected: Arc<dyn ConnectedStore>,
        open: Opener,
        emit: Emitter,
        home: PathBuf,
    ) -> Core {
        // The connected set is per user: it loads once the account is known (refresh).
        Core {
            base,
            keys,
            ssh,
            connected,
            open,
            emit,
            home,
            inner: Mutex::new(Inner {
                state: AppState::default(),
                data: Data::default(),
                session: None,
                client: None,
                account: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the state, then emits it. The lock is not held while emitting.
    fn update<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let (r, state) = {
            let mut g = self.lock();
            let r = f(&mut g);
            (r, g.state.clone())
        };
        (self.emit)(&state);
        r
    }

    pub fn state(&self) -> AppState {
        self.lock().state.clone()
    }

    /// The account count a long flow captures when it starts.
    fn account(&self) -> u64 {
        self.lock().account
    }

    fn client(&self) -> Res<Client> {
        self.lock().client.clone().ok_or_else(|| ErrorView {
            kind: crate::errors::ErrorKind::SignedOut,
            message: "You are not signed in".to_string(),
            minutes: None,
            code: None,
        })
    }

    pub fn install_session(&self, session: Session) {
        let session = Arc::new(session);
        let creds: Arc<dyn Credentials> = session.clone();
        let client = Client::new(self.base.clone(), creds);
        self.update(|i| {
            i.session = Some(session);
            i.client = Some(client);
            i.account += 1;
            i.state.signed_in = true;
            i.state.error = None;
            i.state.sign_in_code = None;
        });
    }

    pub fn set_start_at_login(&self, on: bool) {
        self.update(|i| i.state.start_at_login = on);
    }

    pub fn open_url(&self, url: &str) -> Res<()> {
        let r = if allowed_url(url) {
            (self.open)(url).map_err(ErrorView::other)
        } else {
            Err(ErrorView::other("This link cannot be opened"))
        };
        self.settle(r)
    }

    /// Runs an action's result through the error rules: a failure shows in `state.error`,
    /// the next success clears it. Also checks whether the session ended.
    pub fn settle<T>(&self, r: Res<T>) -> Res<T> {
        self.update(|i| {
            match &r {
                Ok(_) => i.state.error = None,
                Err(e) => i.state.error = Some(e.clone()),
            }
            i.session_flags();
        });
        r
    }

    /// `settle` for a flow that began under `account`: when the account changed since,
    /// the result or error belongs to nobody on screen and is only returned.
    fn settle_in<T>(&self, account: u64, r: Res<T>) -> Res<T> {
        if self.account() != account {
            return r;
        }
        self.settle(r)
    }

    // ---- launch and refresh ----

    pub async fn start(self: &Arc<Self>) {
        let t3 = detect_t3().await;
        self.update(|i| i.state.t3 = t3);
        let base = self.base.clone();
        let session = tokio::task::spawn_blocking(move || Session::load(&base))
            .await
            .ok()
            .flatten();
        if let Some(s) = session {
            self.install_session(s);
            self.refresh_and_sync(true).await;
            self.refresh_mcp().await;
        }
        self.check_update().await;
    }

    /// Reads machines, and with `full` also the account, templates, snapshots and the
    /// desktop T3 version. Lists only; nothing here runs a command on a sandbox.
    pub async fn refresh(&self, full: bool) -> Option<SyncJob> {
        let client = self.lock().client.clone()?;
        let account = self.account();
        let running = tokio::task::spawn_blocking(desktop::running)
            .await
            .unwrap_or(false);
        let full = full || self.lock().data.me.is_none();
        let machines = client.machines().await;
        let account_reads = if full {
            let (me, orgs, templates, snapshots) = tokio::join!(
                client.me(),
                client.orgs(),
                client.templates(),
                client.snapshots()
            );
            Some((me, orgs, templates, snapshots, detect_t3().await))
        } else {
            None
        };
        let was_full = account_reads.is_some();
        let user = match &account_reads {
            Some((Ok(me), ..)) => Some(me.user_id),
            _ => self.lock().data.me.as_ref().map(|m| m.user_id),
        };
        let connected = self.load_connected(user).await;
        self.update(|i| {
            // Signed out while the reads were in flight: drop them.
            i.client.as_ref()?;
            // The account changed while the reads were in flight: they are not its data.
            if i.account != account {
                return None;
            }
            let mut failure: Option<ErrorView> = None;
            i.data.desktop_running = running;
            if let Some((user, set, problem)) = connected {
                i.data.use_connected(user, set);
                if let Some(e) = problem {
                    // Shown once: the list is loaded again only for another user.
                    i.state.error = Some(ErrorView::other(e));
                }
            }
            match machines {
                Ok(m) => {
                    i.state.progress = prune_progress(i.state.progress.take(), &m);
                    i.data.machines = m;
                }
                Err(e) => failure = Some(e.into()),
            }
            let mut version = None;
            if let Some((me, orgs, templates, snapshots, t3)) = account_reads {
                match me {
                    Ok(v) => i.data.me = Some(v),
                    Err(e) => failure = Some(e.into()),
                }
                if let Ok(v) = orgs {
                    i.data.orgs = v;
                }
                if let Ok(v) = templates {
                    i.data.templates = v;
                }
                if let Ok(v) = snapshots {
                    i.data.snapshots = v;
                }
                version = Some(t3);
            }
            i.rebuild();
            // A failed poll is silent; only a failed full read shows.
            if let (true, Some(e)) = (was_full, failure) {
                i.state.error = Some(e);
            }
            let job = version.and_then(|t3| i.set_desktop_version(t3));
            i.session_flags();
            job
        })
    }

    /// The connected set of `user` from disk, when it is not the one in use.
    /// A list that cannot be read comes back empty, with the reason.
    async fn load_connected(
        &self,
        user: Option<i64>,
    ) -> Option<(i64, HashSet<String>, Option<String>)> {
        let user = user.filter(|u| self.lock().data.connected_user != Some(*u))?;
        let store = self.connected.clone();
        let r = tokio::task::spawn_blocking(move || store.load_checked(user))
            .await
            .ok()?;
        Some(match r {
            Ok(set) => (user, set, None),
            Err(e) => (user, HashSet::new(), Some(e)),
        })
    }

    /// Looks for T3 Code on this computer now (the "Check again" button), instead of
    /// waiting for the next full refresh.
    pub async fn refresh_t3(self: &Arc<Self>) -> Res<()> {
        let t3 = detect_t3().await;
        let job = self.update(|i| i.set_desktop_version(t3));
        self.spawn_sync(job);
        Ok(())
    }

    /// `refresh`, then syncs the sandboxes after a desktop version change in the background.
    pub async fn refresh_and_sync(self: &Arc<Self>, full: bool) {
        let job = self.refresh(full).await;
        self.spawn_sync(job);
    }

    fn spawn_sync(self: &Arc<Self>, job: Option<SyncJob>) {
        if let Some(job) = job
            && !job.ids.is_empty()
        {
            let core = self.clone();
            tokio::spawn(async move { core.sync_all(job).await });
        }
    }

    /// One sync per running connected sandbox. This exec follows a version change; it is
    /// not a timer. A sandbox that cannot be updated shows `updatePending`; nothing retries.
    async fn sync_all(&self, job: SyncJob) {
        if self.account() != job.account {
            return;
        }
        let Ok(client) = self.client() else { return };
        for id in &job.ids {
            let r = sandbox::sync::sync(&client, id, &job.version).await;
            self.record_sync(id, &job.version, r.as_ref().ok(), job.account);
        }
    }

    fn record_sync(&self, id: &str, desktop: &str, outcome: Option<&SyncOutcome>, account: u64) {
        self.update(|i| {
            if i.account != account {
                return;
            }
            i.data.record_sync(id, desktop, outcome);
            i.rebuild();
        });
    }

    fn record_version(&self, id: &str, version: &str, account: u64) {
        self.update(|i| {
            if i.account != account {
                return;
            }
            i.data.set_version(id, version);
            i.rebuild();
        });
    }

    pub async fn refresh_mcp(&self) {
        let home = self.home.clone();
        let status = tokio::task::spawn_blocking(move || {
            let installed = mcp::claude_installed();
            mcp_status(
                installed,
                installed && mcp::configured(&home, McpClient::ClaudeCode),
                mcp::configured(&home, McpClient::Codex),
            )
        })
        .await
        .unwrap_or_default();
        self.update(|i| i.state.mcp = status);
    }

    pub async fn check_update(&self) {
        let Ok(http) = reqwest::Client::builder()
            .user_agent("instabox-bridge")
            .timeout(Duration::from_secs(10))
            .build()
        else {
            return;
        };
        let Ok(resp) = http.get(RELEASES_API).send().await else {
            return;
        };
        if !resp.status().is_success() {
            return;
        }
        let Ok(body) = resp.text().await else { return };
        if let Some(u) = newer_release(env!("CARGO_PKG_VERSION"), &body) {
            self.update(|i| i.state.update = Some(u));
        }
    }

    /// Refreshes machines on a timer until the process ends.
    pub async fn run_loop(self: Arc<Self>, window_visible: impl Fn() -> bool) {
        let mut last = Instant::now();
        let mut last_full = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if last.elapsed() < refresh_interval(window_visible()) || !self.state().signed_in {
                continue;
            }
            last = Instant::now();
            let full = last_full.elapsed() >= FULL_REFRESH;
            if full {
                last_full = Instant::now();
            }
            self.refresh_and_sync(full).await;
        }
    }

    // ---- account ----

    pub async fn sign_in(self: &Arc<Self>, provider: Provider) -> Res<()> {
        let r = self.sign_in_inner(provider).await;
        self.update(|i| i.state.sign_in_code = None);
        self.settle(r)?;
        self.refresh_and_sync(true).await;
        self.refresh_mcp().await;
        Ok(())
    }

    async fn sign_in_inner(&self, provider: Provider) -> Res<()> {
        let start = auth::start(&self.base, provider)
            .await
            .map_err(|e| ErrorView::from_auth(&e))?;
        self.update(|i| i.state.sign_in_code = Some(start.user_code.clone()));
        (self.open)(start.browser_url()).map_err(ErrorView::other)?;
        let session = auth::poll(&self.base, &start)
            .await
            .map_err(|e| ErrorView::from_auth(&e))?;
        self.install_session(session);
        Ok(())
    }

    /// An account boundary: forgets the sign-in tokens, the MCP key and every SSH token,
    /// and clears the account state, the user's connected set included (it stays on disk
    /// for that user). Then it ends the session on the server in the background, with the
    /// token it had, and never refreshes for that. The state clears even when a store
    /// fails; the first failure is returned and shown.
    pub async fn sign_out(&self) -> Res<()> {
        let session = self.update(|i| {
            let s = i.session.take();
            i.client = None;
            i.account += 1;
            i.state = i.state.signed_out();
            i.data = Data::default();
            s
        });
        // Capture the token as it is: a refresh here could hold the secrets for 30 s.
        let token = session.as_ref().and_then(|s| s.current_access_token());
        let tokens = match session {
            Some(s) => s.sign_out().map_err(|e| e.to_string()),
            None => Ok(()),
        };
        let key = self.keys.clear();
        let ssh = self.ssh.clear_all();
        if let Some(token) = token {
            // Best effort, in the background: the local sign-out never waits for the server.
            let base = self.base.clone();
            tokio::spawn(async move { auth::logout_with(&base, &token).await });
        }
        self.settle(tokens.and(key).and(ssh).map_err(ErrorView::other))
    }

    pub fn switch_org(&self, org_id: &str) -> Res<()> {
        let r = self.update(|i| {
            if i.data.orgs.iter().any(|o| o.id == org_id) {
                i.state.org_id = Some(org_id.to_string());
                i.rebuild();
                Ok(())
            } else {
                Err(ErrorView::other("Unknown organization"))
            }
        });
        self.settle(r)
    }

    // ---- sandbox actions ----

    async fn desktop_version(&self) -> Res<String> {
        if let Some(v) = self.lock().state.t3.clone() {
            return Ok(v);
        }
        match detect_t3().await {
            Some(v) => {
                self.update(|i| i.state.t3 = Some(v.clone()));
                Ok(v)
            }
            None => Err(ErrorView::other(
                "T3 Code is not installed. Install it first.",
            )),
        }
    }

    pub async fn create_sandbox(self: &Arc<Self>, a: CreateArgs) -> Res<()> {
        let account = self.account();
        let (client, version) = match self.prepare().await {
            Ok(v) => v,
            Err(e) => return self.settle(Err(e)),
        };
        let org_id = self.lock().state.org_id.clone();
        let req = NewSandbox {
            name: a.name,
            source: a.snapshot_id.map_or(Source::Standard, Source::Snapshot),
            spec: MachineSpec {
                vcpus: a.vcpus,
                mem_mib: a.mem_mib,
            },
            auto_suspend_after_seconds: a.auto_suspend_after_seconds,
            org_id,
        };
        self.begin_progress("create", "creating", None, account);
        let r =
            connect::create_and_connect(&client, &req, &version, self.progress_cb(account)).await;
        self.finish_connect(r, &version, account).await
    }

    /// Connects an existing sandbox: wakes it, installs T3 (never an older one), pairs,
    /// then opens T3 Code with the pairing link.
    pub async fn connect_sandbox(self: &Arc<Self>, id: &str) -> Res<()> {
        let account = self.account();
        let (client, version) = match self.prepare().await {
            Ok(v) => v,
            Err(e) => return self.settle(Err(e)),
        };
        self.begin_progress("connect", "connecting", Some(id.to_string()), account);
        let r = connect::connect_existing(&client, id, &version, self.progress_cb(account)).await;
        self.finish_connect(r, &version, account).await
    }

    /// Open in T3 Code. A connected sandbox: wake it, sync its T3, open the app (no setup,
    /// no new pairing). An unconnected one goes through the connect flow.
    pub async fn open_in_t3(self: &Arc<Self>, id: &str) -> Res<()> {
        let connected = self
            .lock()
            .state
            .sandboxes
            .iter()
            .any(|s| s.id == id && s.connected);
        if !connected {
            return self.connect_sandbox(id).await;
        }
        let account = self.account();
        let client = self.client()?;
        let version = self.lock().state.t3.clone();
        let r = async {
            sandbox::ensure_running(&client, id)
                .await
                .map_err(ErrorView::from)?;
            if let Some(v) = &version {
                let out = sandbox::sync::sync(&client, id, v).await;
                self.record_sync(id, v, out.as_ref().ok(), account);
            }
            (self.open)(T3_APP_URL).map_err(ErrorView::other)
        };
        self.act(r).await
    }

    /// Makes a new pairing link for a connected sandbox and opens it.
    pub async fn pair_again(self: &Arc<Self>, id: &str) -> Res<()> {
        let account = self.account();
        let client = self.client()?;
        let linked = std::sync::atomic::AtomicBool::new(false);
        let r = async {
            let link = connect::pair_again(&client, id)
                .await
                .map_err(ErrorView::from)?;
            linked.store(true, std::sync::atomic::Ordering::SeqCst);
            (self.open)(&link).map_err(ErrorView::other)
        };
        let out = self.act(r).await;
        if linked.load(std::sync::atomic::Ordering::SeqCst) {
            // After `act`, which clears the error: a failed save stays visible.
            self.remember(id, account).await;
        }
        out
    }

    /// Adds the sandbox to the set the user connected from this computer, on disk and in
    /// the state. Without a known user it stays in memory until the account loads.
    /// Does nothing when the account changed since `account` was read.
    async fn remember(&self, id: &str, account: u64) {
        let user = self.update(|i| {
            if i.account != account {
                return None;
            }
            i.data.connected.insert(id.to_string());
            i.rebuild();
            i.data.user()
        });
        let Some(user) = user else { return };
        let store = self.connected.clone();
        let id = id.to_string();
        let r = tokio::task::spawn_blocking(move || store.add(user, &id)).await;
        self.report_store(r);
    }

    /// Removes the sandbox from the set the user connected from this computer.
    /// Does nothing when the account changed since `account` was read.
    async fn forget(&self, id: &str, account: u64) {
        let user = self.update(|i| {
            if i.account != account {
                return None;
            }
            i.data.connected.remove(id);
            i.rebuild();
            i.data.user()
        });
        let Some(user) = user else { return };
        let store = self.connected.clone();
        let id = id.to_string();
        let r = tokio::task::spawn_blocking(move || store.remove(user, &id)).await;
        self.report_store(r);
    }

    fn report_store(&self, r: Result<Result<(), String>, tokio::task::JoinError>) {
        let err = match r {
            Ok(Ok(())) => return,
            Ok(Err(e)) => e,
            Err(e) => e.to_string(),
        };
        self.update(|i| {
            i.state.error = Some(ErrorView::other(format!(
                "Could not save the sandboxes connected from this computer: {err}"
            )));
        });
    }

    async fn prepare(&self) -> Res<(Client, String)> {
        Ok((self.client()?, self.desktop_version().await?))
    }

    fn begin_progress(&self, kind: &str, step: &str, machine_id: Option<String>, account: u64) {
        self.update(|i| {
            if i.account != account {
                return;
            }
            i.state.error = None;
            i.state.progress = Some(ProgressView {
                kind: kind.to_string(),
                step: step.to_string(),
                machine_id,
                error: None,
            });
        });
    }

    /// Moves the progress on. A progress that was dismissed or pruned stays gone.
    fn progress_cb(&self, account: u64) -> impl Fn(Step) + '_ {
        move |step| {
            self.update(|i| {
                if i.account != account {
                    return;
                }
                if let Some(p) = &i.state.progress {
                    i.state.progress = Some(progress_after(p, &step));
                }
            });
        }
    }

    /// The × on a failed progress.
    pub fn dismiss_progress(&self) {
        self.update(Inner::clear_progress);
    }

    async fn finish_connect(
        self: &Arc<Self>,
        r: Result<Connected, SandboxError>,
        version: &str,
        account: u64,
    ) -> Res<()> {
        if self.account() != account {
            // Signed out or switched account while this ran: drop the result.
            return Ok(());
        }
        match r {
            Ok(c) => {
                // A sandbox that kept a newer T3 is not moved to the desktop's: the desktop is
                // behind. One that kept an older T3 because T3 was busy offers the update.
                self.update(|i| {
                    i.state.progress = None;
                    i.data.set_version(&c.machine_id, &c.t3_version);
                    if c.update_pending {
                        i.data.update_pending.insert(c.machine_id.clone());
                    } else if c.t3_version != version {
                        i.data.desktop_behind.insert(c.machine_id.clone());
                    }
                    i.rebuild();
                });
                let opened = (self.open)(&c.link).map_err(ErrorView::other);
                let out = self.settle(opened);
                // After `settle`, which clears the error: a failed save stays visible.
                self.remember(&c.machine_id, account).await;
                self.refresh_and_sync(false).await;
                out
            }
            Err(e) => {
                let view = ErrorView::from(e);
                self.update(|i| {
                    i.state.progress = fail_progress(i.state.progress.as_ref(), &view);
                });
                let out = self.settle(Err(view));
                self.refresh_and_sync(false).await;
                out
            }
        }
    }

    /// Runs a sandbox action, then refreshes machines.
    async fn act<T>(self: &Arc<Self>, f: impl std::future::Future<Output = Res<T>>) -> Res<T> {
        let account = self.account();
        let r = f.await;
        let out = self.settle_in(account, r);
        if self.account() == account {
            self.refresh_and_sync(false).await;
        }
        out
    }

    pub async fn pause(self: &Arc<Self>, id: &str) -> Res<()> {
        let client = self.client()?;
        self.act(async { lifecycle::pause(&client, id).await.map_err(ErrorView::from) })
            .await
    }

    /// Wakes the sandbox. A connected one also gets its T3 brought in step with the desktop.
    pub async fn resume(self: &Arc<Self>, id: &str) -> Res<()> {
        let account = self.account();
        let client = self.client()?;
        // Only a sandbox connected from this computer gets its T3 synced after a resume.
        let version = {
            let g = self.lock();
            g.data
                .connected
                .contains(id)
                .then(|| g.state.t3.clone())
                .flatten()
        };
        let r = async {
            match &version {
                Some(v) => {
                    let o = lifecycle::resume(&client, id, v)
                        .await
                        .map_err(ErrorView::from)?;
                    if let Some(o) = o {
                        self.record_sync(id, v, Some(&o), account);
                    }
                    Ok(())
                }
                None => sandbox::ensure_running(&client, id)
                    .await
                    .map(|_| ())
                    .map_err(ErrorView::from),
            }
        };
        self.act(r).await
    }

    pub async fn delete(self: &Arc<Self>, id: &str) -> Res<()> {
        let account = self.account();
        let client = self.client()?;
        let r = self
            .act(async {
                lifecycle::delete(&client, id)
                    .await
                    .map_err(ErrorView::from)
            })
            .await;
        if r.is_ok() {
            self.update(|i| {
                if i.account != account {
                    return;
                }
                i.data.t3_versions.remove(id);
                i.data.update_pending.remove(id);
                i.data.desktop_behind.remove(id);
                // A progress of this sandbox can only retry a sandbox that is gone.
                let of_this = i.state.progress.as_ref().map(|p| p.machine_id.as_deref());
                if of_this == Some(Some(id)) {
                    i.clear_progress();
                }
                i.rebuild();
            });
            self.forget(id, account).await;
        }
        r
    }

    pub async fn resize(self: &Arc<Self>, id: &str, vcpus: u32, mem_mib: u32) -> Res<()> {
        let client = self.client()?;
        self.act(async {
            lifecycle::resize(&client, id, vcpus, mem_mib)
                .await
                .map_err(ErrorView::from)
        })
        .await
    }

    pub async fn set_auto_sleep(self: &Arc<Self>, id: &str, seconds: i64) -> Res<()> {
        let client = self.client()?;
        self.act(async {
            lifecycle::set_auto_sleep(&client, id, seconds)
                .await
                .map_err(ErrorView::from)
        })
        .await
    }

    pub async fn reinstall_t3(self: &Arc<Self>, id: &str) -> Res<()> {
        let account = self.account();
        let client = self.client()?;
        let r = async {
            let v = self.desktop_version().await?;
            lifecycle::reinstall(&client, id, &v)
                .await
                .map_err(ErrorView::from)?;
            self.record_version(id, &v, account);
            Ok(())
        };
        self.act(r).await
    }

    pub async fn agent_sign_in(self: &Arc<Self>, id: &str, agent: Agent) -> Res<()> {
        let client = self.client()?;
        let open = self.open.clone();
        let r = async {
            agents::sign_in(&client, &*self.ssh, id, agent, move |u| {
                open(u).map_err(anyhow::Error::msg)
            })
            .await
            .map_err(|e| anyhow_view(&e))
        };
        self.act(r).await
    }

    /// Whether `agent` is signed in on the sandbox. One status exec, and only when the
    /// sandbox is running: a sleeping sandbox answers false without being woken.
    pub async fn agent_status(&self, id: &str, agent: Agent) -> Res<bool> {
        let client = self.client()?;
        let machine = client.machine(id).await.map_err(ErrorView::from)?;
        if machine.state != "running" {
            return Ok(false);
        }
        Ok(agents::signed_in(&client, id, agent).await)
    }

    /// The SSH login command to show and copy. The details view shows a failure next to
    /// the SSH row, so this leaves `state.error` alone.
    pub async fn ssh_command(&self, id: &str) -> Res<String> {
        let r = self.login_command(id).await;
        if r.is_err() {
            self.update(Inner::session_flags);
        }
        r
    }

    async fn login_command(&self, id: &str) -> Res<String> {
        let client = self.client()?;
        let t = ssh::token(&client, &*self.ssh, id)
            .await
            .map_err(ErrorView::from)?;
        Ok(ssh::display_command(&t))
    }

    pub async fn open_ssh(&self, id: &str) -> Res<()> {
        let account = self.account();
        let r = async {
            let cmd = self.login_command(id).await?;
            ssh::open_in_terminal(&cmd).map_err(ErrorView::other)
        };
        self.settle_in(account, r.await)
    }

    pub async fn add_mcp(&self, which: McpClient) -> Res<()> {
        let account = self.account();
        let client = self.client()?;
        let r = mcp::add(
            &client,
            &mcp::mcp_url(&self.base),
            &self.home,
            which,
            self.keys.clone(),
        )
        .await
        .map_err(|e| anyhow_view(&e));
        let out = self.settle_in(account, r);
        self.refresh_mcp().await;
        out
    }
}

// ---- tests ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{TemplateRef, Tokens};
    use crate::auth::{MemoryStore, TokenStore};
    use crate::connected::{FileConnectedStore, MemoryConnectedStore};
    use crate::mcp::MemoryKeyStore;
    use crate::ssh::MemorySshStore;
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
    }

    fn spec(v: u32, m: u32) -> MachineSpec {
        MachineSpec {
            vcpus: v,
            mem_mib: m,
        }
    }

    fn me() -> Me {
        Me {
            user_id: 7,
            login: "alex".into(),
            allowed_specs: vec![spec(2, 4096), spec(4, 8192)],
            org: Some(crate::api::OrgRef {
                id: "org-me".into(),
                name: Some("alex".into()),
                tier: "free".into(),
                kind: "personal".into(),
                always_on_allowed: Some(false),
            }),
            fixed_auto_suspend_seconds: None,
            idle_choices_seconds: vec![300, 900, 1800],
            always_on_allowed: false,
        }
    }

    fn org(id: &str, tier: &str, specs: Vec<MachineSpec>) -> Org {
        Org {
            id: id.into(),
            name: id.into(),
            tier: tier.into(),
            kind: if id == "org-me" { "personal" } else { "team" }.into(),
            personal: id == "org-me",
            allowed_specs: specs,
        }
    }

    fn template(id: &str, os: Option<&str>, pool: Option<&str>) -> Template {
        Template {
            id: id.into(),
            os: os.map(str::to_string),
            pool: pool.map(str::to_string),
            hidden: false,
            creatable: true,
            latest: true,
        }
    }

    fn machine(id: &str, org: &str, tpl: &str, state: &str, connected: bool) -> Machine {
        let mut metadata = BTreeMap::new();
        if connected {
            metadata.insert("instabox-bridge".to_string(), "t3".to_string());
        }
        Machine {
            id: id.into(),
            name: format!("name-{id}"),
            state: state.into(),
            template: TemplateRef { id: tpl.into() },
            spec: spec(4, 8192),
            auto_suspend_after_seconds: Some(900),
            max_run_minutes: -1,
            run_started_at: None,
            cooldown_until: None,
            metadata,
            org_id: org.into(),
            free: false,
            operation_id: None,
        }
    }

    fn data() -> Data {
        Data {
            me: Some(me()),
            orgs: vec![
                org("org-me", "free", vec![]),
                org("org-team", "pro", vec![spec(8, 16384)]),
            ],
            templates: vec![
                template("agent-browser", None, None),
                template("mac", Some("macos"), None),
                template("gpu", None, Some("gpu")),
            ],
            machines: vec![
                machine("a", "org-me", "agent-browser", "running", true),
                machine("b", "org-team", "agent-browser", "running", true),
                machine("c", "org-me", "mac", "running", false),
                machine("d", "org-me", "gpu", "suspended", false),
                machine("e", "org-me", "agent-browser", "suspended", true),
                machine("f", "org-me", "agent-browser", "running", false),
                machine("g", "org-me", "custom-unknown", "running", false),
            ],
            snapshots: vec![
                snap("s1", "ready", "agent-browser", None),
                snap("s2", "creating", "agent-browser", None),
                snap("s3", "ready", "mac", None),
                snap("s4", "ready", "agent-browser", Some("org-team")),
            ],
            desktop_running: true,
            t3_versions: HashMap::from([("a".to_string(), "0.1.10".to_string())]),
            update_pending: HashSet::from(["e".to_string()]),
            desktop_behind: HashSet::from(["g".to_string()]),
            connected: HashSet::from(["a".to_string(), "b".to_string(), "e".to_string()]),
            connected_user: Some(7),
        }
    }

    fn snap(id: &str, status: &str, tpl: &str, org: Option<&str>) -> Snapshot {
        Snapshot {
            id: id.into(),
            name: format!("snap-{id}"),
            template: TemplateRef { id: tpl.into() },
            status: status.into(),
            org_id: org.map(str::to_string),
        }
    }

    fn ids(v: &[SandboxView]) -> Vec<&str> {
        v.iter().map(|s| s.id.as_str()).collect()
    }

    /// The state the shared fixture `src/fixtures/state.json` holds.
    fn fixture_state() -> AppState {
        let mut d = data();
        d.machines[0].free = true;
        d.machines[0].run_started_at = Some(now() - chrono::Duration::minutes(20));
        d.machines[0].max_run_minutes = 120;
        let mut s = AppState::default();
        s.apply(&d, now());
        s.signed_in = true;
        s.sign_in_code = None;
        s.t3 = Some("0.1.10".into());
        s.mcp = McpStatus {
            claude_code: Some(true),
            codex: Some(false),
        };
        s.progress = Some(ProgressView {
            kind: "create".into(),
            step: "installing".into(),
            machine_id: Some("a".into()),
            error: None,
        });
        s.error = Some(ErrorView::from(SandboxError::Cooldown { minutes: 12 }));
        s.start_at_login = true;
        s.update = Some(UpdateInfo {
            version: "0.2.0".into(),
            url: "https://github.com/InsForge/instabox-bridge/releases/tag/v0.2.0".into(),
        });
        s
    }

    #[test]
    fn state_json_matches_the_shared_fixture() {
        let got = serde_json::to_value(fixture_state()).unwrap();
        let want: serde_json::Value =
            serde_json::from_str(include_str!("../../src/fixtures/state.json")).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn state_keys_are_camel_case() {
        let got = serde_json::to_value(fixture_state()).unwrap();
        let keys: std::collections::BTreeSet<&str> = got
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let want: std::collections::BTreeSet<&str> = [
            "signedIn",
            "login",
            "signInCode",
            "orgs",
            "orgId",
            "tier",
            "t3",
            "sandboxes",
            "sizes",
            "sleepChoices",
            "snapshots",
            "mcp",
            "progress",
            "error",
            "startAtLogin",
            "update",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, want);
    }

    #[test]
    fn selected_org_filters_machines_and_hides_non_linux() {
        let d = data();
        let mut s = AppState::default();
        s.apply(&d, now());
        // org-me (the personal default): a, e, f and the unknown-template g.
        // Hidden: b (other org), c (macOS), d (pool template).
        assert_eq!(s.org_id.as_deref(), Some("org-me"));
        assert_eq!(ids(&s.sandboxes), ["a", "e", "f", "g"]);
        s.org_id = Some("org-team".into());
        s.apply(&d, now());
        assert_eq!(ids(&s.sandboxes), ["b"]);
    }

    #[test]
    fn plan_sizes_and_sleep_follow_the_selected_org() {
        let d = data();
        let mut s = AppState::default();
        s.apply(&d, now());
        // free personal org: the user's specs, one fixed sleep choice.
        assert_eq!(s.tier, "free");
        assert_eq!(s.sizes.len(), 2);
        assert_eq!(s.sleep_choices.len(), 1);
        assert_eq!(s.sleep_choices[0].seconds, 1800);
        s.org_id = Some("org-team".into());
        s.apply(&d, now());
        assert_eq!(s.tier, "pro");
        assert_eq!(s.sizes.len(), 1);
        assert_eq!(s.sizes[0].label, "8 vCPU / 16 GB");
        assert_eq!(
            s.sleep_choices
                .iter()
                .map(|c| c.seconds)
                .collect::<Vec<_>>(),
            [300, 900, 1800]
        );
    }

    #[test]
    fn unknown_selected_org_falls_back_to_the_personal_org() {
        let mut s = AppState {
            org_id: Some("gone".into()),
            ..AppState::default()
        };
        s.apply(&data(), now());
        assert_eq!(s.org_id.as_deref(), Some("org-me"));
    }

    #[test]
    fn orgs_come_from_me_when_the_list_is_empty() {
        let mut d = data();
        d.orgs.clear();
        let mut s = AppState::default();
        s.apply(&d, now());
        assert_eq!(s.orgs.len(), 1);
        assert_eq!(s.orgs[0].id, "org-me");
        assert!(s.orgs[0].personal);
    }

    #[test]
    fn t3_code_connected_needs_connected_running_and_desktop_running() {
        let mut d = data();
        let get = |d: &Data, id: &str| {
            sandbox_views(d, Some("org-me"), now())
                .into_iter()
                .find(|v| v.id == id)
                .unwrap()
        };
        assert!(get(&d, "a").t3_code_connected);
        assert!(!get(&d, "e").t3_code_connected, "suspended");
        assert!(!get(&d, "f").t3_code_connected, "not connected");
        assert_eq!(get(&d, "a").t3_version.as_deref(), Some("0.1.10"));
        assert_eq!(get(&d, "f").t3_version, None);
        d.desktop_running = false;
        assert!(!get(&d, "a").t3_code_connected, "desktop not running");
    }

    #[test]
    fn snapshots_are_ready_linux_and_in_the_selected_org() {
        let d = data();
        let got = |org: &str| {
            snapshot_views(&d.snapshots, &d.templates, Some(org))
                .into_iter()
                .map(|s| s.id)
                .collect::<Vec<_>>()
        };
        // s2 not ready, s3 macOS; s4 belongs to the team org; s1 has no org and shows.
        assert_eq!(got("org-me"), ["s1"]);
        assert_eq!(got("org-team"), ["s1", "s4"]);
    }

    #[test]
    fn desktop_version_change_selects_running_connected_sandboxes_of_every_org() {
        let mut d = data();
        d.machines
            .push(machine("h", "org-me", "agent-browser", "stopped", true));
        // a (org-me) and b (org-team) are running and connected. e: suspended. f/g: running
        // but not connected. h: stopped. c, d: not Linux.
        assert_eq!(sync_targets(&d), ["a", "b"]);
        assert!(version_changed(Some("0.1.9"), Some("0.1.10")));
        assert!(!version_changed(Some("0.1.10"), Some("0.1.10")));
        assert!(!version_changed(None, Some("0.1.10")), "first read");
        assert!(!version_changed(Some("0.1.10"), None), "T3 not readable");
    }

    fn outcome(decision: Decision, remote: Option<&str>) -> SyncOutcome {
        SyncOutcome {
            decision,
            remote_version: remote.map(str::to_string),
        }
    }

    fn view_of(d: &Data, id: &str) -> SandboxView {
        sandbox_views(d, Some("org-me"), now())
            .into_iter()
            .find(|v| v.id == id)
            .unwrap()
    }

    #[test]
    fn sync_outcomes_set_the_version_and_update_pending() {
        let mut d = data();
        d.update_pending.clear();
        // Defer: the sandbox keeps its own version and is pending.
        d.record_sync(
            "a",
            "0.1.11",
            Some(&outcome(Decision::Defer, Some("0.1.10"))),
        );
        let v = view_of(&d, "a");
        assert!(v.update_pending);
        assert_eq!(v.t3_version.as_deref(), Some("0.1.10"));
        // Older: ahead of the desktop, also pending.
        d.record_sync(
            "a",
            "0.1.11",
            Some(&outcome(Decision::Older, Some("0.1.12"))),
        );
        assert!(
            !view_of(&d, "a").update_pending,
            "ahead is not an update to offer"
        );
        assert!(view_of(&d, "a").desktop_behind);
        assert_eq!(view_of(&d, "a").t3_version.as_deref(), Some("0.1.12"));
        // A failed sync is pending and keeps the known version.
        d.record_sync("e", "0.1.11", None);
        assert!(view_of(&d, "e").update_pending);
        // Update: now at the desktop version, no longer pending.
        d.record_sync(
            "a",
            "0.1.11",
            Some(&outcome(Decision::Update, Some("0.1.10"))),
        );
        let v = view_of(&d, "a");
        assert!(!v.update_pending);
        assert_eq!(v.t3_version.as_deref(), Some("0.1.11"));
        // UpToDate: the sandbox's own version.
        d.record_sync(
            "e",
            "0.1.11",
            Some(&outcome(Decision::UpToDate, Some("0.1.11"))),
        );
        assert!(!view_of(&d, "e").update_pending);
    }

    #[test]
    fn a_marked_sandbox_not_connected_from_here_is_left_alone() {
        let mut d = data();
        // A teammate connected "t": it has the server mark but is not in the local set.
        d.machines
            .push(machine("t", "org-me", "agent-browser", "running", true));
        let t = view_of(&d, "t");
        assert!(!t.connected && !t.t3_code_connected);
        assert!(!sync_targets(&d).contains(&"t".to_string()));
        // Once this computer connects it, it is a sync target.
        d.connected.insert("t".into());
        assert!(view_of(&d, "t").connected);
        assert!(sync_targets(&d).contains(&"t".to_string()));
    }

    // A closed local port: a test that reaches the network fails here, never at the real API.
    fn core_with(store: Arc<dyn ConnectedStore>) -> Arc<Core> {
        core_on(Url::parse("http://127.0.0.1:9/").unwrap(), store)
    }

    fn core_on(base: Url, store: Arc<dyn ConnectedStore>) -> Arc<Core> {
        Arc::new(Core::new(
            base,
            Arc::new(MemoryKeyStore::default()),
            Arc::new(MemorySshStore::default()),
            store,
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            PathBuf::from("/nonexistent"),
        ))
    }

    fn connected_result(id: &str, t3_version: &str) -> Connected {
        Connected {
            machine_id: id.into(),
            link: "t3code://pair?pairingUrl=x".into(),
            t3_version: t3_version.into(),
            update_pending: false,
        }
    }

    #[tokio::test]
    async fn connecting_a_busy_sandbox_with_an_older_t3_offers_the_update() {
        let core = core_with(Arc::new(MemoryConnectedStore::default()));
        core.update(|i| {
            i.data.machines = vec![machine("t", "org-me", "agent-browser", "running", true)];
            i.data.me = Some(me());
            i.data.orgs = data().orgs;
            i.rebuild();
        });
        let c = Connected {
            update_pending: true,
            ..connected_result("t", "0.1.9")
        };
        core.finish_connect(Ok(c), "0.1.11", core.account())
            .await
            .unwrap();
        let v = &core.state().sandboxes[0];
        assert!(v.connected && v.update_pending && !v.desktop_behind);
        assert_eq!(v.t3_version.as_deref(), Some("0.1.9"));
    }

    #[tokio::test]
    async fn connect_adds_to_the_local_set_and_delete_removes_it() {
        let store = Arc::new(MemoryConnectedStore::default());
        let core = core_with(store.clone());
        core.update(|i| {
            i.data.machines = vec![machine("t", "org-me", "agent-browser", "running", true)];
            i.data.templates = data().templates;
            i.data.me = Some(me());
            i.data.orgs = data().orgs;
            i.rebuild();
        });
        assert!(!core.state().sandboxes[0].connected);
        core.finish_connect(
            Ok(connected_result("t", "0.1.11")),
            "0.1.11",
            core.account(),
        )
        .await
        .unwrap();
        assert!(store.load(7).contains("t"), "saved for the signed-in user");
        let s = core.state();
        assert!(s.sandboxes[0].connected);
        assert_eq!(s.sandboxes[0].t3_version.as_deref(), Some("0.1.11"));
        assert!(!s.sandboxes[0].desktop_behind);
        core.forget("t", core.account()).await;
        assert!(store.load(7).is_empty(), "removed");
        assert!(!core.state().sandboxes[0].connected);
    }

    #[tokio::test]
    async fn connecting_a_sandbox_with_a_newer_t3_marks_the_desktop_behind() {
        let core = core_with(Arc::new(MemoryConnectedStore::default()));
        core.update(|i| {
            i.data.machines = vec![machine("t", "org-me", "agent-browser", "running", true)];
            i.data.me = Some(me());
            i.data.orgs = data().orgs;
            i.rebuild();
        });
        core.finish_connect(
            Ok(connected_result("t", "0.1.12")),
            "0.1.11",
            core.account(),
        )
        .await
        .unwrap();
        let v = &core.state().sandboxes[0];
        assert!(v.desktop_behind && !v.update_pending);
        assert_eq!(v.t3_version.as_deref(), Some("0.1.12"));
    }

    /// A mock API that lists no machines, and an API key session on it.
    async fn listing_server() -> (wiremock::MockServer, Url) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/machines"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"machines": [], "nextCursor": null})),
            )
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        (server, base)
    }

    fn sign_in_as(core: &Core, base: &Url, user_id: i64) {
        let session = Session::load_from(
            base,
            Some("ibk_test".into()),
            Arc::new(MemoryStore::default()),
        )
        .unwrap();
        core.install_session(session);
        core.update(|i| i.data.me = Some(Me { user_id, ..me() }));
    }

    #[tokio::test]
    async fn the_connected_set_is_per_user_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        let (_server, base) = listing_server().await;
        let first = core_with(Arc::new(FileConnectedStore::new(path.clone())));
        first.update(|i| i.data.me = Some(me()));
        first.remember("m1", first.account()).await;

        // A restart: nothing is loaded until the account is known.
        let again = Arc::new(Core::new(
            base.clone(),
            Arc::new(MemoryKeyStore::default()),
            Arc::new(MemorySshStore::default()),
            Arc::new(FileConnectedStore::new(path)),
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            PathBuf::from("/nonexistent"),
        ));
        assert!(again.lock().data.connected.is_empty());
        sign_in_as(&again, &base, 7);
        again.refresh(false).await;
        assert!(again.lock().data.connected.contains("m1"), "user 7's set");

        again.sign_out().await.unwrap();
        assert!(
            again.lock().data.connected.is_empty(),
            "gone with the account"
        );

        sign_in_as(&again, &base, 8);
        again.refresh(false).await;
        assert!(
            again.lock().data.connected.is_empty(),
            "another user does not get user 7's sandboxes"
        );
        sign_in_as(&again, &base, 7);
        again.refresh(false).await;
        assert!(again.lock().data.connected.contains("m1"), "kept on disk");
    }

    #[test]
    fn progress_keeps_the_machine_id_and_failure_keeps_it_for_retry() {
        let start = ProgressView {
            kind: "create".into(),
            step: "creating".into(),
            machine_id: None,
            error: None,
        };
        let p1 = progress_after(&start, &Step::Creating);
        assert_eq!(p1.machine_id, None);
        let p2 = progress_after(&p1, &Step::Created("m1".into()));
        let p3 = progress_after(&p2, &Step::Installing("0.1.10".into()));
        assert_eq!(p3.step, "installing");
        assert_eq!(p3.kind, "create");
        assert_eq!(p3.machine_id.as_deref(), Some("m1"));
        let err = ErrorView::from(SandboxError::Setup {
            stderr_tail: "boom".into(),
        });
        let failed = fail_progress(Some(&p3), &err).unwrap();
        assert_eq!(failed.machine_id.as_deref(), Some("m1"));
        assert_eq!(failed.error, Some(err.clone()));
        assert_eq!(fail_progress(Some(&p1), &err), None, "nothing to retry yet");
    }

    fn failed_progress(id: &str) -> ProgressView {
        ProgressView {
            kind: "connect".into(),
            step: "installing".into(),
            machine_id: Some(id.into()),
            error: Some(ErrorView::from(SandboxError::Setup {
                stderr_tail: "tar: bad".into(),
            })),
        }
    }

    #[test]
    fn a_failed_progress_ends_when_its_sandbox_is_gone() {
        let listed = vec![machine("m1", "org-me", "agent-browser", "running", false)];
        let kept = prune_progress(Some(failed_progress("m1")), &listed);
        assert_eq!(kept, Some(failed_progress("m1")), "still listed");
        assert_eq!(
            prune_progress(Some(failed_progress("m2")), &listed),
            None,
            "not listed"
        );
        let deleted = vec![machine("m1", "org-me", "agent-browser", "deleted", false)];
        assert_eq!(prune_progress(Some(failed_progress("m1")), &deleted), None);
        // A running create may not be listed yet: its flow ends it.
        let mut running = failed_progress("m2");
        running.error = None;
        assert_eq!(
            prune_progress(Some(running.clone()), &listed),
            Some(running)
        );
    }

    #[tokio::test]
    async fn deleting_the_sandbox_of_a_failed_progress_ends_it() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let m1 = serde_json::json!({
            "id": "m1", "name": "n", "state": "running",
            "template": {"id": "agent-browser"}, "spec": {"vcpus": 2, "memMiB": 4096},
            "metadata": {}, "orgId": "org-me"
        });
        Mock::given(method("DELETE"))
            .and(path("/v1/machines/m1"))
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "id": "op1", "machineId": "m1", "type": "delete", "state": "succeeded",
                "machine": m1
            })))
            .mount(&server)
            .await;
        // A list that still shows the sandbox: the delete itself ends the progress.
        Mock::given(method("GET"))
            .and(path("/v1/machines"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"machines": [m1], "nextCursor": null})),
            )
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let core = Arc::new(Core::new(
            base.clone(),
            Arc::new(MemoryKeyStore::default()),
            Arc::new(MemorySshStore::default()),
            Arc::new(MemoryConnectedStore::default()),
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            PathBuf::from("/nonexistent"),
        ));
        let session = Session::load_from(
            &base,
            Some("ibk_test".into()),
            Arc::new(MemoryStore::default()),
        )
        .unwrap();
        core.install_session(session);
        core.update(|i| {
            i.data.me = Some(me());
            i.state.progress = Some(failed_progress("m1"));
        });
        core.delete("m1").await.unwrap();
        assert_eq!(core.state().progress, None);
    }

    #[test]
    fn dismiss_ends_the_progress_and_its_copy_of_the_error_only() {
        let core = core_with(Arc::new(MemoryConnectedStore::default()));
        let p = failed_progress("m1");
        core.update(|i| {
            i.state.error = p.error.clone();
            i.state.progress = Some(p.clone());
        });
        core.dismiss_progress();
        assert_eq!(core.state().progress, None);
        assert_eq!(core.state().error, None);
        core.update(|i| {
            i.state.error = Some(ErrorView::other("Unknown organization"));
            i.state.progress = Some(p.clone());
        });
        core.dismiss_progress();
        assert_eq!(core.state().progress, None);
        assert_eq!(
            core.state().error,
            Some(ErrorView::other("Unknown organization")),
            "another error stays"
        );
    }

    #[test]
    fn a_new_desktop_version_brings_the_connected_sandboxes() {
        let core = core_with(Arc::new(MemoryConnectedStore::default()));
        let mut i = core.lock();
        i.data = data();
        assert!(
            i.set_desktop_version(Some("0.1.10".into())).is_none(),
            "first read"
        );
        let job = i.set_desktop_version(Some("0.1.11".into())).unwrap();
        assert_eq!(job.version, "0.1.11");
        assert_eq!(job.ids, ["a", "b"]);
        assert!(i.set_desktop_version(None).is_none());
        assert_eq!(i.state.t3, None);
    }

    #[tokio::test]
    async fn the_ssh_command_leaves_the_error_line_alone() {
        let ssh = Arc::new(MemorySshStore::default());
        ssh.save("m1", &ssh_token()).unwrap();
        let base = api_base(None);
        let core = Core::new(
            base.clone(),
            Arc::new(MemoryKeyStore::default()),
            ssh,
            Arc::new(MemoryConnectedStore::default()),
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            PathBuf::from("/nonexistent"),
        );
        let boom = Some(ErrorView::other("boom"));
        core.update(|i| i.state.error = boom.clone());
        assert!(core.ssh_command("m1").await.is_err(), "signed out");
        assert_eq!(core.state().error, boom, "a failure shows in the row");
        // An API key session; the stored token is reused, so no request is made.
        let session = Session::load_from(
            &base,
            Some("ibk_test".into()),
            Arc::new(MemoryStore::default()),
        )
        .unwrap();
        core.install_session(session);
        core.update(|i| i.state.error = boom.clone());
        assert_eq!(
            core.ssh_command("m1").await.unwrap(),
            "ssh ibssh_x@ssh.example.test"
        );
        assert_eq!(core.state().error, boom, "success does not clear it");
    }

    #[test]
    fn newer_release_compares_versions_and_checks_the_link() {
        let body = |tag: &str, url: &str| format!(r#"{{"tag_name":"{tag}","html_url":"{url}"}}"#);
        let ok = newer_release(
            "0.1.0",
            &body(
                "v0.2.0",
                "https://github.com/InsForge/instabox-bridge/releases/tag/v0.2.0",
            ),
        )
        .unwrap();
        assert_eq!(ok.version, "0.2.0");
        assert!(ok.url.ends_with("v0.2.0"));
        assert_eq!(
            newer_release("0.2.0", &body("v0.2.0", "https://github.com/x")),
            None
        );
        assert_eq!(
            newer_release("0.3.0", &body("v0.2.0", "https://github.com/x")),
            None
        );
        assert_eq!(newer_release("0.1.0", "not json"), None);
        assert_eq!(newer_release("0.1.0", r#"{"message":"Not Found"}"#), None);
        let odd = newer_release("0.1.0", &body("v0.2.0", "https://evil.example/x")).unwrap();
        assert_eq!(odd.url, RELEASES_PAGE);
    }

    #[test]
    fn small_decisions() {
        assert_eq!(refresh_interval(true), Duration::from_secs(5));
        assert_eq!(refresh_interval(false), Duration::from_secs(30));
        assert_eq!(api_base(None).as_str(), "https://api.instabox.sh/");
        assert_eq!(
            api_base(Some("http://localhost:8080".into())).as_str(),
            "http://localhost:8080/"
        );
        assert_eq!(
            api_base(Some("nope".into())).as_str(),
            "https://api.instabox.sh/"
        );
        assert!(allowed_url("https://github.com/x"));
        assert!(allowed_url("t3code://pair?pairingUrl=x"));
        assert!(!allowed_url("file:///etc/passwd"));
        assert!(!allowed_url("javascript:alert(1)"));
        assert_eq!(parse_provider("github"), Some(Provider::GitHub));
        assert_eq!(parse_provider("x"), None);
        assert_eq!(parse_mcp_client("claudeCode"), Some(McpClient::ClaudeCode));
        assert_eq!(parse_mcp_client("codex"), Some(McpClient::Codex));
        assert_eq!(mcp_status(false, false, true).claude_code, None);
        assert_eq!(mcp_status(true, false, true).claude_code, Some(false));
    }

    // ---- Core ----

    fn tokens() -> Tokens {
        // The real clock: the session must not refresh before the logout call.
        Tokens {
            access_token: "iba_a".into(),
            access_expires_at: Utc::now() + chrono::Duration::hours(1),
            refresh_token: "ibr_a".into(),
            refresh_expires_at: Utc::now() + chrono::Duration::days(30),
        }
    }

    fn ssh_token() -> crate::api::SshToken {
        crate::api::SshToken {
            host: "ssh.example.test".into(),
            port: 22,
            token: "ibssh_x".into(),
            command: "ssh ibssh_x@ssh.example.test".into(),
            expires_at: Some(Utc::now() + chrono::Duration::days(20)),
            host_key_fingerprint: None,
        }
    }

    /// Signs out a token session against a mock API whose logout answers `status`.
    async fn sign_out_with_logout(status: u16) {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/logout"))
            .and(header("authorization", "Bearer iba_a"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let store = Arc::new(MemoryStore::with(tokens()));
        let keys = Arc::new(MemoryKeyStore::default());
        keys.save("ibk_secret").unwrap();
        let ssh = Arc::new(MemorySshStore::default());
        ssh.save("a", &ssh_token()).unwrap();
        ssh.save("e", &ssh_token()).unwrap();
        let connected = Arc::new(MemoryConnectedStore::default());
        connected.add(7, "a").unwrap();
        let emitted = Arc::new(Mutex::new(Vec::<AppState>::new()));
        let sink = emitted.clone();
        let core = Core::new(
            base.clone(),
            keys.clone(),
            ssh.clone(),
            connected.clone(),
            Arc::new(|_| Ok(())),
            Arc::new(move |s| sink.lock().unwrap().push(s.clone())),
            PathBuf::from("/nonexistent"),
        );
        let session = Session::load_from(&base, None, store.clone()).expect("session");
        core.install_session(session);
        core.update(|i| {
            i.data = data();
            i.data.connected_user = Some(7);
            i.rebuild();
            i.state.t3 = Some("0.1.10".into());
            i.state.start_at_login = true;
        });
        assert!(core.state().signed_in);
        assert!(!core.state().sandboxes.is_empty());

        core.sign_out().await.unwrap();

        assert_eq!(store.load().unwrap(), None, "tokens cleared");
        assert_eq!(keys.load().unwrap(), None, "MCP key cleared");
        assert_eq!(ssh.load("a").unwrap(), None, "SSH tokens cleared");
        assert_eq!(ssh.load("e").unwrap(), None);
        let s = core.state();
        assert!(!s.signed_in);
        assert!(s.sandboxes.is_empty() && s.orgs.is_empty() && s.login.is_none());
        assert!(core.lock().data.connected.is_empty(), "no connected set");
        assert!(connected.load(7).contains("a"), "kept on disk for user 7");
        assert_eq!(s.t3.as_deref(), Some("0.1.10"), "desktop facts stay");
        assert!(s.start_at_login);
        assert!(
            !emitted.lock().unwrap().last().unwrap().signed_in,
            "state emitted"
        );
        assert!(core.client().is_err(), "no client after sign-out");
        for _ in 0..50 {
            if !server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Dropping the server checks the one logout call.
    }

    #[tokio::test]
    async fn sign_out_ends_the_server_session_and_clears_every_secret() {
        sign_out_with_logout(204).await;
    }

    #[tokio::test]
    async fn sign_out_goes_on_when_the_logout_call_fails() {
        sign_out_with_logout(500).await;
    }

    /// A core signed in with `tok` against `server`, plus its token and key stores.
    fn core_with_session(
        server_uri: &str,
        tok: Tokens,
    ) -> (
        Core,
        Arc<MemoryStore>,
        Arc<MemoryKeyStore>,
        Arc<MemorySshStore>,
    ) {
        let base = Url::parse(server_uri).unwrap();
        let store = Arc::new(MemoryStore::with(tok));
        let keys = Arc::new(MemoryKeyStore::default());
        keys.save("ibk_secret").unwrap();
        let ssh = Arc::new(MemorySshStore::default());
        ssh.save("a", &ssh_token()).unwrap();
        let core = Core::new(
            base.clone(),
            keys.clone(),
            ssh.clone(),
            Arc::new(MemoryConnectedStore::default()),
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            PathBuf::from("/nonexistent"),
        );
        let session = Session::load_from(&base, None, store.clone()).expect("session");
        core.install_session(session);
        (core, store, keys, ssh)
    }

    #[tokio::test]
    async fn sign_out_clears_secrets_without_waiting_for_the_server() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_secs(10)))
            .mount(&server)
            .await;
        let (core, store, keys, ssh) = core_with_session(&server.uri(), tokens());
        let started = std::time::Instant::now();
        core.sign_out().await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "sign_out waited for the server"
        );
        assert_eq!(store.load().unwrap(), None, "tokens cleared");
        assert_eq!(keys.load().unwrap(), None, "MCP key cleared");
        assert_eq!(ssh.load("a").unwrap(), None, "SSH tokens cleared");
    }

    #[tokio::test]
    async fn sign_out_never_refreshes_and_logs_out_with_the_captured_token() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/logout"))
            .and(header("authorization", "Bearer iba_a"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        // The access token is about to expire: bearer() would refresh it.
        let mut tok = tokens();
        tok.access_expires_at = Utc::now() + chrono::Duration::seconds(5);
        let (core, store, _keys, _ssh) = core_with_session(&server.uri(), tok);
        core.sign_out().await.unwrap();
        assert_eq!(store.load().unwrap(), None);
        for _ in 0..50 {
            let n = server.received_requests().await.unwrap_or_default().len();
            if n > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Dropping the server checks: one logout with the old token, no refresh.
    }

    #[tokio::test]
    async fn sign_out_without_a_session_still_clears_the_mcp_key() {
        let keys = Arc::new(MemoryKeyStore::default());
        keys.save("ibk_secret").unwrap();
        let core = Core::new(
            api_base(None),
            keys.clone(),
            Arc::new(MemorySshStore::default()),
            Arc::new(MemoryConnectedStore::default()),
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            PathBuf::from("/nonexistent"),
        );
        core.sign_out().await.unwrap();
        assert_eq!(keys.load().unwrap(), None);
    }

    #[test]
    fn failures_of_small_commands_set_state_error() {
        let core = Core::new(
            api_base(None),
            Arc::new(MemoryKeyStore::default()),
            Arc::new(MemorySshStore::default()),
            Arc::new(MemoryConnectedStore::default()),
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            PathBuf::from("/nonexistent"),
        );
        assert!(core.switch_org("nope").is_err());
        assert_eq!(core.state().error.unwrap().message, "Unknown organization");
        assert!(core.open_url("file:///etc/passwd").is_err());
        assert_eq!(
            core.state().error.unwrap().message,
            "This link cannot be opened"
        );
        core.open_url("https://github.com/x").unwrap();
        assert_eq!(core.state().error, None, "next success clears it");
    }

    #[test]
    fn settle_sets_and_clears_the_error() {
        let core = Core::new(
            api_base(None),
            Arc::new(MemoryKeyStore::default()),
            Arc::new(MemorySshStore::default()),
            Arc::new(MemoryConnectedStore::default()),
            Arc::new(|_| Ok(())),
            Arc::new(|_| {}),
            PathBuf::from("/nonexistent"),
        );
        assert!(core.settle::<()>(Err(ErrorView::other("boom"))).is_err());
        assert_eq!(core.state().error, Some(ErrorView::other("boom")));
        core.settle(Ok(())).unwrap();
        assert_eq!(core.state().error, None);
    }

    #[tokio::test]
    async fn sign_out_during_a_refresh_keeps_the_store_empty_and_logs_out() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let rotated = Tokens {
            access_token: "iba_new".into(),
            access_expires_at: Utc::now() + chrono::Duration::hours(1),
            refresh_token: "ibr_new".into(),
            refresh_expires_at: Utc::now() + chrono::Duration::days(30),
        };
        Mock::given(method("POST"))
            .and(path("/v1/auth/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::to_value(&rotated).unwrap())
                    .set_delay(Duration::from_millis(800)),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let mut tok = tokens();
        tok.access_expires_at = Utc::now() + chrono::Duration::seconds(5);
        let (core, store, _keys, _ssh) = core_with_session(&server.uri(), tok);
        // An API call already in flight (a poll), holding the old session: it refreshes.
        let s = core.lock().session.clone().unwrap();
        let call = tokio::spawn(async move { s.bearer().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        core.sign_out().await.unwrap();
        assert_eq!(store.load().unwrap(), None, "cleared right after sign_out");
        let got = call.await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let reqs = server.received_requests().await.unwrap_or_default();
        let logouts = reqs
            .iter()
            .filter(|r| r.url.path() == "/v1/auth/logout")
            .count();
        assert!(
            got.is_err(),
            "the late refresh reports a signed-out session"
        );
        assert_eq!(logouts, 1, "the logout was sent with the token it had");
        assert_eq!(
            store.load().unwrap(),
            None,
            "rotated tokens saved back after sign_out"
        );
    }

    struct FailingConnected;
    impl ConnectedStore for FailingConnected {
        fn load(&self, _: i64) -> std::collections::HashSet<String> {
            Default::default()
        }
        fn add(&self, _: i64, _: &str) -> Result<(), String> {
            Err("disk full".into())
        }
        fn remove(&self, _: i64, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_failed_connected_list_save_stays_visible_after_connect() {
        let core = core_with(Arc::new(FailingConnected));
        core.update(|i| {
            i.data.machines = vec![machine("t", "org-me", "agent-browser", "running", true)];
            i.data.me = Some(me());
            i.data.orgs = data().orgs;
            i.rebuild();
        });
        core.finish_connect(
            Ok(connected_result("t", "0.1.11")),
            "0.1.11",
            core.account(),
        )
        .await
        .unwrap();
        let e = core.state().error.expect("error shown");
        assert!(e.message.contains("disk full"), "{e:?}");
    }

    fn machine_json() -> serde_json::Value {
        serde_json::json!({
            "id": "m1", "name": "n", "state": "running", "revision": 1,
            "template": {"id": "agent-browser", "version": 1, "digest": "d"},
            "spec": {"vcpus": 2, "memMiB": 4096, "persistentDiskGiB": 10},
            "runtime": {"ready": true}, "autoSuspendAfterSeconds": 1800,
            "maxRunMinutes": -1, "runStartedAt": null, "cooldownUntil": null,
            "metadata": {}, "orgId": "o1", "free": false, "operationId": null
        })
    }

    /// A mock API for one running sandbox `m1` that can pair; `delay` slows the pairing.
    async fn pairing_server(delay: Duration) -> (wiremock::MockServer, Url) {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/machines/m1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(machine_json()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/machines"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"machines": [], "nextCursor": null})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/exec"))
            .and(body_string_contains("t3 pair"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!(
                        {"stdout": "Token: abc123\n", "stderr": "", "exitCode": 0}
                    ))
                    .set_delay(delay),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/machines/m1/ports"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!(
                {"id": "p1", "port": 3774, "url": "https://m1.example.test", "access": "public"}
            )))
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        (server, base)
    }

    #[tokio::test]
    async fn a_failed_connected_list_save_stays_visible_after_pair_again() {
        let (_server, base) = pairing_server(Duration::ZERO).await;
        let core = core_on(base.clone(), Arc::new(FailingConnected));
        sign_in_as(&core, &base, 7);
        core.pair_again("m1").await.unwrap();
        let e = core.state().error.expect("error shown");
        assert!(e.message.contains("disk full"), "{e:?}");
    }

    #[tokio::test]
    async fn pair_again_in_flight_during_sign_out_remembers_nothing() {
        let (_server, base) = pairing_server(Duration::from_millis(600)).await;
        let store = Arc::new(MemoryConnectedStore::default());
        let core = core_on(base.clone(), store.clone());
        sign_in_as(&core, &base, 7);
        let c = core.clone();
        let call = tokio::spawn(async move { c.pair_again("m1").await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        core.sign_out().await.unwrap();
        let _ = call.await.unwrap();
        // Another account signs in meanwhile.
        sign_in_as(&core, &base, 8);
        assert!(store.load(7).is_empty() && store.load(8).is_empty());
        assert!(core.lock().data.connected.is_empty());
    }

    #[tokio::test]
    async fn a_connect_result_after_sign_out_is_dropped() {
        let store = Arc::new(MemoryConnectedStore::default());
        let core = core_with(store.clone());
        core.update(|i| i.data.me = Some(me()));
        let started = core.account();
        core.sign_out().await.unwrap();
        // The same user signs in again: still a new account count.
        let base = Url::parse("http://127.0.0.1:9").unwrap();
        sign_in_as(&core, &base, 7);
        let before = core.state();
        core.finish_connect(Ok(connected_result("t", "0.1.11")), "0.1.11", started)
            .await
            .unwrap();
        assert!(store.load(7).is_empty(), "nothing remembered");
        assert!(core.lock().data.connected.is_empty());
        assert_eq!(core.state().error, before.error);
    }

    #[tokio::test]
    async fn a_refresh_started_for_one_account_does_not_fill_the_next() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/machines"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({
                        "machines": [machine_json()], "nextCursor": null
                    }))
                    .set_delay(Duration::from_millis(600)),
            )
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let core = core_on(base.clone(), Arc::new(MemoryConnectedStore::default()));
        sign_in_as(&core, &base, 7);
        let c = core.clone();
        let call = tokio::spawn(async move { c.refresh(false).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        core.sign_out().await.unwrap();
        sign_in_as(&core, &base, 8);
        call.await.unwrap();
        assert!(core.lock().data.machines.is_empty(), "A's machines dropped");
        assert_ne!(core.lock().data.connected_user, Some(7));
    }

    #[tokio::test]
    async fn an_unreadable_connected_list_is_reported_once_and_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        std::fs::write(&path, "{not json").unwrap();
        let (_server, base) = listing_server().await;
        let core = core_on(
            base.clone(),
            Arc::new(FileConnectedStore::new(path.clone())),
        );
        sign_in_as(&core, &base, 7);
        core.refresh(false).await;
        let e = core.state().error.expect("error shown");
        assert!(
            e.message
                .contains("Could not read the list of connected sandboxes"),
            "{e:?}"
        );
        assert!(core.lock().data.connected.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"{not json");
    }

    #[tokio::test]
    async fn a_pair_again_that_fails_after_sign_out_leaves_no_error_for_the_next_account() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/machines/m1"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(serde_json::json!({"error": {
                        "code": "boom", "message": "failed", "retryable": false}}))
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let core = core_on(base.clone(), Arc::new(MemoryConnectedStore::default()));
        sign_in_as(&core, &base, 7);
        let c = core.clone();
        let call = tokio::spawn(async move { c.pair_again("m1").await });
        tokio::time::sleep(Duration::from_millis(150)).await;
        core.sign_out().await.unwrap();
        sign_in_as(&core, &base, 8);
        assert!(call.await.unwrap().is_err(), "the caller still sees it");
        assert_eq!(core.state().error, None, "the new account shows no error");
    }

    #[tokio::test]
    async fn a_delete_finishing_after_an_account_switch_leaves_the_new_list_alone() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/machines/m1"))
            .respond_with(
                ResponseTemplate::new(202)
                    .set_body_json(serde_json::json!({
                        "id": "op1", "machineId": "m1", "type": "delete",
                        "state": "succeeded", "error": null, "machine": null
                    }))
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/machines"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"machines": [], "nextCursor": null})),
            )
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let store = Arc::new(MemoryConnectedStore::default());
        store.add(8, "m1").unwrap();
        let core = core_on(base.clone(), store.clone());
        sign_in_as(&core, &base, 7);
        let c = core.clone();
        let call = tokio::spawn(async move { c.delete("m1").await });
        tokio::time::sleep(Duration::from_millis(150)).await;
        core.sign_out().await.unwrap();
        sign_in_as(&core, &base, 8);
        core.update(|i| {
            i.data.connected_user = Some(8);
            i.data.connected.insert("m1".into());
        });
        let _ = call.await.unwrap();
        assert!(store.load(8).contains("m1"), "user 8's list is untouched");
        assert!(core.lock().data.connected.contains("m1"));
    }

    #[tokio::test]
    async fn progress_from_a_flow_of_an_old_account_is_dropped() {
        let base = Url::parse("http://127.0.0.1:9").unwrap();
        let core = core_on(base.clone(), Arc::new(MemoryConnectedStore::default()));
        sign_in_as(&core, &base, 7);
        let old = core.account();
        let stale = core.progress_cb(old);
        core.sign_out().await.unwrap();
        sign_in_as(&core, &base, 8);
        // The old flow starts and moves its progress after the switch: nothing shows.
        core.begin_progress("create", "creating", None, old);
        stale(Step::Pairing);
        assert_eq!(core.state().progress, None);
        // The new account's own progress is not moved by the old flow's callback.
        core.begin_progress("connect", "connecting", Some("m2".into()), core.account());
        let before = core.state().progress;
        stale(Step::Pairing);
        assert_eq!(core.state().progress, before);
    }
}
