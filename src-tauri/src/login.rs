//! Start at login. A release build turns it on at its first launch, once. After that the
//! person's choice stands. A debug build never touches the login item, so `bun tauri dev`
//! never makes a dev binary start at login.

use std::path::Path;

/// The file in the app config dir that says the default was applied.
pub const DEFAULT_MARKER: &str = "start-at-login-default";

/// What startup does with the login item.
#[derive(Debug, PartialEq, Eq)]
pub enum Startup {
    /// Leave it as it is.
    Keep,
    /// Write it again, so one an older build made gets `--at-login`.
    Rewrite,
    /// The first launch of a release build: save the marker, then turn it on.
    TurnOn,
}

/// `release`: this is a release build. `marked`: the default was applied before.
/// `enabled`: the login item is on now.
pub fn startup(release: bool, marked: bool, enabled: bool) -> Startup {
    match (release, marked, enabled) {
        (false, _, _) => Startup::Keep,
        (true, false, _) => Startup::TurnOn,
        (true, true, true) => Startup::Rewrite,
        (true, true, false) => Startup::Keep,
    }
}

/// Saves the marker at `path`, with its folder.
pub fn mark(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, "")
}

/// This program's Windows Run entry with its path in quotes, so a path with a space in it
/// (a user name such as "Jane Doe") starts the right program. None when the entry is
/// already quoted or is not this program's.
pub fn quoted_run_command(value: &str, exe: &str) -> Option<String> {
    let rest = value.strip_prefix(exe)?;
    Some(format!("\"{exe}\"{rest}"))
}

/// The autostart plugin writes the current user's Run entry without quotes: rewrite it
/// with them. A failure is only logged; the unquoted entry still works for most paths.
#[cfg(windows)]
pub fn quote_windows_run_entry(app_name: &str) {
    use windows_registry::CURRENT_USER;
    const RUN: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.display().to_string();
    let r = CURRENT_USER.create(RUN).and_then(|key| {
        let value = key.get_string(app_name)?;
        match quoted_run_command(&value, &exe) {
            Some(quoted) => key.set_string(app_name, quoted),
            None => Ok(()),
        }
    });
    if let Err(e) = r {
        eprintln!("start at login: could not quote the Run entry: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_debug_build_never_touches_the_login_item() {
        for marked in [false, true] {
            for enabled in [false, true] {
                assert_eq!(startup(false, marked, enabled), Startup::Keep);
            }
        }
    }

    #[test]
    fn the_first_release_launch_turns_it_on() {
        assert_eq!(startup(true, false, false), Startup::TurnOn);
        assert_eq!(startup(true, false, true), Startup::TurnOn);
    }

    #[test]
    fn after_the_default_the_choice_stands() {
        assert_eq!(startup(true, true, false), Startup::Keep);
        assert_eq!(startup(true, true, true), Startup::Rewrite);
    }

    #[test]
    fn the_run_entry_gets_its_path_quoted_once() {
        let exe = r"C:\Users\Jane Doe\AppData\Local\Instabox\instabox-bridge.exe";
        let quoted = quoted_run_command(&format!("{exe} --at-login"), exe).unwrap();
        assert_eq!(quoted, format!("\"{exe}\" --at-login"));
        assert_eq!(quoted_run_command(&quoted, exe), None);
        assert_eq!(
            quoted_run_command(r"C:\Other\app.exe --at-login", exe),
            None
        );
    }

    #[test]
    fn mark_makes_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join(DEFAULT_MARKER);
        mark(&path).unwrap();
        assert!(path.exists());
    }
}
