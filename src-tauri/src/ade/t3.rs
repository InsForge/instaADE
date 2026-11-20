//! T3 Code. On this computer: find the T3 CLI and read its version. On a sandbox: shell
//! scripts run over the API's exec, and parsers for their output. The sandbox layout matches
//! the instabox t3 template.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ade::{Pairing, Platform, Probe};
use crate::process::{RunError, command, run_bounded};

pub const ID: &str = "t3";
pub const NAME: &str = "T3 Code";
pub const SHORT_NAME: &str = "T3";
pub const PLATFORMS: &[Platform] = &[Platform::MacOs, Platform::Windows];
pub const DOWNLOAD_URL: &str = "https://github.com/pingdotgg/t3code/releases/latest";
pub const URL_SCHEME: &str = "t3code";

// ---- this computer ----

/// A stuck `t3 --version` counts as not found after this long, so it cannot hang startup.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Path of the T3 CLI, if it exists.
pub fn locate() -> Option<PathBuf> {
    let bin = dirs::home_dir()?.join(".t3").join("bin");
    let names: &[&str] = if cfg!(windows) {
        &["t3.cmd", "t3.exe"]
    } else {
        &["t3"]
    };
    names.iter().map(|n| bin.join(n)).find(|p| p.is_file())
}

/// Parse `t3 v0.1.10` into `0.1.10`.
pub fn parse_version(out: &str) -> Option<String> {
    out.split_whitespace().find_map(|token| {
        let v = token.strip_prefix('v').unwrap_or(token);
        let core = v.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<&str> = core.split('.').collect();
        let ok = parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
        ok.then(|| v.to_string())
    })
}

/// Run `<cli> --version` and parse the result. Gives up after 5 s.
pub fn version(cli: &Path) -> Result<String> {
    version_within(cli, LOCAL_TIMEOUT)
}

fn version_within(cli: &Path, timeout: Duration) -> Result<String> {
    let mut cmd = command(cli);
    cmd.arg("--version");
    let (code, stdout, _) = match run_bounded(cmd, timeout) {
        Ok(c) => c,
        Err(RunError::Spawn(e)) => bail!("failed to run {}: {e}", cli.display()),
        Err(RunError::TimedOut) => bail!(
            "{} --version did not answer within {} s",
            cli.display(),
            timeout.as_secs_f32()
        ),
    };
    if code != Some(0) {
        bail!("{} --version exited with {code:?}", cli.display());
    }
    let text = String::from_utf8_lossy(&stdout);
    parse_version(&text).with_context(|| format!("unrecognized version output: {}", text.trim()))
}

// ---- the sandbox ----

/// The activity probe, written to the sandbox by the setup script.
pub const PROBE: Probe = Probe {
    name: "t3",
    script: include_str!("../../assets/t3-probe"),
};

const PROBE_EOF: &str = "INSTABOX_PROBE_EOF";

/// The port `t3 serve` listens on, published for pairing.
pub const PORT: u16 = 3774;

/// Opens the T3 Code app without a pairing link.
pub const APP_URL: &str = "t3code://";

pub const PAIR_SCRIPT: &str =
    r#"t3 pair --base-dir "$HOME/.t3-instabox/$(cat /etc/machine-id)" --label 'Instabox'"#;
pub const VERSION_SCRIPT: &str = "t3 --version 2>/dev/null || true";
// The setup script starts an inactive service and waits for it, so stop it first.
pub const STOP_SCRIPT: &str =
    "sudo systemctl stop t3-instabox; sudo systemctl reset-failed t3-instabox 2>/dev/null || true";

pub const PAIRING: Pairing = Pairing {
    script: PAIR_SCRIPT,
    port: PORT,
    app_url: APP_URL,
    code: parse_pair_code,
    link: pairing_link,
};

const SETUP_TEMPLATE: &str = r##"set -eu
v=__VERSION__
name="t3-$v-linux-x64.tar.gz"
base="https://github.com/pingdotgg/t3code/releases/download/v$v"
if [ ! -x "/opt/t3/$v/t3" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL -o "$tmp/$name" "$base/$name"
  curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
  awk -v f="$name" '$2 == f || $2 == "*" f' "$tmp/SHA256SUMS" > "$tmp/want"
  [ -s "$tmp/want" ] || { echo "no checksum for $name in SHA256SUMS" >&2; exit 1; }
  (cd "$tmp" && sha256sum -c --quiet want)
  sudo install -d -m 0755 "/opt/t3/$v"
  sudo tar -xzf "$tmp/$name" -C "/opt/t3/$v" --strip-components=1 --no-same-owner
fi
sudo install -d -m 0755 /usr/local/lib/instabox/activity.d
ptmp=$(mktemp)
cat > "$ptmp" <<'__PROBE_EOF__'
__PROBE__
__PROBE_EOF__
sudo install -m 0755 "$ptmp" /usr/local/lib/instabox/activity.d/t3
rm -f "$ptmp"
changed=0
if [ "$(readlink /usr/local/bin/t3)" != "/opt/t3/$v/t3" ]; then
  if systemctl is-active -q t3-instabox; then
    answer=$(/usr/local/lib/instabox/activity.d/t3 2>/dev/null || true)
    case "$answer" in
      *'"busy": false'*|*'"busy":false'*) ;;
      *) echo "T3 is busy, update deferred" >&2; exit 3 ;;
    esac
  fi
  sudo ln -sfn "/opt/t3/$v/t3" /usr/local/bin/t3
  changed=1
