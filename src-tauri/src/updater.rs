//! Updates of InstaBox itself. A check reads the update manifest of the latest release and
//! never downloads. The app checks on a timer, and Settings has Check for updates. A newer
//! version puts a dot on the Settings button and an Update button in Settings. Only that click
//! downloads the update, checks its signature, installs it and restarts the app. Instabox
//! Preview and dev builds never check.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use semver::Version;
use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use crate::errors::ErrorView;

/// The first check, this long after launch.
pub const FIRST_CHECK: Duration = Duration::from_secs(15);
/// The next checks, this often.
pub const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateStep {
    /// No check has said anything yet. Offer Check for updates.
    #[default]
    Idle,
    /// Check for updates runs.
    Checking,
    /// Check for updates found nothing newer.
    UpToDate,
    /// Check for updates failed: `error` says why.
    CheckFailed,
    /// `version` is newer. Offer Update.
    Available,
    /// `percent` says how far, once the size is known.
    Downloading,
    /// The app restarts when the install is done.
    Installing,
    /// The update failed: `error` says why. Offer Retry.
    Failed,
}

/// What Settings shows under Updates, and whether the Settings button has a dot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateView {
    /// This build's version.
    pub current: String,
    /// Whether this build updates itself (see `enabled`).
    pub enabled: bool,
    pub step: UpdateStep,
    /// The newer version on offer, from Available on.
    pub version: Option<String>,
    pub percent: Option<u8>,
    pub error: Option<ErrorView>,
}

/// What an install reports while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// `bytes` downloaded of `total`. No total when the server does not send one.
    Downloaded { bytes: u64, total: Option<u64> },
    /// The download is checked and installs now.
    Installing,
}

pub type OnProgress = Arc<dyn Fn(Progress) + Send + Sync>;
/// Puts the view in the app state, which sends it to the popover.
pub type Show = Arc<dyn Fn(UpdateView) + Send + Sync>;
/// Quits the app through its own exit, so its cleanup runs, and starts the installed version.
pub type Restart = Arc<dyn Fn() + Send + Sync>;

/// Where updates come from: the updater plugin in the app, a fake in tests.
#[async_trait]
pub trait Source: Send + Sync {
    /// Reads the update manifest: the version it offers for this computer, if any. Never
    /// downloads.
    async fn check(&self) -> Result<Option<Version>, String>;
    /// Downloads `version`, checks its signature and installs it. On Windows a successful
    /// install quits the app instead of returning.
    async fn install(&self, version: &Version, progress: OnProgress) -> Result<(), String>;
}

/// Whether this build updates itself: only a release build of InstaBox. Instabox Preview has
/// its own identifier and must never take InstaBox's releases. A dev build is not installed
/// where an update could replace it.
pub fn enabled(identifier: &str, release: bool) -> bool {
    release && identifier == crate::APP_IDENTIFIER
}

/// Whether `offered` is newer than `current`. An equal or older version is not, and build
/// metadata does not count.
pub fn is_newer(current: &Version, offered: &Version) -> bool {
    offered.cmp_precedence(current).is_gt()
}

/// How much of the download is done; None while the total is unknown.
fn percent(bytes: u64, total: Option<u64>) -> Option<u8> {
    let total = total.filter(|t| *t > 0)?;
    Some((bytes.min(total) * 100 / total) as u8)
}

/// Where updates are in this run.
#[derive(Debug, Default)]
struct Status {
    step: UpdateStep,
    /// The newer version the last check found.
    version: Option<Version>,
    percent: Option<u8>,
    error: Option<String>,
}

impl Status {
    fn view(&self, current: &Version, enabled: bool) -> UpdateView {
        UpdateView {
            current: current.to_string(),
            enabled,
            step: self.step,
            version: self.version.as_ref().map(Version::to_string),
            percent: self.percent,
            error: self.error.clone().map(ErrorView::other),
        }
    }

    fn busy(&self) -> bool {
        matches!(self.step, UpdateStep::Downloading | UpdateStep::Installing)
    }

    /// Check for updates starts. False while an update installs.
    fn checking(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        self.step = UpdateStep::Checking;
        self.error = None;
        true
    }

