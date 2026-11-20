//! Find the local T3 Code install and read its version.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::process::{RunError, command, run_bounded};

pub const DOWNLOAD_URL: &str = "https://github.com/pingdotgg/t3code/releases/latest";

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