fi
first=0
[ -e /etc/systemd/system/t3-instabox.service ] || first=1
(umask 077
mkdir -p "$HOME/.local/share/t3-instabox"
[ -e "$HOME/.local/share/t3-instabox/start" ] || cat > "$HOME/.local/share/t3-instabox/start" <<'S'
#!/bin/sh
set -eu
export PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"
identity=$(cat /etc/machine-id)
case "$identity" in ''|*[!a-f0-9]*) exit 1;; esac
exec t3 serve --host 0.0.0.0 --port 3774 --base-dir "$HOME/.t3-instabox/$identity" /data/workspace
S
chmod 0700 "$HOME/.local/share/t3-instabox/start")
sudo tee /etc/systemd/system/t3-instabox.service >/dev/null <<U
[Unit]
Description=T3 Code on Instabox
After=network-online.target instabox-data-init.service
RequiresMountsFor=/data
StartLimitIntervalSec=300
StartLimitBurst=10
[Service]
User=$(id -u)
Environment=HOME=$HOME LANG=en_US.UTF-8 XDG_RUNTIME_DIR=/run/user/$(id -u)
WorkingDirectory=/data/workspace
ExecStart=$HOME/.local/share/t3-instabox/start
Restart=on-failure
RestartSec=3
LogFilterPatterns=~^Token:
LogFilterPatterns=~^Pairing URL:
LogFilterPatterns=~pairingUrl:
LogFilterPatterns=~^\\s*(\\xe2\\x96[\\x80\\x84\\x88]\\s*)+\$
[Install]
WantedBy=multi-user.target
U
sudo systemctl daemon-reload
# Every run: a service whose first install was cut short still starts at boot.
sudo systemctl enable -q t3-instabox
if [ "$first" = 1 ]; then
  sudo systemctl start t3-instabox
elif [ "$changed" = 1 ] || ! systemctl is-active -q t3-instabox; then
  sudo systemctl restart t3-instabox
fi
# t3 pair finds the server by this file, written after the port opens.
state="$HOME/.t3-instabox/$(cat /etc/machine-id)/userdata/server-runtime.json"
for _ in $(seq 60); do [ -s "$state" ] && curl -s -o /dev/null http://127.0.0.1:3774/ && exit 0; sleep 1; done
echo "T3 server did not start" >&2; exit 1"##;

/// True when `v` is safe to put in a script: `[0-9A-Za-z.+-]+`.
pub fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
}

/// Script that installs T3 `version`, installs the probe and starts the service.
/// A version outside `[0-9A-Za-z.+-]+` gives a script that only fails.
pub fn setup_script(version: &str) -> String {
    if !valid_version(version) {
        return "echo 'invalid T3 version' >&2; exit 1".to_string();
    }
    assert!(
        !PROBE.script.contains(PROBE_EOF),
        "probe contains the heredoc end marker"
    );
    let probe = PROBE.script.strip_suffix('\n').unwrap_or(PROBE.script);
    SETUP_TEMPLATE
        .replace("__VERSION__", version)
        .replace("__PROBE_EOF__", PROBE_EOF)
        .replace("__PROBE__", probe)
}

/// The code from the `Token: <code>` line of `t3 pair`.
pub fn parse_pair_code(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|l| {
        let code = l.trim().strip_prefix("Token:")?.trim();
        (!code.is_empty()).then(|| code.to_string())
    })
}