    /// A check found `offered`; None: the manifest offers nothing for this computer. `manual`:
    /// Check for updates, which says when the app is up to date. Changes nothing while an
    /// update installs. A failed update of the same version keeps its error and Retry, and a
    /// timed check that finds nothing leaves a running Check for updates alone.
    fn found(&mut self, current: &Version, offered: Option<Version>, manual: bool) {
        if self.busy() {
            return;
        }
        let (step, version) = match offered.filter(|v| is_newer(current, v)) {
            Some(v) if self.step == UpdateStep::Failed && self.version.as_ref() == Some(&v) => {
                return;
            }
            Some(v) => (UpdateStep::Available, Some(v)),
            None if !manual && self.step == UpdateStep::Checking => return,
            None if manual || self.step == UpdateStep::UpToDate => (UpdateStep::UpToDate, None),
            None => (UpdateStep::Idle, None),
        };
        *self = Status {
            step,
            version,
            ..Status::default()
        };
    }

    fn check_failed(&mut self, error: String) {
        if self.step == UpdateStep::Checking {
            self.step = UpdateStep::CheckFailed;
            self.error = Some(error);
        }
    }

    /// The Update or Retry click: the version to install, when one is offered and none is
    /// installing yet.
    fn start(&mut self) -> Option<Version> {
        if self.busy() || !matches!(self.step, UpdateStep::Available | UpdateStep::Failed) {
            return None;
        }
        let version = self.version.clone()?;
        self.step = UpdateStep::Downloading;
        self.percent = None;
        self.error = None;
        Some(version)
    }

    fn progress(&mut self, p: Progress) {
        if !self.busy() {
            return;
        }
        (self.step, self.percent) = match p {
            Progress::Downloaded { bytes, total } => {
                (UpdateStep::Downloading, percent(bytes, total))
            }
            Progress::Installing => (UpdateStep::Installing, None),
        };
    }

    fn failed(&mut self, error: String) {
        self.step = UpdateStep::Failed;
        self.percent = None;
        self.error = Some(error);
    }
}

/// Checks for updates on a timer and when asked, and installs one when the user clicks Update.
pub struct Updater {
    current: Version,
    enabled: bool,
    source: Arc<dyn Source>,
    show: Show,
    restart: Restart,
    /// Held while the view is shown, so the popover gets the changes in order. Only commands
    /// that run off the main thread take it, as showing may wait for the main thread (the tray).
    status: Mutex<Status>,
}

