//! Paradev (formerly Superset), a macOS app for coding agents. It runs a sandbox as one of its
//! hosts. The app sets a host up once over SSH: it installs its CLI and a `superset-host` user
//! service on the sandbox, which then connects out to Paradev's relay. After that a link opens a
//! new workspace on the host by its host id. Paradev has no link or CLI that adds a host from
//! an SSH address without the user, so InstaADE copies the address and opens the app's host
//! settings, and the user finishes there. A Paradev that takes
//! `superset://settings/hosts?ssh=<address>` links (none released yet) gets the address in that
//! link instead, which opens Set up a host over SSH filled in.
//! On this computer: find the real Paradev app (its identifier and its signature, not only its
//! name), the newest when there are several, and read its version from its bundle.

use anyhow::{Context, Result, bail};
use ring::hmac;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ade::{AddHostLink, Host, HostState, Platform, SshLogin, percent_encode};
use crate::process::{command, run_bounded};

pub const ID: &str = "paradev";
pub const NAME: &str = "Paradev";
pub const SHORT_NAME: &str = "Paradev";
/// Paradev ships for macOS (Apple silicon and Intel). It has no Windows build.
pub const PLATFORMS: &[Platform] = &[Platform::MacOs];
pub const DOWNLOAD_URL: &str = "https://github.com/paradevai/paradev/releases/latest";
/// The stable app's link scheme, kept from Superset.
pub const URL_SCHEME: &str = "superset";

// ---- this computer ----

/// The names of the bundle: newer releases are `Paradev.app`, older ones kept Superset's name.
/// Both have `BUNDLE_ID`, and a Mac can have both.
const BUNDLES: &[&str] = &["Paradev.app", "Superset.app"];
const VERSION_KEY: &str = "CFBundleShortVersionString";
const ID_KEY: &str = "CFBundleIdentifier";

/// The bundle identifier of the stable Paradev app. The Canary build has another one and is
/// not supported.
pub const BUNDLE_ID: &str = "com.superset.desktop";
/// The code requirement the Paradev app must satisfy: Paradev's identifier, signed with its
/// Developer ID (team 2HVBK29S3C). The app gets the sandbox's SSH address with its token, so
/// only an app that meets it may get one.
pub const REQUIREMENT: &str = r#"identifier "com.superset.desktop" and anchor apple generic and certificate leaf[subject.OU] = "2HVBK29S3C""#;
/// A signature check that takes longer counts as failed.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// The newest Paradev app bundle in `dirs`, by either name, that `verify` accepts
/// (`ade::newest_app`). The links open in that bundle, so the version shown is the app that opens.
pub fn locate_in(dirs: &[PathBuf], verify: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let apps = dirs
        .iter()
        .flat_map(|d| BUNDLES.iter().map(move |b| d.join(b)))
        .filter(|app| app.join("Contents").join("Info.plist").is_file());
    crate::ade::newest_app(apps, |app| plist_string(app, VERSION_KEY).ok(), verify)
}

/// The Paradev app in `/Applications` or `~/Applications`, if one there is the real Paradev
/// (`verify`). A bundle that only has the name counts as not installed.
pub fn locate() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut dirs = vec![PathBuf::from("/Applications")];
    dirs.extend(dirs::home_dir().map(|h| h.join("Applications")));
    locate_in(&dirs, verify)
}

/// True when the bundle at `app` is the real Paradev: its `Info.plist` names `BUNDLE_ID`, and
/// codesign finds its signature valid and meeting `REQUIREMENT`. Blocks for a few seconds.
pub fn verify(app: &Path) -> bool {
    verify_with(app, codesign_meets_requirement)
}

/// `verify` with the signature check `signed`.
pub fn verify_with(app: &Path, signed: impl Fn(&Path) -> bool) -> bool {
    plist_string(app, ID_KEY).is_ok_and(|id| id == BUNDLE_ID) && signed(app)
}

/// `codesign --verify --strict` of `app` against `REQUIREMENT`, run with no shell and a
/// deadline. Anything but exit 0 fails, and so does every platform but macOS.
pub fn codesign_meets_requirement(app: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let mut cmd = command("/usr/bin/codesign");
    cmd.arg("--verify")
        .arg("--strict")
        .arg(format!("-R={REQUIREMENT}"))
        .arg(app);
    matches!(run_bounded(cmd, VERIFY_TIMEOUT), Ok((Some(0), _, _)))
}

