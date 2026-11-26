//! Paseo, an app to run coding agents, on macOS and Windows. Every machine it works on runs a
//! Paseo daemon, and the desktop app connects to it. Over SSH, the app runs
//! `ssh -W 127.0.0.1:6767 <host>` through the user's OpenSSH config, so the daemon must already
//! run there: Paseo installs and starts nothing on the host. So InstaADE installs the Paseo CLI
//! on the sandbox and runs its daemon as a service on 127.0.0.1:6767. On this computer it
//! writes the sandbox as a host in the SSH config (`ssh_config`, as for Orca), copies
//! `ssh://<alias>` and starts Paseo. Paseo has no link that adds a host, so the user adds it
//! once, under Settings, Add host, Remote SSH.
//! On this computer: find the real Paseo app and read its version. On macOS that is its bundle
//! identifier, its signature and its `Info.plist`; on Windows the installed `Paseo.exe` and the
//! version in its file header, read as for Orca.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ade::{Platform, SshHost, orca, t3};
use crate::process::{command, run_bounded};

pub const ID: &str = "paseo";
pub const NAME: &str = "Paseo";
pub const SHORT_NAME: &str = "Paseo";
/// Paseo also ships for Linux, where InstaADE does not run.
pub const PLATFORMS: &[Platform] = &[Platform::MacOs, Platform::Windows];
pub const DOWNLOAD_URL: &str = "https://github.com/getpaseo/paseo/releases/latest";
/// Paseo's `paseo://` links open agents, not hosts, so the app opens none.
pub const URL_SCHEME: &str = "";

pub const SSH_HOST: SshHost = SshHost {
    notice,
    address: Some(address),
    setup: Some(setup_script),
};

/// The port the daemon listens on, on the sandbox's loopback only. Paseo's SSH transport
/// reaches it there by default.
pub const DAEMON_PORT: u16 = 6767;

/// The address Paseo adds the host `alias` by. It holds no token: ssh takes the login from the
/// host in the SSH config.
pub fn address(alias: &str) -> String {
    format!("ssh://{alias}")
}

/// What the user does in Paseo once InstaADE wrote the host `alias` and started Paseo.
pub fn notice(alias: &str) -> String {
    format!(
        "In Paseo, add this sandbox once: open Settings, click Add host, then Remote SSH, and enter {}.",
        address(alias)
    )
}

// ---- this computer: macOS ----

const BUNDLE: &str = "Paseo.app";
const VERSION_KEY: &str = "CFBundleShortVersionString";
const ID_KEY: &str = "CFBundleIdentifier";

/// The bundle identifier of the Paseo app.
pub const BUNDLE_ID: &str = "sh.paseo.desktop";
/// The code requirement the Paseo app must meet: Paseo's identifier, signed with the Developer
/// ID of Paseo's publisher (team 99ZMJMKU9Y). Paseo reads the host with the sandbox's SSH
/// token, so only that app is started.
pub const REQUIREMENT: &str = r#"identifier "sh.paseo.desktop" and anchor apple generic and certificate leaf[subject.OU] = "99ZMJMKU9Y""#;
/// A signature check that takes longer counts as failed.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the Paseo app is: on macOS a bundle in `/Applications` or `~/Applications` that
/// `verify` accepts, on Windows the installed `Paseo.exe`.
pub fn locate() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut dirs = vec![PathBuf::from("/Applications")];
        dirs.extend(dirs::home_dir().map(|h| h.join("Applications")));
        locate_in(&dirs, verify)
    } else if cfg!(windows) {
        orca::locate_exe_in(&exe_paths(), verify_exe)
    } else {
        None
    }
}

/// The Paseo bundle in the first of `dirs` that has one `verify` accepts.
pub fn locate_in(dirs: &[PathBuf], verify: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(BUNDLE))
        .filter(|app| app.join("Contents").join("Info.plist").is_file())
        .find(|app| verify(app))
}

