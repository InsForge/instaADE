//! Paradev (formerly Superset), a macOS app for coding agents. It runs a sandbox as one of its
//! hosts. The app sets a host up once over SSH: it installs its CLI and a `superset-host` user
//! service on the sandbox, which then connects out to Paradev's relay. After that a link opens a
//! new workspace on the host by its host id. Paradev has no link or CLI that adds a host from
//! an SSH address without the user, so InstaADE copies the address and opens the app's host
//! settings, and the user finishes there. Once the host runs, InstaADE's host check installs
//! an idle probe next to it.
//! On this computer: find the real Paradev app (its identifier and its signature, not only its
//! name), the newest when there are several, and read its version from its bundle.

use anyhow::{Context, Result, bail};
use ring::hmac;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ade::{Host, HostState, Platform, Probe, SshLogin};
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

/// The activity probe, which the host check installs once the host runs. Paradev's host
/// reaches Paradev over an outbound relay, which the instabox idle loop does not count, and
/// its chat agents run on pipes, not a terminal. So without the probe a sandbox can sleep
/// under a working agent.
pub const PROBE: Probe = Probe {
    name: "paradev",
    script: include_str!("../../assets/paradev-probe"),
};

/// Prints whether the host service Paradev's setup installs is active, and the machine id
/// Paradev derives the host id from, read as Paradev reads it on Linux (`/etc/machine-id`,
/// else `/var/lib/dbus/machine-id`). When the host is active, it also installs `PROBE`, or
/// brings it up to date, and changes nothing else. A probe it cannot install changes neither
/// its output nor its exit code. InstaADE runs it at Install, Reconnect and Open, so a
/// sandbox set up before the probe existed gets it at its next Open.
pub const HOST_SCRIPT: &str = concat!(
    r#"service=$(systemctl --user is-active superset-host 2>/dev/null)
if [ "$service" = active ]; then
  probe=/usr/local/lib/instabox/activity.d/paradev
  new=$(mktemp) && {
    cat > "$new" <<'INSTABOX_PROBE_EOF'
"#,
    include_str!("../../assets/paradev-probe"),
    r#"INSTABOX_PROBE_EOF
    if ! cmp -s "$new" "$probe" || [ ! -x "$probe" ]; then
      { sudo -n install -d -m 0755 /usr/local/lib/instabox/activity.d &&
        sudo -n install -m 0755 "$new" "$probe"; } || echo "could not install the Paradev probe" >&2
    fi
    rm -f "$new"
  }
fi
echo "service=$service"
echo "machineid=$(cat /etc/machine-id 2>/dev/null || cat /var/lib/dbus/machine-id 2>/dev/null)""#
);

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
};

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