/// A string from the bundle's `Info.plist` (XML or binary).
fn plist_string(app: &Path, key: &str) -> Result<String> {
    let plist = app.join("Contents").join("Info.plist");
    let value = plist::Value::from_file(&plist)
        .with_context(|| format!("could not read {}", plist.display()))?;
    Ok(value
        .as_dictionary()
        .and_then(|d| d.get(key))
        .and_then(plist::Value::as_string)
        .with_context(|| format!("no {key} in {}", plist.display()))?
        .trim()
        .to_string())
}

/// The version in the bundle's `Info.plist`, such as `2.0.11`.
pub fn version(app: &Path) -> Result<String> {
    let v = plist_string(app, VERSION_KEY)?;
    let ok = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'));
    if !ok {
        bail!("unrecognized Paradev version: {v}");
    }
    Ok(v)
}

/// Test only: a bundle named Superset.app in `dir` whose `Info.plist` has `id` and `version`,
/// in XML or binary form. It has no code and no signature.
#[cfg(test)]
pub(crate) fn test_bundle(dir: &Path, id: &str, version: &str, binary: bool) -> PathBuf {
    test_bundle_named(dir, "Superset.app", id, version, binary)
}

/// Test only: `test_bundle` with the bundle name `name`.
#[cfg(test)]
pub(crate) fn test_bundle_named(
    dir: &Path,
    name: &str,
    id: &str,
    version: &str,
    binary: bool,
) -> PathBuf {
    let mut d = plist::Dictionary::new();
    d.insert(ID_KEY.into(), id.into());
    d.insert(VERSION_KEY.into(), version.into());
    let app = dir.join(name);
    std::fs::create_dir_all(app.join("Contents")).unwrap();
    let path = app.join("Contents").join("Info.plist");
    let value = plist::Value::Dictionary(d);
    if binary {
        value.to_file_binary(&path).unwrap();
    } else {
        value.to_file_xml(&path).unwrap();
    }
    app
}

// ---- the sandbox ----

/// Prints whether the host service Paradev's setup installs is active, and the machine id
/// Paradev derives the host id from, read as Paradev reads it on Linux (`/etc/machine-id`,
/// else `/var/lib/dbus/machine-id`). It changes nothing.
pub const HOST_SCRIPT: &str = r#"echo "service=$(systemctl --user is-active superset-host 2>/dev/null)"
echo "machineid=$(cat /etc/machine-id 2>/dev/null || cat /var/lib/dbus/machine-id 2>/dev/null)""#;

/// Opens Paradev's host settings, where the user clicks Set up a host over SSH.
pub const SETUP_LINK: &str = "superset://settings/hosts";

/// What the user does next, once InstaADE copied the address and opened the host settings.
pub const NOTICE: &str =
    "The SSH address is copied. In Paradev, click Set up a host over SSH and paste it.";

pub const HOST: Host = Host {
    script: HOST_SCRIPT,
    state: parse_host,
    setup_link: SETUP_LINK,
    ssh_address,
    notice: NOTICE,
    link: workspace_link,
    add_link: Some(ADD_HOST_LINK),
};

/// Paradev's link that opens Set up a host over SSH with the address filled in
/// (ParadevAI/Paradev issue 10245). No Paradev release takes it yet.
pub const ADD_HOST_LINK: AddHostLink = AddHostLink {
    link: setup_link_with,
    notice: LINK_NOTICE,
    since: None,
};

/// What the user does in Paradev once the link opened the host setup.
pub const LINK_NOTICE: &str = "In Paradev, click Set up to add this sandbox as a host.";

/// `superset://settings/hosts?ssh=<address>`: Paradev opens Set up a host over SSH with
/// `address` filled in, and sets the host up once the user clicks Set up. The address holds
/// the SSH token, so the link opens only in the Paradev app InstaADE checked.
pub fn setup_link_with(address: &str) -> String {
    format!("{SETUP_LINK}?ssh={}", percent_encode(address))
}

/// Reads `HOST_SCRIPT`'s output. The host runs only when systemd says `active`. A machine id
/// counts only in the form Paradev accepts for a Linux host: 32 lowercase hex digits.
pub fn parse_host(out: &str) -> HostState {
    let value = |key: &str| {
        out.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim())
    };
    let machine_id = value("machineid")
        .filter(|v| v.len() == 32 && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .map(str::to_string);
    HostState {
        running: value("service") == Some("active"),
        machine_id,
    }
}

/// The key of Paradev's host id, kept verbatim from Superset so host ids stay stable.
const HOST_ID_KEY: &str = "superset-desktop-device-id-v1";