/// The `t3code://pair` link for a published port URL and a pairing code.
pub fn pairing_link(port_url: &str, code: &str) -> String {
    let pairing = format!("{}/pair#token={}", port_url.trim_end_matches('/'), code);
    let mut out = String::from("t3code://pair?pairingUrl=");
    for b in pairing.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ade::SETUP_EXIT_BUSY;

    #[test]
    fn parses_stable() {
        assert_eq!(parse_version("t3 v0.1.10\n").as_deref(), Some("0.1.10"));
    }

    #[test]
    fn parses_nightly() {
        assert_eq!(
            parse_version("t3 v0.1.11-nightly.20261119.4918").as_deref(),
            Some("0.1.11-nightly.20261119.4918")
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_version("garbage"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn version_errors_on_missing_binary() {
        assert!(version(Path::new("/nonexistent/t3")).is_err());
    }

    #[cfg(unix)]
    fn fake_t3(body: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("t3");
        std::fs::write(&cli, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, cli)
    }

    #[cfg(unix)]
    #[test]
    fn a_stuck_t3_counts_as_not_found() {
        let (_dir, cli) = fake_t3("exec sleep 10");
        let start = std::time::Instant::now();
        let e = version_within(&cli, Duration::from_millis(200)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(e.to_string().contains("did not answer"), "{e}");
        let (_dir, cli) = fake_t3("echo 't3 v0.1.10'");
        assert_eq!(version_within(&cli, LOCAL_TIMEOUT).unwrap(), "0.1.10");
    }

    #[test]
    fn download_url_is_latest_release() {
        assert_eq!(
            DOWNLOAD_URL,
            "https://github.com/pingdotgg/t3code/releases/latest"
        );
    }

    #[test]
    fn setup_script_pins_version() {
        let s = setup_script("0.1.10");
        for want in [
            "/opt/t3/$v/t3",
            "v=0.1.10",
            "releases/download/v$v",
            "t3-$v-linux-x64.tar.gz",
            "SHA256SUMS",
            "sha256sum -c",
            "activity.d/t3",
            "0755",
            "systemctl restart t3-instabox",
            "LogFilterPatterns=~^Token:",
        ] {
            assert!(s.contains(want), "missing {want}");
        }
        assert!(s.contains("#!/usr/bin/env python3"));
    }

    #[test]
    fn setup_script_checks_probe_before_link_change() {
        let s = setup_script("0.1.10");
        let probe = s
            .find("answer=$(/usr/local/lib/instabox/activity.d/t3")
            .unwrap();
        let exit3 = s.find("exit 3").unwrap();
        let link = s.find("ln -sfn").unwrap();
        assert!(probe < exit3 && exit3 < link);
        assert_eq!(SETUP_EXIT_BUSY, 3);
    }

    #[test]
    fn setup_script_enables_the_service_on_every_run() {
        let s = setup_script("0.1.10");
        let enable = s.find("\nsudo systemctl enable -q t3-instabox\n").unwrap();
        let first = s.find("if [ \"$first\" = 1 ]; then").unwrap();
        assert!(
            enable < first,
            "enable runs before and outside the first-install branch"
        );
        assert_eq!(s.matches("systemctl enable").count(), 1);
        // The restart rules stay: start on a first install, restart on a change or when down.
        assert!(s.contains(
            "elif [ \"$changed\" = 1 ] || ! systemctl is-active -q t3-instabox; then\n  sudo systemctl restart t3-instabox"
        ));
    }

    #[test]
    fn setup_script_installs_the_probe_where_it_is_run() {
        let s = setup_script("0.1.10");
        let path = PROBE.path();
        assert!(s.contains(&format!("sudo install -m 0755 \"$ptmp\" {path}\n")));
        assert!(s.contains(&format!("answer=$({path} 2>/dev/null")));
        assert!(s.contains(&format!(":{PORT}/")) && s.contains(&format!("--port {PORT} ")));
    }

    #[cfg(unix)]
    #[test]
    fn setup_script_is_valid_sh() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("setup.sh");
        std::fs::write(&p, setup_script("0.1.10")).unwrap();
        let st = std::process::Command::new("/bin/sh")
            .arg("-n")
            .arg(&p)
            .status()
            .unwrap();
        assert!(st.success());
    }

    #[test]
    fn setup_script_rejects_unsafe_version() {
        let s = setup_script("1.0;rm -rf /");
        assert!(!s.contains("rm -rf"));
        assert!(s.contains("exit 1"));
    }

    #[test]
    fn probe_asset_is_python() {
        assert!(PROBE.script.starts_with("#!/usr/bin/env python3\n"));
        assert!(PROBE.script.contains("statev2.sqlite"));
        // A CRLF checkout (Windows) would break the script on the sandbox.
        assert!(!PROBE.script.contains('\r'));
    }

    #[test]
    fn pair_code_and_link() {
        assert_eq!(
            parse_pair_code("Pairing URL: x\nToken: ABCD1234\n").as_deref(),
            Some("ABCD1234")
        );
        assert_eq!(parse_pair_code("nothing"), None);
        assert_eq!(
            pairing_link("https://p-1.instabox.sh", "ABCD1234"),
            "t3code://pair?pairingUrl=https%3A%2F%2Fp-1.instabox.sh%2Fpair%23token%3DABCD1234"
        );
    }
}
