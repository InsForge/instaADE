//! cmux, a macOS terminal for coding agents. It reaches a sandbox over SSH by itself: a
//! `cmux://ssh` link opens a workspace, and `cmux ssh` uploads its own `cmuxd-remote` to the
//! sandbox, pinned to the app's version. So nothing is installed on the sandbox and no
//! versions must match; the sandbox only needs `PREP_SCRIPT` once, before the first link.
//! On this computer: find the real cmux app (its identifier and its signature, not only its
//! name) and read its version from its bundle.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ade::{Link, Platform, SshLogin};
use crate::process::{command, run_bounded};

pub const ID: &str = "cmux";
pub const NAME: &str = "cmux";
pub const SHORT_NAME: &str = "cmux";
/// cmux ships for macOS only.
pub const PLATFORMS: &[Platform] = &[Platform::MacOs];
pub const DOWNLOAD_URL: &str = "https://github.com/manaflow-ai/cmux/releases/latest";
/// The stable app's link scheme. Never the `https://cmux.com/deeplink` fallback: it would
/// send the SSH token in the link to cmux.com.
pub const URL_SCHEME: &str = "cmux";

// ---- this computer ----

const BUNDLE: &str = "cmux.app";
const VERSION_KEY: &str = "CFBundleShortVersionString";
const ID_KEY: &str = "CFBundleIdentifier";

/// The bundle identifier of the cmux app.
pub const BUNDLE_ID: &str = "com.cmuxterm.app";
/// The code requirement the cmux app must satisfy: cmux's identifier, signed with the
/// Developer ID of Manaflow, Inc. (team 7WLXT3NR37). The cmux link holds an SSH token, so
/// only an app that meets it may get one.
pub const REQUIREMENT: &str = r#"identifier "com.cmuxterm.app" and anchor apple generic and certificate leaf[subject.OU] = "7WLXT3NR37""#;
/// A signature check that takes longer counts as failed.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// The cmux app bundle in the first of `dirs` that has one `verify` accepts.
pub fn locate_in(dirs: &[PathBuf], verify: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(BUNDLE))
        .filter(|app| app.join("Contents").join("Info.plist").is_file())
        .find(|app| verify(app))
}

/// The cmux app in `/Applications` or `~/Applications`, if one there is the real cmux
/// (`verify`). A bundle that only has the name counts as not installed.
pub fn locate() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut dirs = vec![PathBuf::from("/Applications")];
    dirs.extend(dirs::home_dir().map(|h| h.join("Applications")));
    locate_in(&dirs, verify)
}

/// True when the bundle at `app` is the real cmux: its `Info.plist` names `BUNDLE_ID`, and
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

/// The version in the bundle's `Info.plist`, such as `0.64.25`.
pub fn version(app: &Path) -> Result<String> {
    let v = plist_string(app, VERSION_KEY)?;
    let ok = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'));
    if !ok {
        bail!("unrecognized cmux version: {v}");
    }
    Ok(v)
}

