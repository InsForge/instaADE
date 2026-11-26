//! The ADEs (agentic development environments) the app sets up on a sandbox and opens from
//! this computer. Each ADE has its own module; `Ade` hands out its parts and `ALL` lists them.
//! The flows that use them (connect, sync, reinstall) are in `sandbox`.

pub mod cmux;
pub mod herdr;
pub mod orca;
pub mod paradev;
pub mod paseo;
pub mod t3;

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Every ADE the app knows, in the order the app lists them.
pub const ALL: &[Ade] = &[
    Ade::T3,
    Ade::Cmux,
    Ade::Orca,
    Ade::Paradev,
    Ade::Paseo,
    Ade::Herdr,
];

/// Exit code of every ADE's setup script when the ADE is busy and its update waits.
pub const SETUP_EXIT_BUSY: i32 = 3;

/// Where an ADE's setup puts its idle probe. instabox-guest-agent runs every file here.
pub const ACTIVITY_DIR: &str = "/usr/local/lib/instabox/activity.d";

/// An ADE. Its id (`t3`, `cmux`, `orca`, `paradev`, `paseo`, `herdr`) is its JSON form and an entry of a connected sandbox's mark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ade {
    #[default]
    T3,
    Cmux,
    Orca,
    Paradev,
    Paseo,
    Herdr,
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
    /// The app writes the sandbox as a host in the user's SSH config, which the desktop app
    /// imports (Orca) or the user adds by its address (Paseo), and starts the desktop app. The
    /// user connects the host there. A desktop app that takes the ADE's add-host link opens on
    /// the host instead, and the user only confirms.
    SshHost(SshHost),
    /// The desktop app runs the sandbox as one of its hosts, which the user sets up in the app.
    /// A desktop app that takes the ADE's add-host link opens the setup filled in.
    Host(Host),
    /// The ADE runs on the sandbox, and the app opens the user's terminal with an SSH session
    /// that starts it there. Nothing of the ADE runs on this computer.
    Terminal(Terminal),
}