/// Percent-encodes everything but the unreserved characters. ssh decodes the user and host of
/// an `ssh://` address.
fn encode(s: &str) -> String {
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

/// The address the user pastes into Paradev's Set up a host over SSH:
/// `ssh://<token>@<host>:<port>`, the SSH token as the user. The port is left out when it is
/// 22, as ssh assumes it then.
pub fn ssh_address(login: &SshLogin<'_>) -> String {
    let at = format!("{}@{}", encode(login.user), encode(login.host));
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

    // ---- the probe and its install ----

    /// Ends the probe's here-document in `HOST_SCRIPT`.
    const PROBE_EOF: &str = "INSTABOX_PROBE_EOF";

    #[test]
    fn the_host_check_carries_the_probe_to_activity_d() {
        assert_eq!(PROBE.path(), "/usr/local/lib/instabox/activity.d/paradev");
        assert_eq!(
            PROBE.path(),
            format!("{}/paradev", crate::ade::ACTIVITY_DIR)
        );
        assert!(PROBE.script.starts_with("#!/usr/bin/env python3\n"));
        // The here-document holds the probe as it is, and its end marker is on its own line.
        assert!(PROBE.script.ends_with('\n'));
        assert!(!PROBE.script.contains(PROBE_EOF));
        assert!(HOST_SCRIPT.contains(&format!("<<'{PROBE_EOF}'\n{}{PROBE_EOF}\n", PROBE.script)));
        assert!(HOST_SCRIPT.contains(&format!("\n  probe={}\n", PROBE.path())));
        // A CRLF checkout (Windows) would break the script on the sandbox.
        assert!(!PROBE.script.contains('\r'));
        // The probe is written only when the host runs, and with sudo that never asks.
        let gate = HOST_SCRIPT
            .find("if [ \"$service\" = active ]; then")
            .unwrap();
        let install = HOST_SCRIPT.find("sudo -n install -m 0755").unwrap();
        assert!(gate < install);
        assert!(!HOST_SCRIPT.contains("sudo install"));
        // The output Install, Reconnect and Open read stays the last two lines.
        assert!(HOST_SCRIPT.ends_with(
            "echo \"service=$service\"\necho \"machineid=$(cat /etc/machine-id 2>/dev/null || cat /var/lib/dbus/machine-id 2>/dev/null)\""
        ));
    }

    /// Runs `HOST_SCRIPT` with `root` in place of `ACTIVITY_DIR`, a stand-in `systemctl` that
    /// reports `state`, and a stand-in `sudo` that logs its arguments and runs the command (or
    /// fails when `sudo_fails`). Returns the exit code, stdout and stderr.
    #[cfg(unix)]
    fn run_host_script(
        bin: &Path,
        root: &Path,
        log: &Path,
        state: &str,
        sudo_fails: bool,
    ) -> (Option<i32>, String, String) {
        let dir = crate::ade::ACTIVITY_DIR;
        assert_eq!(
            HOST_SCRIPT.matches(dir).count(),
            2,
            "the probe's path and its folder"
        );
        let script = HOST_SCRIPT.replace(dir, &root.display().to_string());
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SERVICE_STATE", state)
            .env("SUDO_LOG", log)
            .env("SUDO_FAIL", if sudo_fails { "1" } else { "" })
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    #[cfg(unix)]
    fn write_exe(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_host_check_installs_the_probe_only_once_the_host_runs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let root = dir.path().join("root");
        let log = dir.path().join("sudo.log");
        std::fs::create_dir_all(&bin).unwrap();
        write_exe(
            &bin.join("systemctl"),
            "#!/bin/sh\necho \"$SERVICE_STATE\"\n[ \"$SERVICE_STATE\" = active ]\n",
        );
        write_exe(
            &bin.join("sudo"),
            r#"#!/bin/sh
echo "$*" >> "$SUDO_LOG"
[ "$1" = -n ] || exit 1
shift
[ -z "$SUDO_FAIL" ] || exit 1
exec "$@"
"#,
        );
        let installed = root.join("paradev");
        let sudo_calls = || std::fs::read_to_string(&log).unwrap_or_default();

        // The host does not run (yet): nothing is written, and the output is as before.
        for state in ["inactive", "activating", "failed"] {
            let (code, out, _) = run_host_script(&bin, &root, &log, state, false);
            assert_eq!(code, Some(0));
            assert!(
                out.starts_with(&format!("service={state}\nmachineid=")),
                "{out}"
            );
            assert!(!parse_host(&out).running);
        }
        assert_eq!(sudo_calls(), "", "no sudo before the host runs");
        assert!(!installed.exists());

        // It runs: the probe goes in, executable, and the output is the same two lines.
        let (code, out, err) = run_host_script(&bin, &root, &log, "active", false);
        assert_eq!(code, Some(0), "{err}");
        assert_eq!(out.lines().count(), 2, "{out}");
        assert!(out.starts_with("service=active\nmachineid="), "{out}");
        assert!(parse_host(&out).running);
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), PROBE.script);
        let mode = std::fs::metadata(&installed).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        let calls = sudo_calls();
        assert_eq!(calls.lines().count(), 2, "{calls}");
        assert!(
            calls.lines().all(|l| l.starts_with("-n install ")),
            "{calls}"
        );

        // The same probe again: nothing is written.
        run_host_script(&bin, &root, &log, "active", false);
        assert_eq!(sudo_calls(), calls, "an unchanged probe is left alone");

        // An older probe, or one that lost its mode, is replaced.
        std::fs::write(&installed, "#!/bin/sh\necho old\n").unwrap();
        run_host_script(&bin, &root, &log, "active", false);
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), PROBE.script);
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o644)).unwrap();
        run_host_script(&bin, &root, &log, "active", false);
        let mode = std::fs::metadata(&installed).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);

        // When sudo fails, the host check still answers, and still succeeds.
        std::fs::remove_file(&installed).unwrap();
        let (code, out, err) = run_host_script(&bin, &root, &log, "active", true);
        assert_eq!(code, Some(0));
        assert!(out.starts_with("service=active\nmachineid="), "{out}");
        assert!(parse_host(&out).running);
        assert!(err.contains("could not install the Paradev probe"), "{err}");
        assert!(!installed.exists());
    }

    // The probe, run as the guest agent runs it, against a stand-in host service.

    #[cfg(unix)]
    const TOKEN: &str = "c0ffee-host-secret-0123456789abcdef";

    /// Each request line the stand-in host got, with its Authorization header.
    #[cfg(unix)]
    type Requests = std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

    /// A stand-in for Paradev's host service: it answers every request with `answer` when the
    /// request carries `TOKEN`, else with 401 as the host does, and records each request in
    /// `seen`.
    #[cfg(unix)]
    struct FakeHost {
        port: u16,
        answer: std::sync::Arc<std::sync::Mutex<String>>,
        seen: Requests,
    }

    #[cfg(unix)]
    impl FakeHost {
        fn start() -> FakeHost {
            use std::io::{BufRead, BufReader, Write};
            use std::sync::{Arc, Mutex};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let answer = Arc::new(Mutex::new(String::new()));
            let seen = Arc::new(Mutex::new(Vec::new()));
            let (a, s) = (answer.clone(), seen.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    let _ = reader.read_line(&mut line);
                    let mut auth = None;
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':')
                            && k.eq_ignore_ascii_case("authorization")
                        {
                            auth = Some(v.trim().to_string());
                        }
                    }
                    let ok = auth.as_deref() == Some(format!("Bearer {TOKEN}").as_str());
                    s.lock().unwrap().push((line.trim_end().to_string(), auth));
                    let (status, body) = if ok {
                        ("200 OK", a.lock().unwrap().clone())
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":{"json":{"message":"Invalid or missing authentication token.","code":-32001}}}"#
                                .to_string(),
                        )
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            });
            FakeHost { port, answer, seen }
        }

        /// Answers `terminalAgents.list` with `agents`, in the host's superjson envelope.
        fn agents(&self, agents: serde_json::Value) {
            let body = serde_json::json!({"result": {"data": {"json": agents}}});
            *self.answer.lock().unwrap() = body.to_string();
        }

        fn endpoint(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }
    }

    /// Writes the manifest Paradev's host writes at every start, under `home`.
    #[cfg(unix)]
    fn manifest(home: &Path, dir: &str, endpoint: &str, token: &str, pid: u32) {
        let org = home
            .join(dir)
            .join("host")
            .join("2f8e0c1a-4b7d-4e9a-9c3f-5d6b7a8e9f01");
        std::fs::create_dir_all(&org).unwrap();
        let m = serde_json::json!({
            "pid": pid,
            "endpoint": endpoint,
            "authToken": token,
            "startedAt": 1_764_000_000_000_u64,
            "organizationId": "2f8e0c1a-4b7d-4e9a-9c3f-5d6b7a8e9f01",
        });
        std::fs::write(org.join("manifest.json"), m.to_string()).unwrap();
    }

    /// Runs the probe as instabox-guest-agent does: python3 with a cleared environment, in the
    /// home directory. Returns the exit code, stdout and stderr, and checks that neither stream
    /// holds a token. None without python3.
    #[cfg(unix)]
    fn run_probe(home: &Path) -> Option<(Option<i32>, String, String)> {
        let Ok(python) = which::which("python3") else {
            eprintln!("python3 not found: skipped");
            return None;
        };
        let probe = home.join("probe");
        std::fs::write(&probe, PROBE.script).unwrap();
        let out = std::process::Command::new(python)
            .arg(&probe)
            .env_clear()
            .env("HOME", home)
            .env("SHELL", "/bin/bash")
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .current_dir(home)
            .output()
            .unwrap();
        let (stdout, stderr) = (
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        );
        for s in [&stdout, &stderr] {
            assert!(!s.contains(TOKEN) && !s.contains("wrong-token"), "{s}");
        }
        Some((out.status.code(), stdout, stderr))
    }

    /// The probe's answer: exit 0 and one JSON line of at most 4 KiB, with a reason of at most
    /// 200 characters. Read with the app's own probe reader too.
    #[cfg(unix)]
    fn answer(home: &Path) -> Option<serde_json::Value> {
        let (code, out, err) = run_probe(home)?;
        assert_eq!(code, Some(0), "{err}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.len() <= 4096);
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert!(v["reason"].as_str().unwrap().chars().count() <= 200);
        assert_eq!(
            crate::ade::parse_probe(&out).busy,
            v["busy"].as_bool().unwrap()
        );
        Some(v)
    }

    #[cfg(unix)]
    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// An agent as `terminalAgents.list` returns it, last heard from `ago_ms` ago.
    #[cfg(unix)]
    fn agent(event: &str, ago_ms: i64) -> serde_json::Value {
        let at = now_ms() - ago_ms;
        serde_json::json!({
            "terminalId": format!("term-{event}-{ago_ms}"),
            "workspaceId": "ws-1",
            "agentId": "claude",
            "agentSessionId": "5b0e6c3e-1f7a-4d8e-9b2a-3c4d5e6f7a8b",
            "startedAt": at - 60_000,
            "lastEventAt": at,
            "lastEventType": event,
        })
    }

    #[cfg(unix)]
    const MINUTE: i64 = 60_000;

    #[cfg(unix)]
    #[test]
    fn the_probe_reads_each_agent_state() {
        let home = tempfile::tempdir().unwrap();
        let host = FakeHost::start();
        manifest(
            home.path(),
            ".paradev",
            &host.endpoint(),
            TOKEN,
            std::process::id(),
        );
        let verdict = |agents: serde_json::Value| -> Option<(bool, String)> {
            host.agents(agents);
            let v = answer(home.path())?;
            Some((
                v["busy"].as_bool().unwrap(),
                v["reason"].as_str().unwrap().to_string(),
            ))
        };
        let chat = |event: &str, ago: i64| {
            let mut a = agent(event, ago);
            a["chatSessionId"] = "chat-1".into();
            a
        };
        let Some(none) = verdict(serde_json::json!([])) else {
            return;
        };
        assert_eq!(none, (false, "no paradev agents".to_string()));
        let cases: Vec<(serde_json::Value, bool, &str)> = vec![
            // Working, and starting: busy.
            (agent("Start", 1_000), true, "1 agent busy (1 working)"),
            (agent("Attached", 1_000), true, "1 agent busy (1 starting)"),
            // Waiting on the user, or done: idle.
            (
                agent("PermissionRequest", 1_000),
                false,
                "no busy agents (1 waiting)",
            ),
            (agent("Stop", 1_000), false, "no busy agents (1 done)"),
            (agent("Failed", 1_000), false, "no busy agents (1 failed)"),
            // A state this probe does not know: busy.
            (agent("Compacting", 1_000), true, "1 agent busy (1 other)"),
            // A Start whose Stop hook was lost (an interrupt fires none) ends after 30 minutes.
            (
                agent("Start", 29 * MINUTE),
                true,
                "1 agent busy (1 working)",
            ),
            (
                agent("Start", 31 * MINUTE),
                false,
                "no busy agents (1 stale)",
            ),
            (
                agent("Compacting", 31 * MINUTE),
                false,
                "no busy agents (1 stale)",
            ),
            // An agent that never got a prompt waits for the user after 5 minutes.
            (
                agent("Attached", 4 * MINUTE),
                true,
                "1 agent busy (1 starting)",
            ),
            (
                agent("Attached", 6 * MINUTE),
                false,
                "no busy agents (1 stale)",
            ),
            (
                chat("Attached", 6 * MINUTE),
                false,
                "no busy agents (1 stale)",
            ),
            // Waiting is idle however recent or old.
            (
                agent("PermissionRequest", 7 * 60 * MINUTE),
                false,
                "no busy agents (1 waiting)",
            ),
            // A chat turn is stamped once, at its start: a long one stays busy.
            (chat("Start", 31 * MINUTE), true, "1 agent busy (1 working)"),
            (
                chat("Start", 5 * 60 * MINUTE),
                true,
                "1 agent busy (1 working)",
            ),
            (
                chat("Start", 7 * 60 * MINUTE),
                false,
                "no busy agents (1 stale)",
            ),
            (chat("Stop", 1_000), false, "no busy agents (1 done)"),
        ];
        for (a, busy, reason) in cases {
            let got = verdict(serde_json::json!([a.clone()])).unwrap();
            assert_eq!(got, (busy, reason.to_string()), "{a}");
        }

        // A subagent's hooks keep its parent's turn alive; an ended one does not.
        let mut parent = agent("Start", 40 * MINUTE);
        let sub = |ago: i64, ended: bool| {
            let at = now_ms() - ago;
            let mut s =
                serde_json::json!({"id": "sub-1", "startedAt": at - MINUTE, "lastEventAt": at});
            if ended {
                s["endedAt"] = at.into();
            }
            s
        };
        parent["subagents"] = serde_json::json!([sub(MINUTE, false)]);
        assert!(verdict(serde_json::json!([parent.clone()])).unwrap().0);
        parent["subagents"] = serde_json::json!([sub(MINUTE, true)]);
        assert!(!verdict(serde_json::json!([parent.clone()])).unwrap().0);
        parent["subagents"] = serde_json::json!([sub(35 * MINUTE, false)]);
        assert!(!verdict(serde_json::json!([parent])).unwrap().0);

        // An ended agent does not count.
        let mut ended = agent("Start", 1_000);
        ended["endedAt"] = now_ms().into();
        assert_eq!(
            verdict(serde_json::json!([ended])).unwrap(),
            (false, "no paradev agents".to_string())
        );

        // Several agents: busy when one is, and each state is counted.
        let all = serde_json::json!([
            agent("Stop", 1_000),
            agent("Start", 2_000),
            agent("PermissionRequest", 3_000),
            agent("Start", 4_000),
        ]);
        assert_eq!(
            verdict(all).unwrap(),
            (
                true,
                "2 agents busy (2 working, 1 waiting, 1 done)".to_string()
            )
        );

        // Every request asked for the agent list, with the manifest's token.
        let seen = host.seen.lock().unwrap();
        assert!(!seen.is_empty());
        for (line, auth) in seen.iter() {
            assert_eq!(line, "GET /trpc/terminalAgents.list HTTP/1.1");
            assert_eq!(auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
        }
    }

    /// `seq` is the newest event's time: it moves when a turn starts or ends, also between two
    /// readings that are both idle.
    #[cfg(unix)]
    #[test]
    fn the_probe_seq_moves_when_an_agent_starts_or_stops() {
        let home = tempfile::tempdir().unwrap();
        let host = FakeHost::start();
        manifest(
            home.path(),
            ".paradev",
            &host.endpoint(),
            TOKEN,
            std::process::id(),
        );
        let seq = |agents: serde_json::Value| -> Option<i64> {
            host.agents(agents);
            Some(answer(home.path())?["seq"].as_i64().unwrap())
        };
        let Some(empty) = seq(serde_json::json!([])) else {
            return;
        };
        assert_eq!(empty, 0);
        let mut a = agent("Stop", 10 * MINUTE);
        let before = seq(serde_json::json!([a.clone()])).unwrap();
        assert_eq!(Some(before), a["lastEventAt"].as_i64());
        // A turn started and stopped since: idle again, with a new seq.
        a["lastEventAt"] = (now_ms() - 1_000).into();
        let after = seq(serde_json::json!([a.clone(), agent("Stop", 20 * MINUTE)])).unwrap();
        assert_eq!(Some(after), a["lastEventAt"].as_i64());
        assert_ne!(before, after);
        // Busy answers carry it too.
        let busy = agent("Start", 500);
        assert_eq!(
            seq(serde_json::json!([a, busy.clone()])),
            busy["lastEventAt"].as_i64()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_is_idle_without_a_running_host() {
        let home = tempfile::tempdir().unwrap();
        let idle = |reason: &str| {
            let Some(v) = answer(home.path()) else {
                return;
            };
            assert_eq!(v, serde_json::json!({"busy": false, "reason": reason}));
        };
        // Paradev never set this sandbox up.
        idle("no paradev host");
        // A manifest whose host process is gone.
        let mut gone = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        manifest(home.path(), ".paradev", "http://127.0.0.1:9", TOKEN, pid);
        idle("paradev host not running");
        // A host process, but nothing listens at the endpoint.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        manifest(
            home.path(),
            ".paradev",
            &format!("http://127.0.0.1:{port}"),
            TOKEN,
            std::process::id(),
        );
        idle("paradev host not running");
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_finds_the_host_under_the_old_folder_once() {
        let home = tempfile::tempdir().unwrap();
        let host = FakeHost::start();
        host.agents(serde_json::json!([agent("Start", 1_000)]));
        // Before the rename the folder was ~/.superset.
        manifest(
            home.path(),
            ".superset",
            &host.endpoint(),
            TOKEN,
            std::process::id(),
        );
        let Some(v) = answer(home.path()) else {
            return;
        };
        assert_eq!(v["busy"], true);
        // Paradev moves it to ~/.paradev and leaves a link: the host is asked once.
        std::fs::rename(home.path().join(".superset"), home.path().join(".paradev")).unwrap();
        std::os::unix::fs::symlink(home.path().join(".paradev"), home.path().join(".superset"))
            .unwrap();
        host.seen.lock().unwrap().clear();
        assert_eq!(answer(home.path()).unwrap()["busy"], true);
        assert_eq!(host.seen.lock().unwrap().len(), 1);
    }

    /// A probe that cannot read the host fails, which the server counts as idle, and never
    /// prints the token: not on stdout, not in its error.
    #[cfg(unix)]
    #[test]
    fn the_probe_fails_without_printing_the_token() {
        let home = tempfile::tempdir().unwrap();
        let host = FakeHost::start();
        let fails = |what: &str| {
            let Some((code, out, err)) = run_probe(home.path()) else {
                return;
            };
            assert_eq!(code, Some(1), "{what}: {out}{err}");
            assert_eq!(out, "", "{what}");
            assert!(err.starts_with("paradev probe: "), "{what}: {err}");
        };
        // The host turns down a token that is not its own.
        manifest(
            home.path(),
            ".paradev",
            &host.endpoint(),
            "wrong-token",
            std::process::id(),
        );
        host.agents(serde_json::json!([agent("Start", 1_000)]));
        fails("a wrong token");
        assert_eq!(
            host.seen.lock().unwrap().last().unwrap().1.as_deref(),
            Some("Bearer wrong-token")
        );
        manifest(
            home.path(),
            ".paradev",
            &host.endpoint(),
            TOKEN,
            std::process::id(),
        );
        // An answer the probe does not know.
        *host.answer.lock().unwrap() = "<html>not trpc</html>".to_string();
        fails("not JSON");
        *host.answer.lock().unwrap() = r#"{"result":{"data":{"json":{"agents":[]}}}}"#.to_string();
        fails("not a list");
        host.agents(serde_json::json!([{"lastEventType": "Start"}]));
        fails("an agent without lastEventAt");
        // The token goes only to this machine: an endpoint elsewhere is never called.
        let asked = host.seen.lock().unwrap().len();
        for endpoint in [
            "https://127.0.0.1:1".to_string(),
            "http://relay.example.com:4879".to_string(),
            format!("http://127.0.0.1.example.com:{}", host.port),
            "http://127.0.0.1".to_string(),
        ] {
            manifest(
                home.path(),
                ".paradev",
                &endpoint,
                TOKEN,
                std::process::id(),
            );
            fails(&endpoint);
        }
        assert_eq!(host.seen.lock().unwrap().len(), asked);
    }
}