/// True when the app at `app` is the real Paseo, read from disk each time. macOS: its
/// `Info.plist` names `BUNDLE_ID`, and codesign finds its signature valid and meeting
/// `REQUIREMENT`; blocks for a few seconds. Windows: `verify_exe`, with no signature check.
pub fn verify(app: &Path) -> bool {
    if cfg!(windows) {
        verify_exe(app)
    } else {
        verify_with(app, codesign_meets_requirement)
    }
}

/// The macOS check of `verify` with the signature check `signed`.
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

/// The version of the Paseo app at `app`, such as `0.17.4`: from the bundle's `Info.plist` on
/// macOS, from the exe's version resource on Windows.
pub fn version(app: &Path) -> Result<String> {
    let v = if cfg!(windows) {
        orca::exe_version(app)?
    } else {
        plist_string(app, VERSION_KEY)?
    };
    if !t3::valid_version(&v) {
        bail!("unrecognized Paseo version: {v}");
    }
    Ok(v)
}

/// Test only: a bundle named Paseo.app in `dir` whose `Info.plist` has `id` and `version`. It
/// has no code and no signature.
#[cfg(test)]
pub(crate) fn test_bundle(dir: &Path, id: &str, version: &str) -> PathBuf {
    let mut d = plist::Dictionary::new();
    d.insert(ID_KEY.into(), id.into());
    d.insert(VERSION_KEY.into(), version.into());
    let app = dir.join(BUNDLE);
    std::fs::create_dir_all(app.join("Contents")).unwrap();
    plist::Value::Dictionary(d)
        .to_file_xml(app.join("Contents").join("Info.plist"))
        .unwrap();
    app
}

// ---- this computer: Windows ----

/// The product name `Paseo.exe` has in its version resource.
const EXE_PRODUCT_NAME: &str = "Paseo";

/// Where Paseo's installer puts `Paseo.exe`: for the user (its default), then for every user.
fn exe_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        paths.push(
            PathBuf::from(d)
                .join("Programs")
                .join("Paseo")
                .join("Paseo.exe"),
        );
    }
    if let Some(d) = std::env::var_os("ProgramFiles") {
        paths.push(PathBuf::from(d).join("Paseo").join("Paseo.exe"));
    }
    paths
}

/// The Windows check of `verify`: `exe` is a PE file whose version resource names the product
/// `Paseo` and holds a version. It reads the file each time and runs nothing. Paseo's Windows
/// app is not signed, so there is no signature to check, and another program built to look
/// like Paseo would pass.
pub fn verify_exe(exe: &Path) -> bool {
    orca::exe_is_product(exe, EXE_PRODUCT_NAME)
}

// ---- the sandbox ----

/// Installs the Paseo CLI `$v` with npm when the sandbox has no `paseo`, and runs its daemon as
/// the `paseo-instabox` service, on 127.0.0.1:6767 only. A `paseo` the sandbox already has
/// stays, whatever its version: Paseo's app and daemon work across versions. The service
/// starts at boot, runs `paseo daemon run` in a login shell, so the daemon finds the agent CLIs
/// on the user's PATH, and restarts when it fails. A running daemon is never restarted: it may
/// be running agents. Then it waits until the daemon answers.
const SETUP_TEMPLATE: &str = r##"set -eu
v=__VERSION__
if ! command -v paseo >/dev/null 2>&1; then
  command -v npm >/dev/null 2>&1 || { echo "Paseo needs Node.js and npm on the sandbox" >&2; exit 1; }
  npm install -g --no-fund --no-audit "@getpaseo/cli@$v"
  command -v paseo >/dev/null 2>&1 || { echo "npm installed Paseo, but paseo is not on the PATH" >&2; exit 1; }
