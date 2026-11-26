//! Herdr, a terminal workspace manager for coding agents. It has no desktop app: it runs on the
//! sandbox, in a terminal. InstaADE installs it there with Herdr's own install script, then
//! opens the user's terminal with an SSH session that starts `herdr` on the sandbox. Herdr keeps
//! its panes in a server on the sandbox, so closing the terminal leaves the agents running, and
//! the next Open attaches to them again.
//! On this computer: the OpenSSH client that the terminal runs, and its version.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ade::{Platform, Terminal};
use crate::process::{RunError, command, run_bounded};

pub const ID: &str = "herdr";
pub const NAME: &str = "Herdr";
pub const SHORT_NAME: &str = "Herdr";
/// Where InstaADE opens a terminal (`ssh::open_in_terminal`).
pub const PLATFORMS: &[Platform] = &[Platform::MacOs, Platform::Windows];
/// Where to get the OpenSSH client, the one part Herdr needs on this computer. macOS always has
/// it, so only Windows ever shows this.
pub const DOWNLOAD_URL: &str =
    "https://learn.microsoft.com/windows-server/administration/openssh/openssh_install_firstuse";
/// Herdr takes no links, so the app opens none for it.
pub const URL_SCHEME: &str = "";

pub const TERMINAL: Terminal = Terminal { command: "herdr" };

/// What the app says when this computer has no OpenSSH client.
pub const MISSING: &str = "Herdr opens in a terminal over SSH, and this computer has no OpenSSH client. Install it first.";

// ---- this computer ----

/// A stuck `ssh -V` counts as not found after this long, so it cannot hang startup.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(5);

/// The OpenSSH client the terminal runs: `/usr/bin/ssh` on macOS; on Windows the first
/// `ssh.exe` on PATH, else the one Windows installs in `System32\OpenSSH`.
pub fn locate() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from("/usr/bin/ssh")).filter(|p| p.is_file())
    } else if cfg!(windows) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let system = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32").join("OpenSSH"));
        std::env::split_paths(&path)
            .chain(system)
            .map(|dir| dir.join("ssh.exe"))
            .find(|p| p.is_file())
    } else {
        None
    }
}

/// Parses `OpenSSH_10.3p1, LibreSSL 3.3.6` (macOS) or `OpenSSH_for_Windows_9.5p1, LibreSSL
/// 3.8.2` into `OpenSSH 10.3p1`.
pub fn parse_ssh_version(out: &str) -> Option<String> {
    let first = out.split([',', ' ', '\n', '\r']).next()?.trim();
    let v = first
        .strip_prefix("OpenSSH_for_Windows_")
        .or_else(|| first.strip_prefix("OpenSSH_"))?;
    let ok = v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.');
    ok.then(|| format!("OpenSSH {v}"))
}

/// Runs `<ssh> -V` and parses what it prints (on stderr). Gives up after 5 s.
pub fn version(ssh: &Path) -> Result<String> {
    let mut cmd = command(ssh);
    cmd.arg("-V");
    let (code, stdout, stderr) = match run_bounded(cmd, LOCAL_TIMEOUT) {
        Ok(c) => c,
        Err(RunError::Spawn(e)) => bail!("failed to run {}: {e}", ssh.display()),
        Err(RunError::TimedOut) => bail!("{} -V did not answer in time", ssh.display()),
    };
    if code != Some(0) {
        bail!("{} -V exited with {code:?}", ssh.display());
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&stderr),
        String::from_utf8_lossy(&stdout)
    );
    parse_ssh_version(&text).with_context(|| format!("unrecognized ssh version: {}", text.trim()))
}

// ---- the sandbox ----

/// Prints Herdr's version on the sandbox, such as `herdr 0.9.5`, or nothing when it is not
/// installed.
pub const VERSION_SCRIPT: &str = "herdr --version 2>/dev/null || true";