/// Test only: a bundle named cmux.app in `dir` whose `Info.plist` has `id` and `version`,
/// in XML or binary form. It has no code and no signature.
#[cfg(test)]
pub(crate) fn test_bundle(dir: &Path, id: &str, version: &str, binary: bool) -> PathBuf {
    let mut d = plist::Dictionary::new();
    d.insert(ID_KEY.into(), id.into());
    d.insert(VERSION_KEY.into(), version.into());
    let app = dir.join(BUNDLE);
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

/// cmux refuses a group-writable state directory, which a fresh sandbox would make; the first
/// link would then fail with "could not prepare secure daemon state directory". Safe to run
/// again.
pub const PREP_SCRIPT: &str = "mkdir -p ~/.local/state && chmod go-w ~/.local ~/.local/state";

pub const LINK: Link = Link {
    prep: PREP_SCRIPT,
    link: ssh_link,
};

/// The longest title cmux takes from a link.
const MAX_TITLE: usize = 160;

/// Percent-encodes everything but the unreserved characters, so a space is `%20`: cmux reads
/// the query with `URLComponents`, which keeps a `+` as it is.
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

/// The `cmux://ssh` link that opens a workspace on the sandbox, with the parameters the
/// instabox dashboard and docs use: `host`, `user` (the SSH token), `port` (left out when it is
/// 22, as there), `title`, and `host-key-policy=accept-new`, without which cmux fails on the
/// gateway's unknown host key. The title drops control characters, which cmux refuses, and
/// keeps cmux's limit of 160 characters.
pub fn ssh_link(login: &SshLogin<'_>) -> String {
    let port = login.port.to_string();
    let title: String = login
        .title
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE)
        .collect();
    let mut query: Vec<(&str, &str)> = vec![("host", login.host), ("user", login.user)];
    if login.port != 0 && login.port != 22 {
        query.push(("port", &port));
    }
    if !title.is_empty() {
        query.push(("title", &title));
    }
    query.push(("host-key-policy", "accept-new"));
    let q: Vec<String> = query
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect();
    format!("{URL_SCHEME}://ssh?{}", q.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login<'a>(port: u16, title: &'a str) -> SshLogin<'a> {
        SshLogin {
            host: "ssh.instabox.sh",
            port,
            user: "ibssh_AbC-12_x",
            title,
        }
    }

    /// The query's values, decoded the way cmux reads them (percent-decoding only).
    fn query(link: &str) -> Vec<(String, String)> {
        let q = link.split_once('?').unwrap().1;
        q.split('&')
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap();
                (k.to_string(), String::from_utf8(percent_decode(v)).unwrap())
            })
            .collect()
    }

    fn percent_decode(s: &str) -> Vec<u8> {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn the_link_carries_the_token_port_title_and_accept_new() {
        let link = ssh_link(&login(2222, "dev-1"));
        assert_eq!(
            link,
            "cmux://ssh?host=ssh.instabox.sh&user=ibssh_AbC-12_x&port=2222&title=dev-1&host-key-policy=accept-new"
        );
    }

    #[test]
    fn port_22_is_left_out_as_the_dashboard_does() {
        let link = ssh_link(&login(22, "dev-1"));
        assert!(!link.contains("port="), "{link}");
        assert!(link.contains("&title=dev-1&host-key-policy=accept-new"));
    }

    #[test]
    fn the_title_is_escaped_so_cmux_reads_it_back() {
        let title = "my box & co=1 #2 +3 ü";
        let link = ssh_link(&login(2222, title));
        assert!(link.contains("title=my%20box%20%26%20co%3D1%20%232%20%2B3%20%C3%BC&"));
        let q = query(&link);
        assert_eq!(
            q.iter().find(|(k, _)| k == "title").unwrap().1,
            title,
            "decoded as cmux decodes it"
        );
        // Nothing breaks out of its parameter.
        let keys: Vec<&str> = q.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["host", "user", "port", "title", "host-key-policy"]);
    }

    #[test]
    fn the_title_drops_control_characters_and_keeps_cmux_limit() {
        let link = ssh_link(&login(2222, "a\nb\u{7}c"));
        assert!(link.contains("&title=abc&"), "{link}");
        let long = "x".repeat(400);
        let q = query(&ssh_link(&login(2222, &long)));
        assert_eq!(q.iter().find(|(k, _)| k == "title").unwrap().1.len(), 160);
        let link = ssh_link(&login(2222, "\n"));
        assert!(
            !link.contains("title="),
            "an empty title is left out: {link}"
        );
    }

    #[test]
    fn the_link_is_never_an_https_link() {
        for title in ["dev-1", "", "https://cmux.com/deeplink/ssh"] {
            let link = ssh_link(&login(2222, title));
            assert!(link.starts_with("cmux://ssh?"), "{link}");
            assert!(!link.starts_with("https:"), "{link}");
            assert!(!link.contains("cmux.com/"), "{link}");
        }
        assert_eq!(URL_SCHEME, "cmux");
    }

    #[test]
    fn the_token_is_the_ssh_user() {
        let q = query(&ssh_link(&login(2222, "t")));
        assert_eq!(q[1], ("user".to_string(), "ibssh_AbC-12_x".to_string()));
        assert_eq!(q[0], ("host".to_string(), "ssh.instabox.sh".to_string()));
    }

    /// A signature check that passes or fails without running codesign.
    fn signed(ok: bool) -> impl Fn(&Path) -> bool {
        move |_| ok
    }

    #[test]
    fn finds_the_first_real_app_and_skips_lookalikes() {
        let system = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let dirs = [system.path().to_path_buf(), user.path().to_path_buf()];
        let real = |p: &Path| verify_with(p, signed(true));
        assert_eq!(locate_in(&dirs, real), None, "not installed");
        // A folder named cmux.app without a bundle is not the app.
        std::fs::create_dir_all(user.path().join(BUNDLE)).unwrap();
        assert_eq!(locate_in(&dirs, real), None);
        let app = test_bundle(user.path(), BUNDLE_ID, "0.64.25", false);
        assert_eq!(locate_in(&dirs, real), Some(app.clone()), "~/Applications");
        // A bundle with another identifier in /Applications does not hide the real one.
        test_bundle(system.path(), "com.example.cmux", "9.9.9", false);
        assert_eq!(
            locate_in(&dirs, real),
            Some(app),
            "the lookalike is skipped"
        );
        let app = test_bundle(system.path(), BUNDLE_ID, "0.64.25", false);
        assert_eq!(locate_in(&dirs, real), Some(app), "/Applications first");
        // The right identifier without the signature counts as not installed.
        assert_eq!(locate_in(&dirs, |p| verify_with(p, signed(false))), None);
    }

    #[test]
    fn only_cmux_bundle_id_and_a_good_signature_pass() {
        let dir = tempfile::tempdir().unwrap();
        let good = test_bundle(dir.path(), BUNDLE_ID, "0.64.25", true);
        assert!(verify_with(&good, signed(true)));
        assert!(!verify_with(&good, signed(false)), "a bad signature");
        let dir = tempfile::tempdir().unwrap();
        let other = test_bundle(dir.path(), "com.cmuxterm.app.evil", "0.64.25", false);
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
    fn the_requirement_names_cmux_and_its_developer() {
        assert_eq!(BUNDLE_ID, "com.cmuxterm.app");
        assert!(REQUIREMENT.contains(r#"identifier "com.cmuxterm.app""#));
        assert!(REQUIREMENT.contains("anchor apple generic"));
        assert!(REQUIREMENT.contains(r#"certificate leaf[subject.OU] = "7WLXT3NR37""#));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn codesign_turns_down_an_unsigned_lookalike() {
        // The real check, run on a bundle with cmux's identifier and no code or signature.
        let dir = tempfile::tempdir().unwrap();
        let fake = test_bundle(dir.path(), BUNDLE_ID, "0.64.25", false);
        assert!(!codesign_meets_requirement(&fake));
        assert!(!verify(&fake));
    }

    /// The real check against the installed cmux, when this Mac has it. It runs codesign
    /// only; it does not open the app.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_installed_cmux_passes_the_real_check() {
        let app = Path::new("/Applications/cmux.app");
        if app.join("Contents").join("Info.plist").is_file() {
            assert!(verify(app), "{} did not verify", app.display());
        }
    }

    #[test]
    fn reads_the_version_from_an_xml_or_binary_info_plist() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_bundle(dir.path(), BUNDLE_ID, "0.64.25", false);
        assert_eq!(version(&app).unwrap(), "0.64.25");
        let dir = tempfile::tempdir().unwrap();
        let app = test_bundle(dir.path(), BUNDLE_ID, "0.65.2.2", true);
        assert_eq!(version(&app).unwrap(), "0.65.2.2");
    }

    #[test]
    fn a_missing_or_odd_version_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(version(&dir.path().join(BUNDLE)).is_err(), "no bundle");
        let app = test_bundle(dir.path(), BUNDLE_ID, "1.0; rm -rf /", false);
        assert!(version(&app).is_err());
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join(BUNDLE);
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        plist::Value::Dictionary(plist::Dictionary::new())
            .to_file_xml(app.join("Contents").join("Info.plist"))
            .unwrap();
        assert!(version(&app).is_err(), "no version key");
    }

    #[test]
    fn locate_and_the_signature_check_are_macos_only() {
        if !cfg!(target_os = "macos") {
            assert_eq!(locate(), None);
            assert!(!codesign_meets_requirement(Path::new("cmux.app")));
        }
    }

    #[test]
    fn the_prep_step_makes_the_state_dir_not_group_writable() {
        assert_eq!(
            PREP_SCRIPT,
            "mkdir -p ~/.local/state && chmod go-w ~/.local ~/.local/state"
        );
        assert_eq!(LINK.prep, PREP_SCRIPT);
    }

    #[cfg(unix)]
    #[test]
    fn the_prep_step_works_twice_on_a_group_writable_home() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let local = home.path().join(".local");
        std::fs::create_dir(&local).unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o775)).unwrap();
        for _ in 0..2 {
            let st = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(PREP_SCRIPT)
                .env("HOME", home.path())
                .status()
                .unwrap();
            assert!(st.success());
        }
        for d in [local.clone(), local.join("state")] {
            let mode = std::fs::metadata(&d).unwrap().permissions().mode();
            assert_eq!(mode & 0o022, 0, "{} is {mode:o}", d.display());
        }
    }
}