fi
dir="$HOME/.local/share/paseo-instabox"
new=$(mktemp -d)
trap 'rm -rf "$new"' EXIT
cat > "$new/paseo-instabox.service" <<U
[Unit]
Description=Paseo daemon on Instabox
After=network-online.target instabox-data-init.service
RequiresMountsFor=/data
StartLimitIntervalSec=300
StartLimitBurst=10
[Service]
User=$(id -u)
Environment=HOME=$HOME LANG=en_US.UTF-8 XDG_RUNTIME_DIR=/run/user/$(id -u)
Environment=PASEO_LISTEN=127.0.0.1:__PORT__
WorkingDirectory=$HOME
ExecStart=$dir/start
Restart=on-failure
RestartSec=3
[Install]
WantedBy=multi-user.target
U
(umask 077
mkdir -p "$dir"
cat > "$dir/start.new" <<'S'
#!/bin/bash -l
exec paseo daemon run
S
chmod 0700 "$dir/start.new"
mv -f "$dir/start.new" "$dir/start")
sudo install -m 0644 "$new/paseo-instabox.service" /etc/systemd/system/paseo-instabox.service
sudo systemctl daemon-reload
sudo systemctl enable -q paseo-instabox
if ! systemctl is-active -q paseo-instabox; then
  sudo systemctl reset-failed paseo-instabox 2>/dev/null || true
  sudo systemctl start paseo-instabox
fi
for _ in $(seq 60); do
  if curl -fs -o /dev/null http://127.0.0.1:__PORT__/api/health; then exit 0; fi
  sleep 1
done
echo "The Paseo daemon did not start" >&2; exit 1"##;