impl Updater {
    pub fn new(
        current: Version,
        enabled: bool,
        source: Arc<dyn Source>,
        show: Show,
        restart: Restart,
    ) -> Updater {
        Updater {
            current,
            enabled,
            source,
            show,
            restart,
            status: Mutex::new(Status::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Shows the view as it is: the version, and whether this build updates itself.
    pub fn publish(&self) {
        let status = self.lock();
        (self.show)(status.view(&self.current, self.enabled));
    }

    /// Changes the status, then shows it when the view changed.
    fn change<R>(&self, f: impl FnOnce(&mut Status) -> R) -> R {
        let mut status = self.lock();
        let before = status.view(&self.current, self.enabled);
        let r = f(&mut status);
        let after = status.view(&self.current, self.enabled);
        if after != before {
            (self.show)(after);
        }
        r
    }

    /// Checks `FIRST_CHECK` after launch, then every `CHECK_EVERY`.
    pub async fn run(self: Arc<Self>) {
        if !self.enabled {
            return;
        }
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            self.check().await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    }

    /// A timed check: reads the update manifest and offers a newer version. Never downloads.
    /// A failed check shows nothing.
    pub async fn check(&self) {
        if !self.enabled || self.lock().busy() {
            return;
        }
        if let Ok(offered) = self.source.check().await {
            self.change(|s| s.found(&self.current, offered, false));
        }
    }

    /// Check for updates: the same check, now, showing that it runs and what it found, or
    /// why it failed. Never downloads. Does nothing while an update installs.
    pub async fn check_now(&self) -> Result<(), ErrorView> {
        if !self.enabled {
            return Err(ErrorView::other("Updates are only for released builds"));
        }
        if !self.change(Status::checking) {
            return Ok(());
        }
        match self.source.check().await {
            Ok(offered) => {
                self.change(|s| s.found(&self.current, offered, true));
                Ok(())
            }
            Err(e) => {
                let message = format!("Could not check for updates: {e}");
                self.change(|s| s.check_failed(message.clone()));
                Err(ErrorView::other(message))
            }
        }
    }

    /// The Update or Retry click: downloads the offered version, checks its signature,
    /// installs it and restarts the app. The view shows the progress, and a failure with
    /// Retry. Does nothing when no update is offered or one is installing already.
    pub async fn install(self: &Arc<Self>) -> Result<(), ErrorView> {
        if !self.enabled {
            return Err(ErrorView::other("Updates are only for released builds"));
        }
        let Some(version) = self.change(Status::start) else {
            return Ok(());
        };
        let me = self.clone();
        let progress: OnProgress = Arc::new(move |p| me.change(|s| s.progress(p)));
        match self.source.install(&version, progress).await {
            Ok(()) => {
                (self.restart)();
                Ok(())
            }
            Err(e) => {
                let message = format!("Could not update to {version}: {e}");
                self.change(|s| s.failed(message.clone()));
                Err(ErrorView::other(message))
            }
        }
    }
}

/// The updater plugin: reads the manifest at `plugins.updater.endpoints` and checks each
/// download against `plugins.updater.pubkey`.
pub struct PluginSource {
    app: AppHandle,
    /// The app's own cleanup on quit. A Windows install quits the app without its exit event.
    on_quit: Arc<dyn Fn() + Send + Sync>,
    /// The update the last check found.
    found: Mutex<Option<tauri_plugin_updater::Update>>,
}

impl PluginSource {
    pub fn new(app: AppHandle, on_quit: Arc<dyn Fn() + Send + Sync>) -> PluginSource {
        PluginSource {
            app,
            on_quit,
            found: Mutex::new(None),
        }
    }

    fn updater(&self) -> Result<tauri_plugin_updater::Updater, String> {
        let (app, on_quit) = (self.app.clone(), self.on_quit.clone());
        self.app
            .updater_builder()
            .version_comparator(|current, release| is_newer(&current, &release.version))
            // Replaces the plugin's own hook, which only cleans up Tauri.
            .on_before_exit(move || {
                on_quit();
                app.cleanup_before_exit();
            })
            .build()
            .map_err(|e| e.to_string())
    }

    fn found(&self, version: &Version) -> Option<tauri_plugin_updater::Update> {
        self.found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .filter(|u| Version::parse(&u.version).is_ok_and(|v| v == *version))
    }
}

#[async_trait]
impl Source for PluginSource {
    async fn check(&self) -> Result<Option<Version>, String> {
        let update = self.updater()?.check().await.map_err(|e| e.to_string())?;
        let version = update
            .as_ref()
            .and_then(|u| Version::parse(&u.version).ok());
        *self.found.lock().unwrap_or_else(PoisonError::into_inner) = update;
        Ok(version)
    }

    async fn install(&self, version: &Version, progress: OnProgress) -> Result<(), String> {
        let update = match self.found(version) {
            Some(u) => u,
            None => {
                self.check().await?;
                self.found(version)
                    .ok_or_else(|| "it is no longer offered".to_string())?
            }
        };
        let mut bytes = 0u64;
        let on_chunk = progress.clone();
        // The download checks the signature before it returns.
        let data = update
            .download(
                move |chunk, total| {
                    bytes += chunk as u64;
                    on_chunk(Progress::Downloaded { bytes, total });
                },
                || {},
            )
            .await
            .map_err(|e| e.to_string())?;
        progress(Progress::Installing);
        tokio::task::spawn_blocking(move || update.install(data))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    /// A manifest that offers `offered` (a check fails while `check_fails` is set), and an
    /// install that fails while `fail` is set.
    #[derive(Default)]
    struct Fake {
        offered: Mutex<Option<Version>>,
        check_fails: Mutex<Option<String>>,
        fail: Mutex<Option<String>>,
        checks: AtomicUsize,
        installs: AtomicUsize,
    }

    #[async_trait]
    impl Source for Fake {
        async fn check(&self) -> Result<Option<Version>, String> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = self.check_fails.lock().unwrap().clone() {
                return Err(e);
            }
            Ok(self.offered.lock().unwrap().clone())
        }

        async fn install(&self, _: &Version, progress: OnProgress) -> Result<(), String> {
            self.installs.fetch_add(1, Ordering::SeqCst);
            progress(Progress::Downloaded {
                bytes: 0,
                total: Some(200),
            });
            progress(Progress::Downloaded {
                bytes: 100,
                total: Some(200),
            });
            if let Some(e) = self.fail.lock().unwrap().clone() {
                return Err(e);
            }
            progress(Progress::Installing);
            Ok(())
        }
    }

    struct Run {
        updater: Arc<Updater>,
        source: Arc<Fake>,
        shown: Arc<Mutex<Vec<UpdateView>>>,
        restarts: Arc<AtomicUsize>,
    }

    impl Run {
        fn shown(&self) -> Vec<UpdateView> {
            self.shown.lock().unwrap().clone()
        }

        fn last(&self) -> Option<UpdateView> {
            self.shown().last().cloned()
        }

        fn checks(&self) -> usize {
            self.source.checks.load(Ordering::SeqCst)
        }

        fn installs(&self) -> usize {
            self.source.installs.load(Ordering::SeqCst)
        }

        fn restarts(&self) -> usize {
            self.restarts.load(Ordering::SeqCst)
        }
    }

    /// A launch of InstaBox 0.1.7 whose manifest offers `offered`.
    fn launch(enabled: bool, offered: Option<&str>) -> Run {
        let source = Arc::new(Fake::default());
        *source.offered.lock().unwrap() = offered.map(v);
        let shown = Arc::new(Mutex::new(Vec::new()));
        let restarts = Arc::new(AtomicUsize::new(0));
        let updater = {
            let (shown, restarts) = (shown.clone(), restarts.clone());
            Arc::new(Updater::new(
                v("0.1.7"),
                enabled,
                source.clone(),
                Arc::new(move |u| shown.lock().unwrap().push(u)),
                Arc::new(move || {
                    restarts.fetch_add(1, Ordering::SeqCst);
                }),
            ))
        };
        Run {
            updater,
            source,
            shown,
            restarts,
        }
    }

    /// The view of InstaBox 0.1.7, a build that updates itself.
    fn view(step: UpdateStep, version: Option<&str>, percent: Option<u8>) -> UpdateView {
        UpdateView {
            current: "0.1.7".into(),
            enabled: true,
            step,
            version: version.map(str::to_string),
            percent,
            error: None,
        }
    }

    fn failed(step: UpdateStep, version: Option<&str>, message: &str) -> UpdateView {
        UpdateView {
            error: Some(ErrorView::other(message)),
            ..view(step, version, None)
        }
    }

    #[test]
    fn only_a_strictly_newer_version_counts() {
        let current = v("0.1.7");
        assert!(is_newer(&current, &v("0.1.8")));
        assert!(is_newer(&current, &v("0.2.0")));
        assert!(is_newer(&current, &v("1.0.0")));
        assert!(!is_newer(&current, &v("0.1.7")), "equal");
        assert!(!is_newer(&current, &v("0.1.6")), "older");
        assert!(!is_newer(&current, &v("0.0.9")), "older");
        assert!(!is_newer(&current, &v("0.1.7+build.2")), "build metadata");
        assert!(
            !is_newer(&current, &v("0.1.7-rc.1")),
            "a prerelease of this version"
        );
        assert!(is_newer(&current, &v("0.1.8-rc.1")));
    }

    #[test]
    fn only_release_builds_of_instabox_update_themselves() {
        assert!(enabled("sh.instabox.bridge", true));
        assert!(
            !enabled("sh.instabox.bridge.preview", true),
            "Instabox Preview never takes InstaBox's releases"
        );
        assert!(!enabled("sh.instabox.bridge", false), "a dev build");
        assert!(!enabled("sh.instabox.bridge.preview", false));
    }

    #[test]
    fn the_preview_config_turns_updates_off() {
        let conf = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        let app = conf(include_str!("../tauri.conf.json"));
        let preview = conf(include_str!("../tauri.preview.conf.json"));
        assert!(enabled(app["identifier"].as_str().unwrap(), true));
        assert_eq!(app["bundle"]["createUpdaterArtifacts"], true);
        assert_eq!(
            app["plugins"]["updater"]["endpoints"],
            serde_json::json!([
                "https://github.com/InsForge/instabox-bridge/releases/latest/download/latest.json"
            ])
        );
        assert!(
            !app["plugins"]["updater"]["pubkey"]
                .as_str()
                .unwrap()
                .is_empty()
        );
        // A preview build signs no updater files, so it needs no updater key, and its app has
        // no manifest to read even if it checked.
        assert!(!enabled(preview["identifier"].as_str().unwrap(), true));
        assert_eq!(preview["bundle"]["createUpdaterArtifacts"], false);
        assert_eq!(
            preview["plugins"]["updater"]["endpoints"],
            serde_json::json!([])
        );
    }

    #[test]
    fn the_plugin_reads_the_manifest_release_yml_writes() {
        // The shape the manifest job in .github/workflows/release.yml writes.
        let base = "https://github.com/InsForge/instabox-bridge/releases/download/v0.1.8";
        let manifest = serde_json::json!({
            "version": "0.1.8",
            "notes": "## What's Changed\n* Update from the app",
            "pub_date": "2026-11-25T21:24:33Z",
            "platforms": {
                "darwin-aarch64": { "url": format!("{base}/Instabox_0.1.8_aarch64.app.tar.gz"), "signature": "c2ln" },
                "darwin-x86_64": { "url": format!("{base}/Instabox_0.1.8_x64.app.tar.gz"), "signature": "c2ln" },
                "windows-x86_64": { "url": format!("{base}/Instabox_0.1.8_x64-setup.exe"), "signature": "c2ln" }
            }
        });
        let release: tauri_plugin_updater::RemoteRelease =
            serde_json::from_value(manifest).unwrap();
        assert!(is_newer(&v("0.1.7"), &release.version));
        assert!(release.pub_date.is_some());
        for (target, file) in [
            ("darwin-aarch64", "Instabox_0.1.8_aarch64.app.tar.gz"),
            ("darwin-x86_64", "Instabox_0.1.8_x64.app.tar.gz"),
            ("windows-x86_64", "Instabox_0.1.8_x64-setup.exe"),
        ] {
            assert_eq!(
                release.download_url(target).unwrap().as_str(),
                format!("{base}/{file}")
            );
            assert_eq!(release.signature(target).unwrap(), "c2ln");
        }
    }

    #[test]
    fn percent_needs_a_total() {
        assert_eq!(percent(50, Some(200)), Some(25));
        assert_eq!(percent(200, Some(200)), Some(100));
        assert_eq!(percent(300, Some(200)), Some(100));
        assert_eq!(percent(50, None), None);
        assert_eq!(percent(50, Some(0)), None);
    }

    #[test]
    fn publish_shows_the_version_and_whether_updates_are_on() {
        let r = launch(true, None);
        r.updater.publish();
        assert_eq!(r.shown(), [view(UpdateStep::Idle, None, None)]);
        let off = launch(false, None);
        off.updater.publish();
        assert_eq!(
            off.shown(),
            [UpdateView {
                enabled: false,
                ..view(UpdateStep::Idle, None, None)
            }]
        );
    }

    #[tokio::test]
    async fn a_timed_check_offers_a_newer_version_and_never_downloads() {
        let r = launch(true, Some("0.1.8"));
        r.updater.check().await;
        assert_eq!(
            r.shown(),
            [view(UpdateStep::Available, Some("0.1.8"), None)]
        );
        // The same offer again shows nothing new.
        r.updater.check().await;
        assert_eq!(r.shown().len(), 1);
        // A newer one replaces it.
        *r.source.offered.lock().unwrap() = Some(v("0.1.9"));
        r.updater.check().await;
        assert_eq!(
            r.last(),
            Some(view(UpdateStep::Available, Some("0.1.9"), None))
        );
        // An offer the manifest drops goes away.
        *r.source.offered.lock().unwrap() = None;
        r.updater.check().await;
        assert_eq!(r.last(), Some(view(UpdateStep::Idle, None, None)));
        assert_eq!(r.installs(), 0);
        assert_eq!(r.restarts(), 0);
    }

    #[tokio::test]
    async fn an_equal_or_older_version_is_not_offered() {
        for offered in [Some("0.1.7"), Some("0.1.6"), Some("0.1.7+build.2"), None] {
            let r = launch(true, offered);
            r.updater.check().await;
            assert_eq!(r.shown(), Vec::<UpdateView>::new(), "{offered:?}");
            r.updater.check_now().await.unwrap();
            assert_eq!(
                r.last(),
                Some(view(UpdateStep::UpToDate, None, None)),
                "{offered:?}"
            );
        }
    }

    #[tokio::test]
    async fn check_for_updates_shows_checking_then_what_it_found() {
        let r = launch(true, None);
        r.updater.check_now().await.unwrap();
        assert_eq!(
            r.shown(),
            [
                view(UpdateStep::Checking, None, None),
                view(UpdateStep::UpToDate, None, None),
            ]
        );
        // A timed check that finds nothing keeps saying so.
        r.updater.check().await;
        assert_eq!(r.shown().len(), 2);
        *r.source.offered.lock().unwrap() = Some(v("0.1.8"));
        r.updater.check_now().await.unwrap();
        assert_eq!(
            r.shown()[2..],
            [
                view(UpdateStep::Checking, None, None),
                view(UpdateStep::Available, Some("0.1.8"), None),
            ]
        );
        assert_eq!(r.checks(), 3);
        assert_eq!(r.installs(), 0, "a check never downloads");
    }

    #[tokio::test]
    async fn a_failed_check_for_updates_shows_why() {
        let r = launch(true, Some("0.1.8"));
        *r.source.check_fails.lock().unwrap() = Some("offline".into());
        let e = r.updater.check_now().await.unwrap_err();
        assert_eq!(e.message, "Could not check for updates: offline");
        assert_eq!(
            r.last(),
            Some(failed(
                UpdateStep::CheckFailed,
                None,
                "Could not check for updates: offline"
            ))
        );
        // A failed timed check shows nothing.
        r.updater.check().await;
        assert_eq!(r.shown().len(), 2);
        // Checking again clears it.
        *r.source.check_fails.lock().unwrap() = None;
        r.updater.check_now().await.unwrap();
        assert_eq!(
            r.last(),
            Some(view(UpdateStep::Available, Some("0.1.8"), None))
        );
    }

    #[tokio::test]
    async fn a_build_without_updates_never_checks_or_installs() {
        let r = launch(false, Some("0.1.8"));
        r.updater.check().await;
        let e = r.updater.check_now().await.unwrap_err();
        assert_eq!(e.message, "Updates are only for released builds");
        assert!(r.updater.install().await.is_err());
        r.updater.clone().run().await;
        assert_eq!(r.checks(), 0);
        assert_eq!(r.installs(), 0);
        assert_eq!(r.shown(), Vec::<UpdateView>::new());
    }

    #[tokio::test]
    async fn update_downloads_installs_then_restarts() {
        let r = launch(true, Some("0.1.8"));
        // Nothing offered yet: the click does nothing.
        r.updater.install().await.unwrap();
        assert_eq!(r.installs(), 0);
        r.updater.check().await;
        r.updater.install().await.unwrap();
        let v8 = Some("0.1.8");
        assert_eq!(
            r.shown(),
            [
                view(UpdateStep::Available, v8, None),
                view(UpdateStep::Downloading, v8, None),
                view(UpdateStep::Downloading, v8, Some(0)),
                view(UpdateStep::Downloading, v8, Some(50)),
                view(UpdateStep::Installing, v8, None),
            ]
        );
        assert_eq!(r.installs(), 1);
        assert_eq!(r.restarts(), 1);
    }

    #[tokio::test]
    async fn a_failed_update_shows_why_and_retry_installs_it() {
        let r = launch(true, Some("0.1.8"));
        *r.source.fail.lock().unwrap() = Some("signature mismatch".into());
        r.updater.check().await;
        let e = r.updater.install().await.unwrap_err();
        assert_eq!(e.message, "Could not update to 0.1.8: signature mismatch");
        assert_eq!(
            r.last(),
            Some(failed(UpdateStep::Failed, Some("0.1.8"), &e.message))
        );
        assert_eq!(r.restarts(), 0);
        // A timed check keeps the error and Retry on screen.
        let before = r.shown().len();
        r.updater.check().await;
        assert_eq!(r.shown().len(), before);
        // Retry.
        *r.source.fail.lock().unwrap() = None;
        r.updater.install().await.unwrap();
        assert_eq!(
            r.last(),
            Some(view(UpdateStep::Installing, Some("0.1.8"), None))
        );
        assert_eq!(r.installs(), 2);
        assert_eq!(r.restarts(), 1);
    }

    #[test]
    fn an_install_in_progress_ignores_checks_and_clicks() {
        let current = v("0.1.7");
        let mut s = Status::default();
        s.found(&current, Some(v("0.1.8")), false);
        assert_eq!(s.start(), Some(v("0.1.8")));
        s.progress(Progress::Downloaded {
            bytes: 10,
            total: Some(100),
        });
        let downloading = s.view(&current, true);
        s.found(&current, Some(v("0.1.9")), false);
        s.found(&current, None, true);
        assert!(!s.checking(), "Check for updates");
        assert_eq!(s.start(), None, "a second click");
        assert_eq!(s.view(&current, true), downloading);
    }

    #[test]
    fn a_timed_check_leaves_a_running_check_for_updates_alone() {
        let current = v("0.1.7");
        let mut s = Status::default();
        assert!(s.checking());
        s.found(&current, None, false);
        assert_eq!(s.step, UpdateStep::Checking);
        // What it finds still counts.
        s.found(&current, Some(v("0.1.8")), false);
        assert_eq!(s.step, UpdateStep::Available);
        // A failure of the click's own check, after that, changes nothing.
        s.check_failed("offline".into());
        assert_eq!(
            s.view(&current, true),
            view(UpdateStep::Available, Some("0.1.8"), None)
        );
    }
}