/// Installs Herdr on the sandbox when an SSH command cannot find it, then prints its version.
/// An SSH command runs with sshd's PATH, which has no `~/.local/bin`, where Herdr's install
/// script puts it (and checks its SHA-256). So the script links it as `/usr/local/bin/herdr`,
/// as the sandbox does with its own tools. A Herdr the sandbox already has stays as it is, and
/// so does one in `~/.local/bin`, which `herdr update` keeps up to date.
pub const SETUP_SCRIPT: &str = r#"set -eu
export PATH=/usr/local/bin:/usr/bin:/bin
if ! command -v herdr >/dev/null 2>&1; then
  if [ ! -x "$HOME/.local/bin/herdr" ]; then
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    curl -fsSL https://herdr.dev/install.sh -o "$tmp/install.sh"
    sh "$tmp/install.sh" >/dev/null
  fi
  sudo ln -sfn "$HOME/.local/bin/herdr" /usr/local/bin/herdr
fi
herdr --version 2>/dev/null || true
"#;

/// Parses `herdr 0.9.5` into `0.9.5`: the last such line of the output.
pub fn parse_version(out: &str) -> Option<String> {
    out.lines().rev().find_map(|line| {
        let v = line.trim().strip_prefix("herdr ")?.trim();
        let ok = v.starts_with(|c: char| c.is_ascii_digit())
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
        ok.then(|| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_herdr_prints() {
        assert_eq!(parse_version("herdr 0.9.5\n").as_deref(), Some("0.9.5"));
        assert_eq!(
            parse_version("herdr 0.10.0-preview.1\r\n").as_deref(),
            Some("0.10.0-preview.1")
        );
        assert_eq!(
            parse_version("herdr 0.9.4\nherdr 0.9.5\n").as_deref(),
            Some("0.9.5"),
            "the last line"
        );
        for bad in [
            "",
            "herdr\n",
            "herdr: command not found",
            "herdr v",
            "t3 0.1.10",
        ] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn reads_the_openssh_version_on_macos_and_windows() {
        assert_eq!(
            parse_ssh_version("OpenSSH_10.3p1, LibreSSL 3.3.6\n").as_deref(),
            Some("OpenSSH 10.3p1")
        );
        assert_eq!(
            parse_ssh_version("OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2\r\n").as_deref(),
            Some("OpenSSH 9.5p1")
        );
        assert_eq!(
            parse_ssh_version("OpenSSH_9.6p1 Ubuntu-3ubuntu13.5, OpenSSL 3.0.13").as_deref(),
            Some("OpenSSH 9.6p1")
        );
        for bad in ["", "ssh: illegal option", "OpenSSH_", "OpenSSH_x9"] {
            assert_eq!(parse_ssh_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn setup_installs_only_when_an_ssh_command_cannot_find_herdr() {
        let s = SETUP_SCRIPT;
        assert!(s.starts_with("set -eu\nexport PATH=/usr/local/bin:/usr/bin:/bin\n"));
        let check = s.find("if ! command -v herdr").unwrap();
        let install = s
            .find("curl -fsSL https://herdr.dev/install.sh -o")
            .unwrap();
        let link = s
            .find("sudo ln -sfn \"$HOME/.local/bin/herdr\" /usr/local/bin/herdr")
            .unwrap();
        assert!(check < install && install < link);
        // The installer's banner stays out of the output the version is read from.
        assert!(s.contains("sh \"$tmp/install.sh\" >/dev/null\n"));
        // Nothing else needs root, and the last line prints the version.
        assert_eq!(s.matches("sudo ").count(), 1);
        assert!(s.ends_with(&format!("{VERSION_SCRIPT}\n")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_the_ssh_client_on_this_mac() {
        let ssh = locate().expect("macOS has /usr/bin/ssh");
        assert_eq!(ssh, PathBuf::from("/usr/bin/ssh"));
        assert!(version(&ssh).unwrap().starts_with("OpenSSH "));
    }

    #[test]
    fn a_missing_ssh_client_has_no_version() {
        assert!(version(Path::new("/nonexistent/ssh")).is_err());
    }
}