/// The setup for the desktop app at `version`, which the sandbox gets when it has no Paseo. A
/// version outside `[0-9A-Za-z.+-]+` gives a script that only fails.
pub fn setup_script(version: &str) -> String {
    if !t3::valid_version(version) {
        return "echo 'invalid Paseo version' >&2; exit 1".to_string();
    }
    SETUP_TEMPLATE
        .replace("__VERSION__", version)
        .replace("__PORT__", &DAEMON_PORT.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(ok: bool) -> impl Fn(&Path) -> bool {
        move |_| ok
    }

    #[test]
    fn the_address_names_only_the_host_and_the_notice_says_where_to_add_it() {
        assert_eq!(address("instabox-dev-1"), "ssh://instabox-dev-1");
        assert_eq!(
            notice("instabox-dev-1"),
            "In Paseo, add this sandbox once: open Settings, click Add host, then Remote SSH, \
             and enter ssh://instabox-dev-1."
        );
        assert_eq!((SSH_HOST.notice)("x"), notice("x"));
        assert_eq!(SSH_HOST.address.map(|a| a("x")).as_deref(), Some("ssh://x"));
        assert_eq!(
            SSH_HOST.setup.map(|s| s("0.17.4")),
            Some(setup_script("0.17.4"))
        );
    }

    #[test]
    fn finds_the_first_real_app_and_skips_lookalikes() {
        let system = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let dirs = [system.path().to_path_buf(), user.path().to_path_buf()];
        let real = |p: &Path| verify_with(p, signed(true));
        assert_eq!(locate_in(&dirs, real), None, "not installed");
        std::fs::create_dir_all(user.path().join(BUNDLE)).unwrap();
        assert_eq!(locate_in(&dirs, real), None, "a folder is not the app");
        let app = test_bundle(user.path(), BUNDLE_ID, "0.17.4");
        assert_eq!(locate_in(&dirs, real), Some(app.clone()), "~/Applications");
        test_bundle(system.path(), "sh.paseo.desktop.dev", "9.9.9");
        assert_eq!(
            locate_in(&dirs, real),
            Some(app),
            "the lookalike is skipped"
        );
        let app = test_bundle(system.path(), BUNDLE_ID, "0.17.4");
        assert_eq!(locate_in(&dirs, real), Some(app), "/Applications first");
        assert_eq!(locate_in(&dirs, |p| verify_with(p, signed(false))), None);
    }

    #[test]
    fn only_paseo_bundle_id_and_a_good_signature_pass() {
        let dir = tempfile::tempdir().unwrap();
        let good = test_bundle(dir.path(), BUNDLE_ID, "0.17.4");
        assert!(verify_with(&good, signed(true)));
        assert!(!verify_with(&good, signed(false)), "a bad signature");
        let dir = tempfile::tempdir().unwrap();
        let other = test_bundle(dir.path(), "sh.paseo.desktop.evil", "0.17.4");
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
    fn the_requirement_names_paseo_and_its_developer() {
        assert!(REQUIREMENT.contains(r#"identifier "sh.paseo.desktop""#));
        assert!(REQUIREMENT.contains("anchor apple generic"));
        assert!(REQUIREMENT.contains(r#"certificate leaf[subject.OU] = "99ZMJMKU9Y""#));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn codesign_turns_down_an_unsigned_lookalike() {
        let dir = tempfile::tempdir().unwrap();
        let fake = test_bundle(dir.path(), BUNDLE_ID, "0.17.4");
        assert!(!codesign_meets_requirement(&fake));
        assert!(!verify(&fake));
    }

    /// The real check against the installed Paseo, when this Mac has it. It runs codesign
    /// only; it does not open the app.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_installed_paseo_passes_the_real_check() {
        let app = Path::new("/Applications/Paseo.app");
        if app.join("Contents").join("Info.plist").is_file() {
            assert!(verify(app), "{} did not verify", app.display());
            assert!(version(app).is_ok());
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn reads_the_version_from_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_bundle(dir.path(), BUNDLE_ID, "0.17.4");
        assert_eq!(version(&app).unwrap(), "0.17.4");
        let dir = tempfile::tempdir().unwrap();
        let app = test_bundle(dir.path(), BUNDLE_ID, "1.0; rm -rf /");
        assert!(version(&app).is_err());
        assert!(version(&dir.path().join("none.app")).is_err());
    }

    #[test]
    fn the_windows_check_wants_a_pe_named_paseo_with_a_version() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Paseo.exe");
        assert!(!verify_exe(&exe), "missing");
        std::fs::write(&exe, orca::test_exe_named([0, 17, 4, 0], "Paseo")).unwrap();
        assert!(verify_exe(&exe));
        assert_eq!(orca::exe_version(&exe).unwrap(), "0.17.4");
        std::fs::write(&exe, orca::test_exe([0, 17, 4, 0])).unwrap();
        assert!(!verify_exe(&exe), "Orca is not Paseo");
        std::fs::write(&exe, orca::test_exe_named([0, 17, 4, 0], "Paseo Dev")).unwrap();
        assert!(!verify_exe(&exe), "only the exact name");
        let mut no_version = orca::test_exe_named([0, 17, 4, 0], "Paseo");
        no_version[0x252..0x256].fill(0);
        std::fs::write(&exe, &no_version).unwrap();
        assert!(!verify_exe(&exe), "no version");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        assert!(!verify_exe(&exe), "not an exe");
        if cfg!(windows) {
            std::fs::write(&exe, orca::test_exe_named([0, 17, 4, 0], "Paseo")).unwrap();
            assert!(verify(&exe), "the production check on Windows");
            assert_eq!(version(&exe).unwrap(), "0.17.4");
            assert_eq!(
                orca::locate_exe_in(std::slice::from_ref(&exe), verify_exe),
                Some(exe.clone())
            );
        }
    }

    #[test]
    fn locate_and_the_signature_check_need_macos_or_windows() {
        if !cfg!(target_os = "macos") {
            assert!(!codesign_meets_requirement(Path::new("Paseo.app")));
        }
        if !cfg!(any(target_os = "macos", windows)) {
            assert_eq!(locate(), None);
        }
    }

    #[test]
    fn the_setup_installs_the_desktop_version_and_listens_on_loopback_only() {
        let s = setup_script("0.17.4");
        assert!(s.starts_with("set -eu\nv=0.17.4\n"));
        for want in [
            "npm install -g --no-fund --no-audit \"@getpaseo/cli@$v\"",
            "Environment=PASEO_LISTEN=127.0.0.1:6767\n",
            "#!/bin/bash -l\nexec paseo daemon run\n",
            "/etc/systemd/system/paseo-instabox.service",
            "sudo systemctl enable -q paseo-instabox\n",
            "curl -fs -o /dev/null http://127.0.0.1:6767/api/health",
            "Restart=on-failure",
        ] {
            assert!(s.contains(want), "missing {want}");
        }
        // Nothing listens anywhere but the loopback, and a running daemon is never restarted.
        assert!(!s.contains("0.0.0.0"));
        assert!(!s.contains("systemctl restart"));
        assert!(!s.contains("__"), "every placeholder is filled");
        assert_eq!(DAEMON_PORT, 6767);
    }

    #[test]
    fn the_setup_rejects_an_unsafe_version() {
        let s = setup_script("1.0;rm -rf /");
        assert!(!s.contains("rm -rf"));
        assert!(!s.contains("npm"));
        assert!(s.contains("exit 1"));
    }

    #[cfg(unix)]
    #[test]
    fn the_setup_is_valid_sh() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("setup.sh");
        std::fs::write(&p, setup_script("0.17.4")).unwrap();
        let st = std::process::Command::new("/bin/sh")
            .arg("-n")
            .arg(&p)
            .status()
            .unwrap();
        assert!(st.success());
    }

    /// Runs the setup with `sh`, on stand-ins for `paseo`, `npm`, `sudo`, `systemctl` and
    /// `curl` that log their arguments. `paseo` is on the PATH when `installed`, `npm` adds it
    /// otherwise; the daemon runs when `active`. Returns the exit code, the log and the start
    /// script it wrote.
    #[cfg(unix)]
    fn run_setup(installed: bool, active: bool) -> (i32, String, String) {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let log = home.path().join("log");
        let tool = |name: &str, body: &str| {
            let p = bin.path().join(name);
            std::fs::write(
                &p,
                format!(
                    "#!/bin/sh\necho \"{name} $*\" >> '{}'\n{body}\n",
                    log.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let paseo = "#!/bin/sh\necho 0.17.4\n";
        if installed {
            std::fs::write(bin.path().join("paseo"), paseo).unwrap();
            std::fs::set_permissions(
                bin.path().join("paseo"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        // npm puts paseo on the PATH, as `npm install -g` does on the standard sandbox.
        tool(
            "npm",
            &format!(
                "printf '%s' '{paseo}' > '{dir}/paseo'; chmod 755 '{dir}/paseo'",
                dir = bin.path().display()
            ),
        );
        tool("sudo", "exit 0");
        tool(
            "systemctl",
            if active {
                "exit 0"
            } else {
                "[ \"$1\" = is-active ] && exit 3; exit 0"
            },
        );
        tool("curl", "exit 0");
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(setup_script("0.17.4"))
            .env("HOME", home.path())
            .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
            .output()
            .unwrap();
        let start = home.path().join(".local/share/paseo-instabox/start");
        (
            out.status.code().unwrap_or(-1),
            std::fs::read_to_string(&log).unwrap_or_default(),
            std::fs::read_to_string(start).unwrap_or_default(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn the_setup_installs_paseo_only_when_missing_and_starts_only_a_stopped_daemon() {
        let (code, log, start) = run_setup(false, false);
        assert_eq!(code, 0, "{log}");
        assert!(
            log.contains("npm install -g --no-fund --no-audit @getpaseo/cli@0.17.4\n"),
            "{log}"
        );
        assert!(log.contains("sudo systemctl enable -q paseo-instabox\n"));
        assert!(
            log.contains("sudo systemctl start paseo-instabox\n"),
            "{log}"
        );
        assert!(log.contains("curl -fs -o /dev/null http://127.0.0.1:6767/api/health\n"));
        assert_eq!(start, "#!/bin/bash -l\nexec paseo daemon run\n");

        // A Paseo already there stays, whatever its version, and a running daemon keeps
        // running: it may be running agents.
        let (code, log, _) = run_setup(true, true);
        assert_eq!(code, 0, "{log}");
        assert!(!log.contains("npm"), "{log}");
        assert!(!log.contains("systemctl start"), "{log}");
        assert!(log.contains("sudo systemctl daemon-reload\n"));
    }
}
