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
    fn mark_makes_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join(DEFAULT_MARKER);
        mark(&path).unwrap();
        assert!(path.exists());
    }
}
