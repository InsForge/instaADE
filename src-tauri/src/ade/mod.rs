//! The ADEs (agentic development environments) the app sets up on a sandbox and opens from
//! this computer. Each ADE has its own module; `Ade` hands out its parts and `ALL` lists them.
//! The flows that use them (connect, sync, reinstall) are in `sandbox`.

pub mod t3;

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Every ADE the app knows, in the order the app lists them.
pub const ALL: &[Ade] = &[Ade::T3];

/// Exit code of every ADE's setup script when the ADE is busy and its update waits.
pub const SETUP_EXIT_BUSY: i32 = 3;

/// Where an ADE's setup puts its idle probe. instabox-guest-agent runs every file here.
pub const ACTIVITY_DIR: &str = "/usr/local/lib/instabox/activity.d";

/// An ADE. Its id (`t3`) is its JSON form and the value of a connected sandbox's mark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ade {
    #[default]
    T3,
}

/// A platform the desktop side of an ADE runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    /// This computer's platform.
    pub fn current() -> Option<Platform> {
        if cfg!(target_os = "macos") {
            Some(Platform::MacOs)
        } else if cfg!(windows) {
            Some(Platform::Windows)
        } else if cfg!(target_os = "linux") {
            Some(Platform::Linux)
        } else {
            None
        }
    }
}

/// How the app opens an ADE on a sandbox.
#[derive(Debug, Clone, Copy)]
pub enum Open {
    /// The desktop app pairs with a server on the sandbox.
    Pair(Pairing),
}

/// A pairing. `script` prints a one-time code on the sandbox and `code` reads it; `link`
/// turns the URL of the published `port` and the code into a link that pairs the desktop
/// app. A paired sandbox opens with `app_url`.
#[derive(Debug, Clone, Copy)]
pub struct Pairing {
    pub script: &'static str,
    pub port: u16,
    pub app_url: &'static str,
    pub code: fn(&str) -> Option<String>,
    pub link: fn(&str, &str) -> String,
}

/// The idle probe an ADE's setup installs as `ACTIVITY_DIR/<name>`. It prints one JSON
/// line, `{"busy": true}` or `{"busy": false}`, so the sandbox sleeps only when the ADE is
/// idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub name: &'static str,
    pub script: &'static str,
}

impl Probe {
    /// The command that runs the probe on the sandbox.
    pub fn path(&self) -> String {
        format!("{ACTIVITY_DIR}/{}", self.name)
    }
}

impl Ade {
    /// The ADE with this id, if the app knows it.
    pub fn from_id(id: &str) -> Option<Ade> {
        ALL.iter().copied().find(|a| a.id() == id)
    }

    pub fn id(self) -> &'static str {
        match self {
            Ade::T3 => t3::ID,
        }
    }

    /// The desktop app's name, such as "T3 Code".
    pub fn name(self) -> &'static str {
        match self {
            Ade::T3 => t3::NAME,
        }
    }

    /// The short name for what runs on the sandbox, such as "T3".
    pub fn short_name(self) -> &'static str {
        match self {
            Ade::T3 => t3::SHORT_NAME,
        }
    }

    pub fn platforms(self) -> &'static [Platform] {
        match self {
            Ade::T3 => t3::PLATFORMS,
        }
    }

    /// True when the desktop app runs on this computer's platform.
    pub fn runs_here(self) -> bool {
        Platform::current().is_some_and(|p| self.platforms().contains(&p))
    }

    /// Where to get the desktop app.
    pub fn download_url(self) -> &'static str {
        match self {
            Ade::T3 => t3::DOWNLOAD_URL,
        }
    }

    /// The scheme of the desktop app's links, which the app may open.
    pub fn url_scheme(self) -> &'static str {
        match self {
            Ade::T3 => t3::URL_SCHEME,
        }
    }

    // ---- this computer ----

    /// The path of the desktop app's CLI, if it is installed.
    pub fn locate(self) -> Option<PathBuf> {
        match self {
            Ade::T3 => t3::locate(),
        }
    }

    /// Asks the CLI at `cli` for its version. Gives up after a few seconds.
    pub fn local_version(self, cli: &Path) -> Result<String> {
        match self {
            Ade::T3 => t3::version(cli),
        }
    }

    /// The version of the desktop app on this computer; None when it is not found or does not
    /// answer. It blocks, so run it off the async runtime.
    pub fn detect(self) -> Option<String> {
        let cli = self.locate()?;
        self.local_version(&cli).ok()
    }

    /// Reads a version from the ADE's `--version` output, here or on a sandbox.
    pub fn parse_version(self, out: &str) -> Option<String> {
        match self {
            Ade::T3 => t3::parse_version(out),
        }
    }

    /// True when the sandbox must run the same version as the desktop app. The app then
    /// installs that version and brings connected sandboxes along when it changes.
    pub fn pins_local_version(self) -> bool {
        match self {
            Ade::T3 => true,
        }
    }

    // ---- the sandbox ----

    /// Prints the ADE's version on the sandbox, or nothing when it is not installed.
    pub fn version_script(self) -> &'static str {
        match self {
            Ade::T3 => t3::VERSION_SCRIPT,
        }
    }

    /// Installs `version` with its probe and starts it. Exits `SETUP_EXIT_BUSY` when the ADE
    /// is busy and the update waits.
    pub fn setup_script(self, version: &str) -> String {
        match self {
            Ade::T3 => t3::setup_script(version),
        }
    }

    /// Stops the ADE's service before a reinstall.
    pub fn stop_script(self) -> &'static str {
        match self {
            Ade::T3 => t3::STOP_SCRIPT,
        }
    }

    pub fn probe(self) -> Probe {
        match self {
            Ade::T3 => t3::PROBE,
        }
    }

    pub fn open(self) -> Open {
        match self {
            Ade::T3 => Open::Pair(t3::PAIRING),
        }
    }

    /// The sandbox port the app publishes for the ADE, if any.
    pub fn port(self) -> Option<u16> {
        match self.open() {
            Open::Pair(p) => Some(p.port),
        }
    }
}

