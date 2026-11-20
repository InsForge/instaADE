//! The ADEs (agentic development environments) the app sets up on a sandbox and opens from
//! this computer. Each ADE has its own module; `Ade` hands out its parts and `ALL` lists them.
//! The flows that use them (connect, sync, reinstall) are in `sandbox`.

pub mod cmux;
pub mod t3;

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Every ADE the app knows, in the order the app lists them.
pub const ALL: &[Ade] = &[Ade::T3, Ade::Cmux];

/// Exit code of every ADE's setup script when the ADE is busy and its update waits.
pub const SETUP_EXIT_BUSY: i32 = 3;

/// Where an ADE's setup puts its idle probe. instabox-guest-agent runs every file here.
pub const ACTIVITY_DIR: &str = "/usr/local/lib/instabox/activity.d";

/// An ADE. Its id (`t3`, `cmux`) is its JSON form and an entry of a connected sandbox's mark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ade {
    #[default]
    T3,
    Cmux,
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
    /// The app opens a link, and the desktop app reaches the sandbox over SSH by itself.
    Link(Link),
}

/// An ADE opened by a link. `prep` readies the sandbox before the first link (over exec, so
/// the sandbox must run); `link` builds the link from an SSH login of the sandbox.
#[derive(Debug, Clone, Copy)]
pub struct Link {
    pub prep: &'static str,
    pub link: fn(&SshLogin<'_>) -> String,
}

/// What a link needs to reach a sandbox: the SSH gateway, a token as the user, and a title.
#[derive(Debug, Clone, Copy)]
pub struct SshLogin<'a> {
    pub host: &'a str,
    pub port: u16,
    pub user: &'a str,
    pub title: &'a str,
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
            Ade::Cmux => cmux::ID,
        }
    }

    /// The desktop app's name, such as "T3 Code".
    pub fn name(self) -> &'static str {
        match self {
            Ade::T3 => t3::NAME,
            Ade::Cmux => cmux::NAME,
        }
    }

    /// The short name for what runs on the sandbox, such as "T3".
    pub fn short_name(self) -> &'static str {
        match self {
            Ade::T3 => t3::SHORT_NAME,
            Ade::Cmux => cmux::SHORT_NAME,
        }
    }

    pub fn platforms(self) -> &'static [Platform] {
        match self {
            Ade::T3 => t3::PLATFORMS,
            Ade::Cmux => cmux::PLATFORMS,
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
            Ade::Cmux => cmux::DOWNLOAD_URL,
        }
    }

    /// The scheme of the desktop app's links, which the app may open.
    pub fn url_scheme(self) -> &'static str {
        match self {
            Ade::T3 => t3::URL_SCHEME,
            Ade::Cmux => cmux::URL_SCHEME,
        }
    }

    // ---- this computer ----

    /// Where the desktop app is, if it is installed: T3's CLI, cmux's app bundle.
    pub fn locate(self) -> Option<PathBuf> {
        match self {
            Ade::T3 => t3::locate(),
            Ade::Cmux => cmux::locate(),
        }
    }

    /// The version of the desktop app at `path` (what `locate` found). Gives up after a few
    /// seconds.
    pub fn local_version(self, path: &Path) -> Result<String> {
        match self {
            Ade::T3 => t3::version(path),
            Ade::Cmux => cmux::version(path),
        }
    }

    /// The version of the desktop app on this computer; None when it is not found or does not
    /// answer. It blocks, so run it off the async runtime.
    pub fn detect(self) -> Option<String> {
        let cli = self.locate()?;
        self.local_version(&cli).ok()
    }

    /// Reads a version from the output of `version_script`, here or on a sandbox. None for an
    /// ADE that installs nothing on the sandbox.
    pub fn parse_version(self, out: &str) -> Option<String> {
        match self {
            Ade::T3 => t3::parse_version(out),
            Ade::Cmux => None,
        }
    }

    /// True when the sandbox must run the same version as the desktop app. The app then
    /// installs that version and brings connected sandboxes along when it changes.
    pub fn pins_local_version(self) -> bool {
        match self {
            Ade::T3 => true,
            Ade::Cmux => false,
        }
    }

    // ---- the sandbox ----

    /// Prints the ADE's version on the sandbox, or nothing when it is not installed. None for
    /// an ADE that installs nothing there (cmux uploads its own remote part).
    pub fn version_script(self) -> Option<&'static str> {
        match self {
            Ade::T3 => Some(t3::VERSION_SCRIPT),
            Ade::Cmux => None,
        }
    }

    /// Sets the ADE up on the sandbox. T3 installs `version` with its probe and starts it; it
    /// exits `SETUP_EXIT_BUSY` when the ADE is busy and the update waits. cmux runs its prep
    /// command, and `version` does not apply.
    pub fn setup_script(self, version: &str) -> String {
        match self {
            Ade::T3 => t3::setup_script(version),
            Ade::Cmux => cmux::PREP_SCRIPT.to_string(),
        }
    }

    /// Stops the ADE's service before a reinstall. None: no service runs on the sandbox.
    pub fn stop_script(self) -> Option<&'static str> {
        match self {
            Ade::T3 => Some(t3::STOP_SCRIPT),
            Ade::Cmux => None,
        }
    }

    /// The idle probe the setup installs. None: the ADE has none, and the sandbox sleeps by
    /// its terminals and CPU, as it does without an ADE.
    pub fn probe(self) -> Option<Probe> {
        match self {
            Ade::T3 => Some(t3::PROBE),
            Ade::Cmux => None,
        }
    }

    /// True when the ADE can be set up again in place (Reinstall): it installs a version, a
    /// service to stop and a probe that says when it is idle.
    pub fn reinstalls(self) -> bool {
        self.version_script().is_some() && self.stop_script().is_some() && self.probe().is_some()
    }

    pub fn open(self) -> Open {
        match self {
            Ade::T3 => Open::Pair(t3::PAIRING),
            Ade::Cmux => Open::Link(cmux::LINK),
        }
    }

    /// The sandbox port the app publishes for the ADE, if any.
    pub fn port(self) -> Option<u16> {
        match self.open() {
            Open::Pair(p) => Some(p.port),
            Open::Link(_) => None,
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
    fn the_registry_lists_t3_then_cmux_by_their_ids() {
        assert_eq!(ALL, &[Ade::T3, Ade::Cmux]);
        assert_eq!(Ade::default(), Ade::T3);
        assert_eq!(Ade::from_id("t3"), Some(Ade::T3));
        assert_eq!(Ade::from_id("cmux"), Some(Ade::Cmux));
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
        assert!(t3.reinstalls());
        assert_eq!(t3.port(), Some(3774));
        assert_eq!(
            t3.version_script(),
            Some("t3 --version 2>/dev/null || true")
        );
        assert_eq!(
            t3.stop_script()
                .map(|s| s.contains("systemctl stop t3-instabox")),
            Some(true)
        );
        let probe = t3.probe().unwrap();
        assert_eq!(probe.path(), "/usr/local/lib/instabox/activity.d/t3");
        assert!(probe.script.starts_with("#!/usr/bin/env python3\n"));
        assert_eq!(t3.parse_version("t3 v0.1.10\n").as_deref(), Some("0.1.10"));
        let Open::Pair(p) = t3.open() else {
            panic!("T3 pairs")
        };
        assert_eq!(p.app_url, "t3code://");
        assert_eq!((p.code)("Token: ABCD1234\n").as_deref(), Some("ABCD1234"));
        assert!((p.link)("https://p-1.instabox.sh", "ABCD1234").starts_with("t3code://pair?"));
    }

    #[test]
    fn cmux_parts() {
        let c = Ade::Cmux;
        assert_eq!((c.id(), c.name(), c.short_name()), ("cmux", "cmux", "cmux"));
        assert_eq!(
            c.download_url(),
            "https://github.com/manaflow-ai/cmux/releases/latest"
        );
        assert_eq!(c.url_scheme(), "cmux");
        // Nothing to install, keep in step or probe on the sandbox: cmux brings its own part.
        assert!(!c.pins_local_version());
        assert!(!c.reinstalls());
        assert_eq!(c.port(), None);
        assert_eq!(c.version_script(), None);
        assert_eq!(c.stop_script(), None);
        assert_eq!(c.probe(), None);
        assert_eq!(c.parse_version("cmux 0.64.25"), None);
        let Open::Link(l) = c.open() else {
            panic!("cmux opens by a link")
        };
        assert_eq!(l.prep, cmux::PREP_SCRIPT);
        assert_eq!(c.setup_script("ignored"), cmux::PREP_SCRIPT);
    }

    #[test]
    fn t3_runs_on_every_platform_the_app_ships_for_and_cmux_on_macos_only() {
        for p in [Platform::MacOs, Platform::Windows] {
            assert!(Ade::T3.platforms().contains(&p), "{p:?}");
        }
        assert_eq!(Ade::Cmux.platforms(), &[Platform::MacOs]);
        if cfg!(target_os = "macos") {
            assert!(Ade::T3.runs_here() && Ade::Cmux.runs_here());
            assert_eq!(local(), vec![Ade::T3, Ade::Cmux]);
        }
        if cfg!(windows) {
            assert!(Ade::T3.runs_here() && !Ade::Cmux.runs_here());
            assert_eq!(local(), vec![Ade::T3], "no cmux on Windows");
        }
    }

    #[test]
    fn every_link_scheme_is_opened_by_its_own_links() {
        for a in ALL {
            let scheme = format!("{}://", a.url_scheme());
            match a.open() {
                Open::Pair(p) => {
                    assert!(p.app_url.starts_with(&scheme), "{a:?}");
                    assert!(
                        (p.link)("https://x.test", "c").starts_with(&scheme),
                        "{a:?}"
                    );
                }
                Open::Link(l) => {
                    let login = SshLogin {
                        host: "ssh.example.test",
                        port: 2222,
                        user: "ibssh_x",
                        title: "box",
                    };
                    assert!((l.link)(&login).starts_with(&scheme), "{a:?}");
                }
            }
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
