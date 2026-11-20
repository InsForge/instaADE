//! Shell scripts run on a sandbox over the API's exec, and parsers for their output.
//! The layout matches the instabox t3 template.

use serde::Deserialize;

/// The activity probe, written to the sandbox by the setup script.
const PROBE: &str = include_str!("../../assets/t3-probe");

const PROBE_EOF: &str = "INSTABOX_PROBE_EOF";

/// Exit code of the setup script when T3 is busy and the update is deferred.
pub const SETUP_EXIT_BUSY: i32 = 3;

pub const PAIR_SCRIPT: &str =
    r#"t3 pair --base-dir "$HOME/.t3-instabox/$(cat /etc/machine-id)" --label 'Instabox'"#;
pub const VERSION_SCRIPT: &str = "t3 --version 2>/dev/null || true";
pub const PROBE_SCRIPT: &str = "/usr/local/lib/instabox/activity.d/t3";

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
        !PROBE.contains(PROBE_EOF),
        "probe contains the heredoc end marker"
    );
    let probe = PROBE.strip_suffix('\n').unwrap_or(PROBE);
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
    use std::process::Command;

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
    fn setup_script_is_valid_sh() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("setup.sh");
        std::fs::write(&p, setup_script("0.1.10")).unwrap();
        let st = Command::new("/bin/sh").arg("-n").arg(&p).status().unwrap();
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
        assert!(PROBE.starts_with("#!/usr/bin/env python3\n"));
        assert!(PROBE.contains("statev2.sqlite"));
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