/// The ADEs whose desktop app runs on this computer.
pub fn local() -> Vec<Ade> {
    ALL.iter().copied().filter(|a| a.runs_here()).collect()
}

#[derive(Debug, PartialEq, Eq)]
pub struct ProbeAnswer {
    pub busy: bool,
    pub reason: String,
}

#[derive(Deserialize)]
struct RawProbe {
    busy: bool,
    #[serde(default)]
    reason: String,
}

/// Parse the probe's JSON line. Anything unparsable counts as busy.
pub fn parse_probe(stdout: &str) -> ProbeAnswer {
    stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<RawProbe>(l.trim()).ok())
        .map(|p| ProbeAnswer {
            busy: p.busy,
            reason: p.reason,
        })
        .unwrap_or_else(|| ProbeAnswer {
            busy: true,
            reason: "probe answer unreadable".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_lists_t3_by_its_id() {
        assert_eq!(ALL, &[Ade::T3]);
        assert_eq!(Ade::default(), Ade::T3);
        assert_eq!(Ade::from_id("t3"), Some(Ade::T3));
        assert_eq!(Ade::from_id("other"), None);
        assert_eq!(Ade::from_id(""), None);
    }

    #[test]
    fn ids_are_unique_and_match_the_json_form() {
        for (i, a) in ALL.iter().enumerate() {
            assert_eq!(
                serde_json::to_value(a).unwrap(),
                serde_json::json!(a.id()),
                "{a:?}"
            );
            assert_eq!(
                serde_json::from_value::<Ade>(serde_json::json!(a.id())).unwrap(),
                *a
            );
            assert!(ALL[..i].iter().all(|b| b.id() != a.id()), "{a:?}");
        }
        assert!(serde_json::from_value::<Ade>(serde_json::json!("other")).is_err());
    }

    #[test]
    fn t3_parts() {
        let t3 = Ade::T3;
        assert_eq!(
            (t3.id(), t3.name(), t3.short_name()),
            ("t3", "T3 Code", "T3")
        );
        assert_eq!(
            t3.download_url(),
            "https://github.com/pingdotgg/t3code/releases/latest"
        );
        assert_eq!(t3.url_scheme(), "t3code");
        assert!(t3.pins_local_version());
        assert_eq!(t3.port(), Some(3774));
        assert_eq!(t3.version_script(), "t3 --version 2>/dev/null || true");
        assert_eq!(t3.probe().path(), "/usr/local/lib/instabox/activity.d/t3");
        assert!(t3.probe().script.starts_with("#!/usr/bin/env python3\n"));
        assert_eq!(t3.parse_version("t3 v0.1.10\n").as_deref(), Some("0.1.10"));
        let Open::Pair(p) = t3.open();
        assert_eq!(p.app_url, "t3code://");
        assert_eq!((p.code)("Token: ABCD1234\n").as_deref(), Some("ABCD1234"));
        assert!((p.link)("https://p-1.instabox.sh", "ABCD1234").starts_with("t3code://pair?"));
    }

    #[test]
    fn t3_runs_on_every_platform_the_app_ships_for() {
        for p in [Platform::MacOs, Platform::Windows] {
            assert!(Ade::T3.platforms().contains(&p), "{p:?}");
        }
        if cfg!(any(target_os = "macos", windows)) {
            assert!(Ade::T3.runs_here());
            assert_eq!(local(), vec![Ade::T3]);
        }
    }

    #[test]
    fn every_link_scheme_is_opened_by_its_own_links() {
        for a in ALL {
            let Open::Pair(p) = a.open();
            let scheme = format!("{}://", a.url_scheme());
            assert!(p.app_url.starts_with(&scheme), "{a:?}");
            assert!(
                (p.link)("https://x.test", "c").starts_with(&scheme),
                "{a:?}"
            );
        }
    }

    #[test]
    fn probe_answers() {
        let idle = parse_probe(r#"{"busy":false,"reason":"no runs active","seq":3}"#);
        assert!(!idle.busy);
        assert_eq!(idle.reason, "no runs active");
        assert!(parse_probe(r#"{"busy":true,"reason":"x"}"#).busy);
        assert!(parse_probe("").busy);
        assert!(parse_probe(r#"{"reason":"x"}"#).busy);
    }
}