/// An ADE opened by a link. `prep` readies the sandbox before the first link (over exec, so
/// the sandbox must run); `link` builds the link from an SSH login of the sandbox.
#[derive(Debug, Clone, Copy)]
pub struct Link {
    pub prep: &'static str,
    pub link: fn(&SshLogin<'_>) -> String,
}

/// An ADE that reaches a sandbox through a host in the user's SSH config (`ssh_config`).
/// `notice` tells the user what to do in the app, given the host's alias.
#[derive(Debug, Clone, Copy)]
pub struct SshHost {
    pub notice: fn(&str) -> String,
    /// The address the user adds the host by in the app, from its alias (Paseo: `ssh://<alias>`).
    /// It holds no token, and the app copies it. None: the desktop app imports the host itself
    /// (Orca).
    pub address: Option<fn(&str) -> String>,
    /// Sets up on the sandbox what the desktop app reaches over the host (Paseo's daemon), given
    /// the desktop version. It runs at Install, before the mark. None: the desktop app brings
    /// its own part (Orca).
    pub setup: Option<fn(&str) -> String>,
    /// The link that opens the app on the host, from its alias, when the app takes it. It then
    /// replaces the copy of `address`, the plain start of the app, and `notice`.
    pub add_link: Option<AddHostLink>,
}

/// An ADE that runs the sandbox as one of its hosts (Paradev). The desktop app sets the host
/// up over SSH by itself, with the user: the app copies `ssh_address`, opens `setup_link` and
/// shows `notice`. `script` reads on the sandbox whether the host runs and its machine id,
/// `state` parses that, and once the host runs `link` opens the sandbox from its machine id.
#[derive(Debug, Clone, Copy)]
pub struct Host {
    pub script: &'static str,
    pub state: fn(&str) -> HostState,
    pub setup_link: &'static str,
    pub ssh_address: fn(&SshLogin<'_>) -> String,
    pub notice: &'static str,
    pub link: fn(&str) -> String,
    /// The link that opens the host setup with `ssh_address` filled in, when the app takes it.
    /// It then replaces the copy, `setup_link` and `notice`.
    pub add_link: Option<AddHostLink>,
}

/// Set to `1`, InstaADE opens the add-host links of Orca, Paseo and Paradev whatever the app's
/// version, to try builds of those apps that have the links before a release does.
pub const ADD_HOST_LINKS_ENV: &str = "INSTAADE_ADD_HOST_LINKS";

/// A link that opens the ADE's app on its own add-host step, filled in for the sandbox, so the
/// user only confirms there. It carries what the user would enter without it: the host's alias
/// (Orca, Paseo) or the SSH address (Paradev). It opens only in the app InstaADE checked.
#[derive(Debug, Clone, Copy)]
pub struct AddHostLink {
    /// The link, from the host's alias (Orca, Paseo) or the SSH address (Paradev).
    pub link: fn(&str) -> String,
    /// What the user does in the app once the link opened it.
    pub notice: &'static str,
    /// The first version of the app that takes the link. None while no release has it.
    pub since: Option<&'static str>,
}

impl AddHostLink {
    /// True when the app at `version` takes the link: it is `since` or newer, or `forced`
    /// (`ADD_HOST_LINKS_ENV`). An app whose version does not read counts as older.
    pub fn taken_by(&self, version: Option<&str>, forced: bool) -> bool {
        forced
            || self.since.zip(version).is_some_and(|(since, v)| {
                matches!(
                    (version_order(v), version_order(since)),
                    (Some(v), Some(s)) if v >= s
                )
            })
    }
}

/// True when `value`, the value of `ADD_HOST_LINKS_ENV`, turns the links on.
pub fn add_host_links_forced(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "1")
}

/// Percent-encodes everything but the unreserved characters, for a value in a link or in the
/// user and host of an `ssh://` address.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// An ADE that runs on the sandbox in a terminal (Herdr). `command` is what the SSH session
/// runs there.
#[derive(Debug, Clone, Copy)]
pub struct Terminal {
    pub command: &'static str,
}

/// What a host ADE's `script` found on the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostState {
    /// The host runs, so its setup is done.
    pub running: bool,
    /// The machine id the host is known by; None when it is missing or not valid.
    pub machine_id: Option<String>,
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
/// app.
#[derive(Debug, Clone, Copy)]
pub struct Pairing {
    pub script: &'static str,
    pub port: u16,
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
            Ade::Orca => orca::ID,
            Ade::Paradev => paradev::ID,
            Ade::Paseo => paseo::ID,
            Ade::Herdr => herdr::ID,
        }
    }

    /// The desktop app's name, such as "T3 Code".
    pub fn name(self) -> &'static str {
        match self {
            Ade::T3 => t3::NAME,
            Ade::Cmux => cmux::NAME,
            Ade::Orca => orca::NAME,
            Ade::Paradev => paradev::NAME,
            Ade::Paseo => paseo::NAME,
            Ade::Herdr => herdr::NAME,
        }
    }

    /// The short name for what runs on the sandbox, such as "T3".
    pub fn short_name(self) -> &'static str {
        match self {
            Ade::T3 => t3::SHORT_NAME,
            Ade::Cmux => cmux::SHORT_NAME,
            Ade::Orca => orca::SHORT_NAME,
            Ade::Paradev => paradev::SHORT_NAME,
            Ade::Paseo => paseo::SHORT_NAME,
            Ade::Herdr => herdr::SHORT_NAME,
        }
    }

    pub fn platforms(self) -> &'static [Platform] {
        match self {
            Ade::T3 => t3::PLATFORMS,
            Ade::Cmux => cmux::PLATFORMS,
            Ade::Orca => orca::PLATFORMS,
            Ade::Paradev => paradev::PLATFORMS,
            Ade::Paseo => paseo::PLATFORMS,
            Ade::Herdr => herdr::PLATFORMS,
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
            Ade::Orca => orca::DOWNLOAD_URL,
            Ade::Paradev => paradev::DOWNLOAD_URL,
            Ade::Paseo => paseo::DOWNLOAD_URL,
            Ade::Herdr => herdr::DOWNLOAD_URL,
        }
    }

    /// The scheme of the desktop app's links, which the app may open.
    pub fn url_scheme(self) -> &'static str {
        match self {
            Ade::T3 => t3::URL_SCHEME,
            Ade::Cmux => cmux::URL_SCHEME,
            Ade::Orca => orca::URL_SCHEME,
            Ade::Paradev => paradev::URL_SCHEME,
            Ade::Paseo => paseo::URL_SCHEME,
            Ade::Herdr => herdr::URL_SCHEME,
        }
    }

    // ---- this computer ----

    /// Where the desktop app is, if it is installed: T3's CLI, or the app of cmux, Orca, Paradev
    /// or Paseo; for Herdr, the OpenSSH client.
    pub fn locate(self) -> Option<PathBuf> {
        match self {
            Ade::T3 => t3::locate(),
            Ade::Cmux => cmux::locate(),
            Ade::Orca => orca::locate(),
            Ade::Paradev => paradev::locate(),
            Ade::Paseo => paseo::locate(),
            Ade::Herdr => herdr::locate(),
        }
    }

    /// The version of the desktop app at `path` (what `locate` found). Gives up after a few
    /// seconds.
    pub fn local_version(self, path: &Path) -> Result<String> {
        match self {
            Ade::T3 => t3::version(path),
            Ade::Cmux => cmux::version(path),
            Ade::Orca => orca::version(path),
            Ade::Paradev => paradev::version(path),
            Ade::Paseo => paseo::version(path),
            Ade::Herdr => herdr::version(path),
        }
    }

    /// True when the app at `path` is the ADE's own, checked again right before the app hands
    /// it a secret. cmux: its bundle identifier and signature (an SSH token goes into its
    /// link). T3: the app gets no token from a link, so the path `locate` found stands. Orca:
    /// on macOS its identifier and signature, as the app InstaADE starts. Paradev: its
    /// identifier and signature (it gets the sandbox's SSH address with a token). Paseo: as
    /// Orca (it reads the sandbox's host with its token). Herdr: no app gets the token, only
    /// the OpenSSH client in the user's terminal, as with Open in Terminal.
    /// Blocks for a few seconds, so run it off the async runtime.
    pub fn verify(self, path: &Path) -> bool {
        match self {
            Ade::T3 => true,
            Ade::Cmux => cmux::verify(path),
            Ade::Orca => orca::verify(path),
            Ade::Paradev => paradev::verify(path),
            Ade::Paseo => paseo::verify(path),
            Ade::Herdr => true,
        }
    }

    /// What the app says when `locate` finds nothing: the desktop app is not installed, or for
    /// Herdr, the OpenSSH client.
    pub fn missing(self) -> String {
        match self {
            Ade::Herdr => herdr::MISSING.to_string(),
            _ => format!("{} is not installed. Install it first.", self.name()),
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
            Ade::Orca => None,
            Ade::Paradev => None,
            Ade::Paseo => None,
            Ade::Herdr => herdr::parse_version(out),
        }
    }

    /// True when the sandbox must run the same version as the desktop app. The app then
    /// installs that version and brings connected sandboxes along when it changes.
    pub fn pins_local_version(self) -> bool {
        match self {
            Ade::T3 => true,
            Ade::Cmux => false,
            Ade::Orca => false,
            Ade::Paradev => false,
            Ade::Paseo => false,
            Ade::Herdr => false,
        }
    }

    // ---- the sandbox ----

    /// Prints the ADE's version on the sandbox, or nothing when it is not installed. None for
    /// an ADE that installs nothing there (cmux uploads its own remote part, Paradev's app
    /// installs its host), or keeps the version the sandbox has (Paseo, whose app and daemon
    /// work across versions).
    pub fn version_script(self) -> Option<&'static str> {
        match self {
            Ade::T3 => Some(t3::VERSION_SCRIPT),
            Ade::Cmux => None,
            Ade::Orca => None,
            Ade::Paradev => None,
            Ade::Paseo => None,
            Ade::Herdr => Some(herdr::VERSION_SCRIPT),
        }
    }

    /// Sets the ADE up on the sandbox. T3 installs `version` with its probe and starts it; it
    /// exits `SETUP_EXIT_BUSY` when the ADE is busy and the update waits. cmux runs its prep
    /// command, and `version` does not apply. Paradev's app sets its host up itself: this only
    /// reads whether the host runs. Paseo installs its CLI at `version` when the sandbox has
    /// none, and runs its daemon as a service (`SshHost::setup`). Herdr is installed when the
    /// sandbox lacks it, at Herdr's latest release, and its version printed.
    pub fn setup_script(self, version: &str) -> String {
        match self {
            Ade::T3 => t3::setup_script(version),
            Ade::Cmux => cmux::PREP_SCRIPT.to_string(),
            Ade::Orca => String::new(),
            Ade::Paradev => paradev::HOST_SCRIPT.to_string(),
            Ade::Paseo => paseo::setup_script(version),
            Ade::Herdr => herdr::SETUP_SCRIPT.to_string(),
        }
    }

    /// Brings the ADE's service on the sandbox to this build's setup without changing its
    /// version, for a sandbox at the right version whose setup is older (`setup_current`). It
    /// exits `SETUP_EXIT_BUSY` when the ADE is busy and its restart waits. None: the app runs
    /// no service there.
    pub fn refresh_script(self) -> Option<String> {
        match self {
            Ade::T3 => Some(t3::refresh_script()),
            Ade::Cmux => None,
            Ade::Orca => None,
            Ade::Paradev => None,
            Ade::Paseo => None,
            Ade::Herdr => None,
        }
    }

    /// True when the output of `version_script` shows the sandbox runs this build's setup.
    /// Always true for an ADE without `refresh_script`.
    pub fn setup_current(self, version_output: &str) -> bool {
        match self {
            Ade::T3 => t3::setup_current(version_output),
            Ade::Cmux => true,
            Ade::Orca => true,
            Ade::Paradev => true,
            Ade::Paseo => true,
            Ade::Herdr => true,
        }
    }

    /// True when a pairing of the desktop app at `desktop` with the ADE at `server` on the
    /// sandbox works only once the sandbox runs this build's setup (T3 with DPoP on both sides).
    pub fn pairing_needs_setup(self, desktop: &str, server: Option<&str>) -> bool {
        match self {
            Ade::T3 => t3::pairs_with_dpop(desktop, server),
            Ade::Cmux => false,
            Ade::Orca => false,
            Ade::Paradev => false,
            Ade::Paseo => false,
            Ade::Herdr => false,
        }
    }

    /// Stops the ADE's service before a reinstall. None: no service runs on the sandbox.
    pub fn stop_script(self) -> Option<&'static str> {
        match self {
            Ade::T3 => Some(t3::STOP_SCRIPT),
            Ade::Cmux => None,
            Ade::Orca => None,
            Ade::Paradev => None,
            Ade::Paseo => None,
            Ade::Herdr => None,
        }
    }

    /// The idle probe the setup installs. None: the ADE has none, and the sandbox sleeps by
    /// its terminals and CPU, as it does without an ADE.
    pub fn probe(self) -> Option<Probe> {
        match self {
            Ade::T3 => Some(t3::PROBE),
            Ade::Cmux => None,
            Ade::Orca => None,
            Ade::Paradev => None,
            Ade::Paseo => None,
            Ade::Herdr => None,
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
            Ade::Orca => Open::SshHost(orca::SSH_HOST),
            Ade::Paradev => Open::Host(paradev::HOST),
            Ade::Paseo => Open::SshHost(paseo::SSH_HOST),
            Ade::Herdr => Open::Terminal(herdr::TERMINAL),
        }
    }

    /// The sandbox port the app publishes for the ADE, if any.
    pub fn port(self) -> Option<u16> {
        match self.open() {
            Open::Pair(p) => Some(p.port),
            Open::Link(_) => None,
            Open::SshHost(_) => None,
            Open::Host(_) => None,
            Open::Terminal(_) => None,
        }
    }
}