/// Paradev's host id of a machine: HMAC-SHA256 of its machine id with `HOST_ID_KEY`, in hex,
/// the first 32 characters (`hostIdFromMachineId` in Paradev's `packages/shared/src/host-info.ts`).
pub fn host_id(machine_id: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, HOST_ID_KEY.as_bytes());
    let tag = hmac::sign(&key, machine_id.as_bytes());
    let hex: String = tag.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    hex[..32].to_string()
}

/// The link that opens Paradev's new-workspace page with the sandbox's host picked. Its page
/// reads the `host` search parameter and prefers it to the host used last.
pub fn workspace_link(machine_id: &str) -> String {
    format!("{URL_SCHEME}://new-workspace?host={}", host_id(machine_id))
}

/// The address the user pastes into Paradev's Set up a host over SSH:
/// `ssh://<token>@<host>:<port>`, the SSH token as the user. The port is left out when it is
/// 22, as ssh assumes it then. ssh decodes the percent-encoded user and host.
pub fn ssh_address(login: &SshLogin<'_>) -> String {
    let at = format!(
        "{}@{}",
        percent_encode(login.user),
        percent_encode(login.host)
    );
    if login.port == 22 {
        format!("ssh://{at}")
    } else {
        format!("ssh://{at}:{}", login.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(port: u16) -> SshLogin<'static> {
        SshLogin {
            host: "ssh.instabox.sh",
            port,
            user: "ibssh_AbC-12_x",
            title: "dev 1",
        }
    }

    #[test]
    fn the_ssh_address_has_the_token_as_user_and_the_gateway_port() {
        assert_eq!(
            ssh_address(&login(2222)),
            "ssh://ibssh_AbC-12_x@ssh.instabox.sh:2222"
        );
        assert_eq!(
            ssh_address(&login(22)),
            "ssh://ibssh_AbC-12_x@ssh.instabox.sh"
        );
        // The title is not part of it, and odd characters cannot break out of the user.
        let odd = SshLogin {
            host: "ssh.example.test",
            port: 2222,
            user: "a@b:c/d e",
            title: "",
        };
        assert_eq!(
            ssh_address(&odd),
            "ssh://a%40b%3Ac%2Fd%20e@ssh.example.test:2222"
        );
    }

    /// Vectors from Paradev's own `hostIdFromMachineId` (source 62dc0f3), run with Bun, and
    /// checked with `openssl dgst -sha256 -hmac superset-desktop-device-id-v1`.
    #[test]
    fn the_host_id_matches_paradev() {
        assert_eq!(
            host_id("0123456789abcdef0123456789abcdef"),
            "977b9d5310b351a8e85d4a4b333d5481"
        );
        assert_eq!(
            host_id("4c4c4544004e3510804ab4c04f4e3732"),
            "e87a73da707aa033cb2cfbf0f1710dd2"
        );
    }

    #[test]
    fn the_workspace_link_picks_the_host_and_never_goes_to_the_web() {
        let link = workspace_link("0123456789abcdef0123456789abcdef");
        assert_eq!(
            link,
            "superset://new-workspace?host=977b9d5310b351a8e85d4a4b333d5481"
        );
        for l in [link.as_str(), SETUP_LINK] {
            assert!(l.starts_with("superset://"), "{l}");
            assert!(!l.contains("https:"), "{l}");
        }
        assert_eq!(SETUP_LINK, "superset://settings/hosts");
        assert_eq!(HOST.setup_link, SETUP_LINK);
    }

    #[test]
    fn the_setup_link_holds_the_whole_address_in_one_parameter() {
        assert_eq!(
            setup_link_with(&ssh_address(&login(2222))),
            "superset://settings/hosts?ssh=ssh%3A%2F%2Fibssh_AbC-12_x%40ssh.instabox.sh%3A2222"
        );
        // An address with encoded characters is encoded again, so Paradev reads it back whole.
        let odd = SshLogin {
            host: "ssh.example.test",
            port: 2222,
            user: "a@b&ssh=x#y",
            title: "",
        };
        assert_eq!(
            setup_link_with(&ssh_address(&odd)),
            "superset://settings/hosts?ssh=ssh%3A%2F%2Fa%2540b%2526ssh%253Dx%2523y%40ssh.example.test%3A2222"
        );
        assert!(
            !LINK_NOTICE.contains("ssh://"),
            "the notice never shows the address"
        );
        let l = HOST.add_link.unwrap();
        assert_eq!((l.link)("ssh://u@h"), setup_link_with("ssh://u@h"));
        assert_eq!((l.notice, l.since), (LINK_NOTICE, None));
    }

    #[test]
    fn the_host_runs_only_when_systemd_says_active() {
        let id = "5f0c3a9e7b2d4e61a8c0f9d3b7e2a14e";
        let out = |service: &str| format!("service={service}\nmachineid={id}\n");
        let on = parse_host(&out("active"));
        assert!(on.running);
        assert_eq!(on.machine_id.as_deref(), Some(id));
        for state in ["inactive", "activating", "failed", "unknown", ""] {
            assert!(!parse_host(&out(state)).running, "{state}");
        }
        assert!(!parse_host("").running);
        assert_eq!(parse_host("").machine_id, None);
    }

    #[test]
    fn only_a_machine_id_paradev_accepts_counts() {
        let id = |m: &str| parse_host(&format!("service=active\nmachineid={m}\n")).machine_id;
        assert_eq!(
            id(" 5f0c3a9e7b2d4e61a8c0f9d3b7e2a14e \r").as_deref(),
            Some("5f0c3a9e7b2d4e61a8c0f9d3b7e2a14e"),
            "trimmed, as Paradev reads it"
        );
        for bad in [
            "",
            "uninitialized",
            "5F0C3A9E7B2D4E61A8C0F9D3B7E2A14C",
            "5f0c3a9e7b2d4e61a8c0f9d3b7e2a14",
            "5f0c3a9e7b2d4e61a8c0f9d3b7e2a14e0",
            "5f0c3a9e7b2d4e61a8c0f9d3b7e2a14g",
        ] {
            assert_eq!(id(bad), None, "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_host_script_is_valid_sh() {
        let st = std::process::Command::new("/bin/sh")
            .arg("-n")
            .arg("-c")
            .arg(HOST_SCRIPT)
            .status()
            .unwrap();
        assert!(st.success());
        assert!(HOST_SCRIPT.contains("systemctl --user is-active superset-host"));
    }

    /// A signature check that passes or fails without running codesign.
    fn signed(ok: bool) -> impl Fn(&Path) -> bool {
        move |_| ok
    }

    #[test]
    fn finds_the_real_app_and_skips_lookalikes() {
        let system = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let dirs = [system.path().to_path_buf(), user.path().to_path_buf()];
        let real = |p: &Path| verify_with(p, signed(true));
        assert_eq!(locate_in(&dirs, real), None, "not installed");
        // A folder named Superset.app or Paradev.app without a bundle is not the app.
        for name in BUNDLES {
            std::fs::create_dir_all(user.path().join(name)).unwrap();
        }
        assert_eq!(locate_in(&dirs, real), None);
        let app = test_bundle(user.path(), BUNDLE_ID, "2.0.11", false);
        assert_eq!(locate_in(&dirs, real), Some(app.clone()), "~/Applications");
        // The Canary build in /Applications does not hide the real one.
        test_bundle(system.path(), "com.superset.desktop.canary", "2.1.0", false);
        assert_eq!(locate_in(&dirs, real), Some(app), "the Canary is skipped");
        let app = test_bundle(system.path(), BUNDLE_ID, "2.0.11", false);
        assert_eq!(locate_in(&dirs, real), Some(app), "/Applications first");
        // The right identifier without the signature counts as not installed.
        assert_eq!(locate_in(&dirs, |p| verify_with(p, signed(false))), None);
    }

    /// Paradev was Superset: a Mac can have the old Superset.app and the new Paradev.app, both
    /// with Paradev's identifier and signature. The newest is the one found, shown and opened.
    #[test]
    fn of_superset_app_and_paradev_app_the_newest_wins() {
        let system = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let dirs = [system.path().to_path_buf(), user.path().to_path_buf()];
        let real = |p: &Path| verify_with(p, signed(true));
        let old = test_bundle_named(system.path(), "Superset.app", BUNDLE_ID, "2.0.11", false);
        assert_eq!(
            locate_in(&dirs, real),
            Some(old.clone()),
            "Superset.app alone"
        );
        let new = test_bundle_named(system.path(), "Paradev.app", BUNDLE_ID, "2.0.12", true);
        assert_eq!(
            locate_in(&dirs, real),
            Some(new.clone()),
            "the newer Paradev.app"
        );
        assert_eq!(version(&new).unwrap(), "2.0.12", "the version shown");
        // Newer is by version, not by name or folder.
        let newest = test_bundle_named(user.path(), "Superset.app", BUNDLE_ID, "2.0.13", false);
        assert_eq!(locate_in(&dirs, real), Some(newest.clone()));
        // A newer copy that fails the check does not hide an older real one.
        let not = |p: &Path| p != newest && real(p);
        assert_eq!(locate_in(&dirs, not), Some(new.clone()));
        // A newer Canary is not Paradev.
        test_bundle_named(
            user.path(),
            "Paradev.app",
            "com.superset.desktop.canary",
            "3.0.0",
            false,
        );
        assert_eq!(locate_in(&dirs, real), Some(newest));
        assert_eq!(locate_in(&dirs, |p| verify_with(p, signed(false))), None);
        // The same version under both names: Paradev.app.
        let dir = tempfile::tempdir().unwrap();
        test_bundle_named(dir.path(), "Superset.app", BUNDLE_ID, "2.0.12", false);
        let new = test_bundle_named(dir.path(), "Paradev.app", BUNDLE_ID, "2.0.12", false);
        assert_eq!(locate_in(&[dir.path().to_path_buf()], real), Some(new));
    }

    #[test]
    fn only_paradev_bundle_id_and_a_good_signature_pass() {
        let dir = tempfile::tempdir().unwrap();
        let good = test_bundle(dir.path(), BUNDLE_ID, "2.0.11", true);
        assert!(verify_with(&good, signed(true)));
        assert!(!verify_with(&good, signed(false)), "a bad signature");
        let dir = tempfile::tempdir().unwrap();
        let other = test_bundle(dir.path(), "com.superset.desktop.canary", "2.0.11", false);
        let asked = std::cell::Cell::new(false);
        let check = |_: &Path| {
            asked.set(true);
            true
        };
        assert!(!verify_with(&other, check), "another identifier");
        assert!(!asked.get(), "no signature check for the wrong identifier");
        assert!(!verify_with(&dir.path().join("none.app"), signed(true)));
    }

    #[test]
    fn the_requirement_names_paradev_and_its_developer() {
        assert_eq!(BUNDLE_ID, "com.superset.desktop");
        assert!(REQUIREMENT.contains(r#"identifier "com.superset.desktop""#));
        assert!(REQUIREMENT.contains("anchor apple generic"));
        assert!(REQUIREMENT.contains(r#"certificate leaf[subject.OU] = "2HVBK29S3C""#));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn codesign_turns_down_an_unsigned_lookalike() {
        // The real check, run on a bundle with Paradev's identifier and no code or signature.
        let dir = tempfile::tempdir().unwrap();
        let fake = test_bundle(dir.path(), BUNDLE_ID, "2.0.11", false);
        assert!(!codesign_meets_requirement(&fake));
        assert!(!verify(&fake));
    }

    /// The real check against each installed Paradev, under either name, when this Mac has one,
    /// and `locate` finds the newest. It runs codesign only; it does not open the app.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_installed_paradev_passes_the_real_check() {
        let apps: Vec<PathBuf> = BUNDLES
            .iter()
            .map(|b| Path::new("/Applications").join(b))
            .filter(|app| app.join("Contents").join("Info.plist").is_file())
            .collect();
        for app in &apps {
            assert!(verify(app), "{} did not verify", app.display());
        }
        if apps.is_empty() {
            return;
        }
        let found = locate().expect("an installed Paradev");
        for app in apps {
            let newest =
                crate::ade::newest_app([found.clone(), app], |a| version(a).ok(), |_| true);
            assert_eq!(
                newest.as_ref(),
                Some(&found),
                "{} is not the newest",
                found.display()
            );
        }
    }

    #[test]
    fn reads_the_version_from_an_xml_or_binary_info_plist() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_bundle(dir.path(), BUNDLE_ID, "2.0.11", false);
        assert_eq!(version(&app).unwrap(), "2.0.11");
        let dir = tempfile::tempdir().unwrap();
        let app = test_bundle(dir.path(), BUNDLE_ID, "2.1.0-beta.1", true);
        assert_eq!(version(&app).unwrap(), "2.1.0-beta.1");
    }

    #[test]
    fn a_missing_or_odd_version_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            version(&dir.path().join("Paradev.app")).is_err(),
            "no bundle"
        );
        let app = test_bundle(dir.path(), BUNDLE_ID, "1.0; rm -rf /", false);
        assert!(version(&app).is_err());
    }

    #[test]
    fn locate_and_the_signature_check_are_macos_only() {
        if !cfg!(target_os = "macos") {
            assert_eq!(locate(), None);
            assert!(!codesign_meets_requirement(Path::new("Superset.app")));
        }
    }
}