/// The ADEs whose desktop app runs on this computer.
pub fn local() -> Vec<Ade> {
    ALL.iter().copied().filter(|a| a.runs_here()).collect()
}

/// Of the apps at `paths`, the one with the newest `version` that `verify` accepts, so that with
/// several copies installed the app finds, shows and opens the newest. An app whose version does
/// not read comes last, and of two with the same version the one first in `paths` wins. The
/// newest is checked first, so `verify` runs only until one passes.
pub fn newest_app(
    paths: impl IntoIterator<Item = PathBuf>,
    version: impl Fn(&Path) -> Option<String>,
    verify: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let mut found: Vec<_> = paths
        .into_iter()
        .map(|p| (version(&p).as_deref().and_then(version_order), p))
        .collect();
    // A stable sort, newest first: the same version keeps the order of `paths`.
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, p)| p).find(|p| verify(p))
}

/// How an app's version sorts, by semver precedence: its numbers, then its pre-release as
/// `semver::Prerelease` orders it (numeric identifiers as numbers, others as text, a release
/// after its pre-releases). So `2.0.12` > `2.0.11`, `0.65.2.2` > `0.65.2` (cmux has four numbers),
/// `2.1.0-beta.10` > `2.1.0-beta.2` and `2.1.0` > `2.1.0-rc.1`. Build metadata does not count.
/// None when the numbers are not all digits or the pre-release is not valid semver.
fn version_order(v: &str) -> Option<(Vec<u64>, semver::Prerelease)> {
    let v = v.split('+').next().unwrap_or_default();
    let (numbers, pre) = match v.split_once('-') {
        Some((n, p)) => (
            n,
            semver::Prerelease::new(p).ok().filter(|p| !p.is_empty())?,
        ),
        None => (v, semver::Prerelease::EMPTY),
    };
    let numbers = numbers
        .split('.')
        .map(|n| n.parse().ok())
        .collect::<Option<Vec<u64>>>()?;
    Some((numbers, pre))
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
        assert_eq!(
            ALL,
            &[
                Ade::T3,
                Ade::Cmux,
                Ade::Orca,
                Ade::Paradev,
                Ade::Paseo,
                Ade::Herdr
            ]
        );
        assert_eq!(Ade::default(), Ade::T3);
        assert_eq!(Ade::from_id("t3"), Some(Ade::T3));
        assert_eq!(Ade::from_id("cmux"), Some(Ade::Cmux));
        assert_eq!(Ade::from_id("orca"), Some(Ade::Orca));
        assert_eq!(Ade::from_id("paradev"), Some(Ade::Paradev));
        assert_eq!(Ade::from_id("paseo"), Some(Ade::Paseo));
        assert_eq!(Ade::from_id("herdr"), Some(Ade::Herdr));
        assert_eq!(Ade::from_id("superset"), None, "one entry, by its new name");
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
        assert_eq!(t3.version_script(), Some(t3::VERSION_SCRIPT));
        assert!(t3::VERSION_SCRIPT.starts_with("t3 --version 2>/dev/null || true;"));
        assert_eq!(t3.refresh_script(), Some(t3::refresh_script()));
        let current = format!("t3 v0.1.12\n{}\n", t3::setup_line());
        assert!(t3.setup_current(&current));
        assert!(!t3.setup_current("t3 v0.1.12\n"));
        assert!(t3.pairing_needs_setup("0.1.12", Some("0.1.12")));
        assert!(!t3.pairing_needs_setup("0.1.10", Some("0.1.12")));
        assert_eq!(t3.parse_version(&current).as_deref(), Some("0.1.12"));
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
        assert_eq!(c.refresh_script(), None);
        assert!(c.setup_current(""));
        assert!(!c.pairing_needs_setup("0.1.12", Some("0.1.12")));
        let Open::Link(l) = c.open() else {
            panic!("cmux opens by a link")
        };
        assert_eq!(l.prep, cmux::PREP_SCRIPT);
        assert_eq!(c.setup_script("ignored"), cmux::PREP_SCRIPT);
    }

    #[test]
    fn orca_parts() {
        let o = Ade::Orca;
        assert_eq!((o.id(), o.name(), o.short_name()), ("orca", "Orca", "Orca"));
        assert_eq!(
            o.download_url(),
            "https://github.com/stablyai/orca/releases/latest"
        );
        assert_eq!(o.url_scheme(), "orca");
        // Orca installs its own server on the first connect: nothing to set up or keep in step.
        assert!(!o.pins_local_version());
        assert!(!o.reinstalls());
        assert_eq!(o.port(), None);
        assert_eq!(o.version_script(), None);
        assert_eq!(o.stop_script(), None);
        assert_eq!(o.probe(), None);
        assert_eq!(o.parse_version("1.4.269"), None);
        assert_eq!(o.refresh_script(), None);
        assert!(o.setup_current(""));
        assert!(!o.pairing_needs_setup("0.1.12", Some("0.1.12")));
        assert_eq!(o.setup_script("ignored"), "");
        let Open::SshHost(h) = o.open() else {
            panic!("Orca opens through an SSH host")
        };
        assert_eq!((h.notice)("instabox-x"), orca::notice("instabox-x"));
        let l = h.add_link.expect("Orca's connect link");
        assert_eq!(
            (l.link)("instabox-x"),
            "orca://ssh/connect?alias=instabox-x"
        );
        assert_eq!(l.since, None, "no Orca release has it yet");
        assert_eq!(o.platforms(), &[Platform::MacOs, Platform::Windows]);
        assert_eq!(o.runs_here(), cfg!(any(target_os = "macos", windows)));
    }

    #[test]
    fn paradev_parts() {
        let p = Ade::Paradev;
        assert_eq!(
            (p.id(), p.name(), p.short_name()),
            ("paradev", "Paradev", "Paradev")
        );
        assert_eq!(
            p.download_url(),
            "https://github.com/paradevai/paradev/releases/latest"
        );
        assert_eq!(p.url_scheme(), "superset");
        // InstaADE installs, keeps in step and probes nothing: Paradev's app sets its host up.
        assert!(!p.pins_local_version());
        assert!(!p.reinstalls());
        assert_eq!(p.port(), None);
        assert_eq!(p.version_script(), None);
        assert_eq!(p.stop_script(), None);
        assert_eq!(p.probe(), None);
        assert_eq!(p.parse_version("2.0.11"), None);
        assert_eq!(p.refresh_script(), None);
        assert!(p.setup_current(""));
        assert!(!p.pairing_needs_setup("0.1.12", Some("0.1.12")));
        let Open::Host(h) = p.open() else {
            panic!("Paradev runs the sandbox as a host")
        };
        assert_eq!(h.script, paradev::HOST_SCRIPT);
        assert_eq!(h.setup_link, "superset://settings/hosts");
        assert_eq!(h.notice, paradev::NOTICE);
        let l = h.add_link.expect("Paradev's host setup link");
        assert_eq!(
            (l.link)("ssh://u@h:2222"),
            "superset://settings/hosts?ssh=ssh%3A%2F%2Fu%40h%3A2222"
        );
        assert_eq!(l.since, None, "no Paradev release has it yet");
        assert_eq!(p.setup_script("ignored"), paradev::HOST_SCRIPT);
        assert_eq!(p.platforms(), &[Platform::MacOs]);
    }

    #[test]
    fn paseo_parts() {
        let p = Ade::Paseo;
        assert_eq!(
            (p.id(), p.name(), p.short_name()),
            ("paseo", "Paseo", "Paseo")
        );
        assert_eq!(
            p.download_url(),
            "https://github.com/getpaseo/paseo/releases/latest"
        );
        assert_eq!(p.url_scheme(), "paseo");
        // InstaADE installs Paseo's daemon, but keeps no version in step and has no probe yet.
        assert!(!p.pins_local_version());
        assert!(!p.reinstalls());
        assert_eq!(
            p.port(),
            None,
            "the daemon is reached over SSH, never published"
        );
        assert_eq!(p.version_script(), None);
        assert_eq!(p.stop_script(), None);
        assert_eq!(p.probe(), None);
        assert_eq!(p.parse_version("0.17.4"), None);
        assert_eq!(p.refresh_script(), None);
        assert!(p.setup_current(""));
        assert!(!p.pairing_needs_setup("0.17.4", Some("0.17.4")));
        assert_eq!(p.setup_script("0.17.4"), paseo::setup_script("0.17.4"));
        let Open::SshHost(h) = p.open() else {
            panic!("Paseo opens through an SSH host")
        };
        assert_eq!((h.notice)("instabox-x"), paseo::notice("instabox-x"));
        assert_eq!(
            h.address.map(|a| a("instabox-x")).as_deref(),
            Some("ssh://instabox-x")
        );
        assert_eq!(h.setup.map(|s| s("0.17.4")), Some(p.setup_script("0.17.4")));
        let l = h.add_link.expect("Paseo's add host link");
        assert_eq!(
            (l.link)("instabox-x"),
            "paseo://hosts/add?ssh=ssh%3A%2F%2Finstabox-x"
        );
        assert_eq!(l.since, None, "no Paseo release has it yet");
        assert_eq!(p.platforms(), &[Platform::MacOs, Platform::Windows]);
        // Orca brings its own server and imports the host by itself.
        let Open::SshHost(o) = Ade::Orca.open() else {
            panic!("Orca opens through an SSH host")
        };
        assert!(o.address.is_none() && o.setup.is_none());
    }

    #[test]
    fn herdr_parts() {
        let h = Ade::Herdr;
        assert_eq!(
            (h.id(), h.name(), h.short_name()),
            ("herdr", "Herdr", "Herdr")
        );
        assert_eq!(h.url_scheme(), "", "Herdr takes no links");
        // It installs Herdr, but no service, probe or pinned version: nothing to keep in step
        // or reinstall, and Reconnect runs the setup again.
        assert!(!h.pins_local_version());
        assert!(!h.reinstalls());
        assert_eq!(h.port(), None);
        assert_eq!(h.version_script(), Some(herdr::VERSION_SCRIPT));
        assert_eq!(h.stop_script(), None);
        assert_eq!(h.probe(), None);
        assert_eq!(h.refresh_script(), None);
        assert!(h.setup_current(""));
        assert!(!h.pairing_needs_setup("0.1.12", Some("0.1.12")));
        assert_eq!(h.setup_script("ignored"), herdr::SETUP_SCRIPT);
        assert_eq!(h.parse_version("herdr 0.9.5\n").as_deref(), Some("0.9.5"));
        let Open::Terminal(t) = h.open() else {
            panic!("Herdr opens in a terminal")
        };
        assert_eq!(t.command, "herdr");
        // The token goes to the OpenSSH client, not to an app to check.
        assert!(h.verify(Path::new("/nonexistent")));
        assert_eq!(h.platforms(), &[Platform::MacOs, Platform::Windows]);
        assert_eq!(h.runs_here(), cfg!(any(target_os = "macos", windows)));
        assert!(h.missing().contains("no OpenSSH client"));
        assert_eq!(
            Ade::Orca.missing(),
            "Orca is not installed. Install it first."
        );
    }

    #[test]
    fn t3_runs_on_every_platform_the_app_ships_for_and_cmux_on_macos_only() {
        for p in [Platform::MacOs, Platform::Windows] {
            assert!(Ade::T3.platforms().contains(&p), "{p:?}");
        }
        assert_eq!(Ade::Cmux.platforms(), &[Platform::MacOs]);
        assert_eq!(Ade::Paradev.platforms(), &[Platform::MacOs]);
        if cfg!(target_os = "macos") {
            assert!(Ade::T3.runs_here() && Ade::Cmux.runs_here() && Ade::Paradev.runs_here());
            assert_eq!(
                local(),
                vec![
                    Ade::T3,
                    Ade::Cmux,
                    Ade::Orca,
                    Ade::Paradev,
                    Ade::Paseo,
                    Ade::Herdr
                ]
            );
        }
        if cfg!(windows) {
            assert!(Ade::T3.runs_here() && !Ade::Cmux.runs_here() && !Ade::Paradev.runs_here());
            assert_eq!(
                local(),
                vec![Ade::T3, Ade::Orca, Ade::Paseo, Ade::Herdr],
                "no cmux or Paradev on Windows"
            );
        }
    }

    #[test]
    fn every_link_scheme_is_opened_by_its_own_links() {
        for a in ALL {
            let scheme = format!("{}://", a.url_scheme());
            match a.open() {
                Open::Pair(p) => {
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
                Open::Terminal(_) => assert_eq!(a.url_scheme(), "", "{a:?} takes no links"),
                Open::SshHost(h) => {
                    // An address the user adds by hand holds no token: ssh reads it from the host.
                    if let Some(address) = h.address {
                        assert_eq!(address("instabox-x"), "ssh://instabox-x", "{a:?}");
                    }
                    match h.add_link {
                        Some(l) => {
                            let link = (l.link)("instabox-x");
                            assert!(link.starts_with(&scheme), "{a:?}");
                            // It names the host's alias only: the token stays in the host.
                            assert!(link.ends_with("instabox-x"), "{link}");
                        }
                        None => assert_eq!(a.url_scheme(), "", "{a:?} takes no links"),
                    }
                }
                Open::Host(h) => {
                    assert!(h.setup_link.starts_with(&scheme), "{a:?}");
                    let id = "0123456789abcdef0123456789abcdef";
                    assert!((h.link)(id).starts_with(&scheme), "{a:?}");
                    if let Some(l) = h.add_link {
                        assert!((l.link)("ssh://u@h").starts_with(h.setup_link), "{a:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn versions_sort_by_semver_precedence() {
        let order = |v: &str| version_order(v).unwrap_or_else(|| panic!("{v} reads"));
        assert!(order("2.0.12") > order("2.0.11"));
        assert!(order("2.0.10") > order("2.0.9"), "numbers, not text");
        assert!(order("0.65.2.2") > order("0.65.2"));
        assert!(order("2.1.0") > order("2.1.0-beta.1"));
        assert!(order("2.1.0") > order("2.1.0-rc.1"));
        assert!(order("2.1.0-beta.1") > order("2.0.12"));
        // The whole pre-release counts: its numbers as numbers, its words as text.
        assert!(order("2.1.0-beta.2") > order("2.1.0-beta.1"));
        assert!(order("2.1.0-beta.10") > order("2.1.0-beta.2"));
        assert!(order("2.1.0-rc.1") > order("2.1.0-beta.10"));
        assert!(order("2.1.0-beta.1.1") > order("2.1.0-beta.1"));
        assert_eq!(order("1.4.269+build.7"), order("1.4.269"));
        // What does not read is None, without a panic, and sorts after every version that reads.
        for bad in [
            "",
            "abc",
            "1..2",
            "1.x",
            "v1.0",
            "2.1.0-",
            "2.1.0-beta..1",
            "2.1.0-beta.01",
            "-1",
            "2.1.0-ü",
        ] {
            assert_eq!(version_order(bad), None, "{bad}");
        }
        assert!(Some(order("0.0.1-a")) > version_order("2.1.0-beta..1"));
    }

    #[test]
    fn the_newest_app_that_passes_the_check_wins() {
        let (a, b, c) = (
            PathBuf::from("/a/X.app"),
            PathBuf::from("/b/X.app"),
            PathBuf::from("/c/X.app"),
        );
        let versions = |list: &'static [(&'static str, &'static str)]| {
            move |p: &Path| {
                list.iter()
                    .find(|(dir, _)| p.starts_with(dir))
                    .map(|(_, v)| v.to_string())
            }
        };
        let all = || [a.clone(), b.clone(), c.clone()];
        let any = |_: &Path| true;
        let v = versions(&[("/a", "2.0.11"), ("/b", "2.0.12"), ("/c", "2.0.9")]);
        assert_eq!(newest_app(all(), v, any), Some(b.clone()));
        // A newer copy that fails the check does not hide an older real one.
        assert_eq!(newest_app(all(), v, |p: &Path| p != b), Some(a.clone()));
        assert_eq!(newest_app(all(), v, |_: &Path| false), None);
        assert_eq!(newest_app(Vec::new(), v, any), None);
        // The same version: the first in the list. No version: after every one that has one.
        let v = versions(&[("/a", "1.0.0"), ("/b", "1.0.0")]);
        assert_eq!(newest_app(all(), v, any), Some(a.clone()));
        assert_eq!(newest_app([c.clone(), b.clone()], v, any), Some(b.clone()));
        assert_eq!(newest_app([c.clone()], v, any), Some(c.clone()));
        // Pre-releases: the newer one wins wherever it is in the list.
        let v = versions(&[
            ("/a", "2.1.0-beta.1"),
            ("/b", "2.1.0-beta.2"),
            ("/c", "2.1.0-rc.1"),
        ]);
        assert_eq!(newest_app([a.clone(), b.clone()], v, any), Some(b.clone()));
        assert_eq!(newest_app(all(), v, any), Some(c.clone()));
        let v = versions(&[("/a", "2.1.0-beta.2"), ("/b", "2.1.0-beta.10")]);
        assert_eq!(newest_app(all(), v, any), Some(b.clone()));
        // A version that does not read neither panics nor wins.
        let v = versions(&[("/a", "2.1.0-beta..1"), ("/b", "x"), ("/c", "0.0.1")]);
        assert_eq!(newest_app(all(), v, any), Some(c.clone()));
        assert_eq!(newest_app([a.clone(), b.clone()], v, any), Some(a.clone()));
        // The newest is checked first, and the check stops at the first that passes.
        let checked = std::cell::RefCell::new(Vec::new());
        let v = versions(&[("/a", "1.0.0"), ("/b", "3.0.0"), ("/c", "2.0.0")]);
        let check = |p: &Path| {
            checked.borrow_mut().push(p.to_path_buf());
            p != b
        };
        assert_eq!(newest_app(all(), v, check), Some(c.clone()));
        assert_eq!(*checked.borrow(), [b, c]);
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

    #[test]
    fn an_add_host_link_is_taken_from_its_version_on_or_when_forced() {
        fn link(_: &str) -> String {
            String::new()
        }
        let none = AddHostLink {
            link,
            notice: "",
            since: None,
        };
        // No release has it: only forced, whatever the version.
        assert!(!none.taken_by(Some("9.9.9"), false));
        assert!(!none.taken_by(None, false));
        assert!(none.taken_by(Some("1.0.0"), true));
        assert!(none.taken_by(None, true));
        let since = AddHostLink {
            since: Some("1.5.0"),
            ..none
        };
        assert!(since.taken_by(Some("1.5.0"), false));
        assert!(since.taken_by(Some("1.5.1"), false));
        assert!(since.taken_by(Some("1.10.0"), false), "numbers, not text");
        assert!(!since.taken_by(Some("1.4.269"), false));
        assert!(
            !since.taken_by(Some("1.5.0-rc.1"), false),
            "before the release"
        );
        // A version that does not read, or none, is older.
        assert!(!since.taken_by(Some("1.5.x"), false));
        assert!(!since.taken_by(None, false));
        assert!(since.taken_by(Some("1.4.269"), true));
    }

    #[test]
    fn only_1_forces_the_add_host_links() {
        assert!(add_host_links_forced(Some("1")));
        assert!(add_host_links_forced(Some(" 1\n")));
        for off in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("yes"),
            Some("11"),
        ] {
            assert!(!add_host_links_forced(off), "{off:?}");
        }
        assert_eq!(ADD_HOST_LINKS_ENV, "INSTAADE_ADD_HOST_LINKS");
    }

    #[test]
    fn percent_encode_keeps_only_unreserved_characters() {
        assert_eq!(percent_encode("instabox-dev_1.x~"), "instabox-dev_1.x~");
        assert_eq!(
            percent_encode("ssh://a@b:22/c d?e=f&g#h"),
            "ssh%3A%2F%2Fa%40b%3A22%2Fc%20d%3Fe%3Df%26g%23h"
        );
        assert_eq!(percent_encode("%41\n"), "%2541%0A");
        assert_eq!(percent_encode("é"), "%C3%A9");
    }
}
