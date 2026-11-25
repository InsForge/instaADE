//! The app was called Instabox before it became InstaADE. Installing InstaADE from its `.dmg`
//! leaves `Instabox.app` next to it, with the same identifier and the same sign-in. At launch, a
//! release build on macOS looks for that old copy, and the popover offers to move it to the
//! Trash. Only a folder named `Instabox.app` in `/Applications` or `~/Applications`, with this
//! app's identifier, that this app does not run from, is ever offered or moved.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The old app's name.
const OLD_NAME: &str = "Instabox";
/// The old app's bundle.
const OLD_BUNDLE: &str = "Instabox.app";
/// The identifier the old app shares with this one.
const IDENTIFIER: &str = "sh.instabox.bridge";

/// The old copies the popover offers to move to the Trash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OldAppView {
    pub paths: Vec<String>,
}

/// Whether this build looks for an old copy: only a release build with the old app's
/// identifier. A dev build and InstaADE Preview never do.
pub fn looks(identifier: &str, release: bool) -> bool {
    release && identifier == IDENTIFIER
}

/// The folders an old copy may be in.
pub fn folders(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications")];
    dirs.extend(home.map(|h| h.join("Applications")));
    dirs
}

/// The bundle `exe` runs from: `<X>.app` when `exe` is `<X>.app/Contents/MacOS/<name>`.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let shape = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    shape.then(|| bundle.to_path_buf())
}

/// Whether `bundle` is an old copy: a folder, not a link, named `Instabox.app`, whose
/// `Info.plist` has `identifier`, and not `running`, the bundle this app runs from. An update
/// in place keeps the bundle's old name, so `running` may be named `Instabox.app` too.
pub fn is_old_copy(bundle: &Path, identifier: &str, running: &Path) -> bool {
    bundle.file_name().is_some_and(|n| n == OLD_BUNDLE)
        && std::fs::symlink_metadata(bundle).is_ok_and(|m| m.is_dir())
        && bundle_identifier(bundle).as_deref() == Some(identifier)
        && !same(bundle, running)
}

/// The old copies in `folders`.
pub fn find(folders: &[PathBuf], identifier: &str, running: &Path) -> Vec<PathBuf> {
    folders
        .iter()
        .map(|d| d.join(OLD_BUNDLE))
        .filter(|b| is_old_copy(b, identifier, running))
        .collect()
}

/// Whether one of `executables`, the programs of running processes, runs from `bundle`.
pub fn runs_from(bundle: &Path, executables: &[PathBuf]) -> bool {
    executables
        .iter()
        .filter_map(|e| bundle_of(e))
        .any(|b| same(&b, bundle))
}

/// Moves the old copies the popover offered to the Trash, and says how many moved. Each one
/// is checked again first, and one that is no longer an old copy stays. Nothing moves while
/// one of them runs.
pub fn move_to_trash(
    offered: &[PathBuf],
    is_old: impl Fn(&Path) -> bool,
    runs: impl Fn(&Path) -> bool,
    trash: impl Fn(&Path) -> Result<(), String>,
) -> Result<usize, String> {
    let old: Vec<&PathBuf> = offered.iter().filter(|p| is_old(p)).collect();
    if old.iter().any(|p| runs(p)) {
        return Err(format!("Quit {OLD_NAME}, then click Move to Trash again."));
    }
    for p in &old {
        trash(p).map_err(|e| format!("Could not move {OLD_NAME} to the Trash: {e}"))?;
    }
    Ok(old.len())
}

/// The old copies on this computer, when this build looks for them (see [`looks`]).
pub fn look(identifier: &str, release: bool) -> Option<OldAppView> {
    if !cfg!(target_os = "macos") || !looks(identifier, release) {
        return None;
    }
    let running = running_bundle()?;
    let found = find(&folders(dirs::home_dir().as_deref()), identifier, &running);
    (!found.is_empty()).then(|| OldAppView {
        paths: found.iter().map(|p| p.display().to_string()).collect(),
    })
}

/// Moves the old copies in `view` to the Trash (see [`move_to_trash`]). macOS only.
pub fn trash_offered(view: &OldAppView, identifier: &str) -> Result<usize, String> {
    #[cfg(target_os = "macos")]
    {
        let running = running_bundle()
            .ok_or_else(|| "Could not find where this app runs from".to_string())?;
        let offered: Vec<PathBuf> = view.paths.iter().map(PathBuf::from).collect();
        let executables = macos::running_executables();
        move_to_trash(
            &offered,
            |p| is_old_copy(p, identifier, &running),
            |p| runs_from(p, &executables),
            macos::trash,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (view, identifier);
        Err("Only on macOS".to_string())
    }
}

/// The bundle this process runs from.
fn running_bundle() -> Option<PathBuf> {
    bundle_of(&std::env::current_exe().ok()?)
}

/// The `CFBundleIdentifier` in the bundle's `Contents/Info.plist`.
fn bundle_identifier(bundle: &Path) -> Option<String> {
    let info = plist::Value::from_file(bundle.join("Contents").join("Info.plist")).ok()?;
    let id = info
        .as_dictionary()?
        .get("CFBundleIdentifier")?
        .as_string()?;
    Some(id.to_string())
}

/// Whether `a` and `b` are the same folder, whatever path or case names them. When that
/// cannot be read, they count as the same, so the folder is never offered.
fn same(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => true,
        }
    }
    #[cfg(not(unix))]
    {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => true,
        }
    }
}

/// The running processes and the Trash.
#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};

    use objc2_foundation::{NSFileManager, NSString, NSURL};

    /// The programs of the processes this user can read.
    pub fn running_executables() -> Vec<PathBuf> {
        // SAFETY: a null buffer only asks for the count.
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return Vec::new();
        }
        // Room for processes that start in between.
        let mut pids: Vec<libc::c_int> = vec![0; n as usize + 64];
        let size = std::mem::size_of_val(pids.as_slice()) as libc::c_int;
        // SAFETY: `pids` holds `size` bytes, and the call writes at most that many.
        let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size) };
        if n <= 0 {
            return Vec::new();
        }
        pids.truncate(n as usize);
        pids.into_iter().filter_map(program).collect()
    }

    fn program(pid: libc::c_int) -> Option<PathBuf> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buf` holds the number of bytes given, and the call writes at most that many.
        let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if len <= 0 {
            return None;
        }
        buf.truncate(len as usize);
        Some(PathBuf::from(OsString::from_vec(buf)))
    }

    /// Moves `path` to the Trash, from where the user can put it back.
    pub fn trash(path: &Path) -> Result<(), String> {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|e| e.localizedDescription().to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::running_executables;

        #[test]
        fn the_running_programs_include_this_test() {
            let me = std::env::current_exe().unwrap().canonicalize().unwrap();
            let all = running_executables();
            assert!(
                all.iter().any(|p| p.canonicalize().is_ok_and(|p| p == me)),
                "{me:?} not in {} programs",
                all.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const ID: &str = "sh.instabox.bridge";

    /// Makes `<dir>/<name>` with its executable, and an Info.plist with `id` when given.
    /// Returns the bundle.
    fn bundle(dir: &Path, name: &str, id: Option<&str>) -> PathBuf {
        let app = dir.join(name);
        let macos = app.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("instabox-bridge"), "").unwrap();
        if let Some(id) = id {
            let mut info = plist::Dictionary::new();
            info.insert("CFBundleIdentifier".into(), id.into());
            plist::Value::Dictionary(info)
                .to_file_xml(app.join("Contents").join("Info.plist"))
                .unwrap();
        }
        app
    }

    fn exe(bundle: &Path) -> PathBuf {
        bundle
            .join("Contents")
            .join("MacOS")
            .join("instabox-bridge")
    }

    #[test]
    fn only_a_release_build_with_the_old_identifier_looks() {
        assert!(looks(ID, true));
        assert!(!looks(ID, false), "a dev build");
        assert!(
            !looks("sh.instabox.bridge.preview", true),
            "InstaADE Preview"
        );
        assert!(!looks("com.example.other", true));
    }

    #[test]
    fn it_looks_in_both_applications_folders() {
        let home = Path::new("/Users/jane");
        assert_eq!(
            folders(Some(home)),
            [
                PathBuf::from("/Applications"),
                PathBuf::from("/Users/jane/Applications")
            ]
        );
        assert_eq!(folders(None), [PathBuf::from("/Applications")]);
    }

    #[test]
    fn the_bundle_of_an_executable() {
        let p = Path::new("/Applications/InstaADE.app/Contents/MacOS/instabox-bridge");
        assert_eq!(
            bundle_of(p),
            Some(PathBuf::from("/Applications/InstaADE.app"))
        );
        for p in [
            "/usr/local/bin/instabox-bridge",
            "/Applications/InstaADE/Contents/MacOS/instabox-bridge",
            "/Applications/InstaADE.app/Contents/instabox-bridge",
            "/Applications/InstaADE.app/Contents/Resources/instabox-bridge",
            "instabox-bridge",
        ] {
            assert_eq!(bundle_of(Path::new(p)), None, "{p}");
        }
    }

    #[test]
    fn an_old_copy_next_to_the_new_app_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let new = bundle(&apps, "InstaADE.app", Some(ID));
        let old = bundle(&apps, "Instabox.app", Some(ID));
        assert!(is_old_copy(&old, ID, &new));
        assert_eq!(find(&[apps], ID, &new), [old]);
    }

    #[test]
    fn copies_in_both_folders_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("Applications");
        let user = dir.path().join("home").join("Applications");
        let new = bundle(&system, "InstaADE.app", Some(ID));
        let a = bundle(&system, "Instabox.app", Some(ID));
        let b = bundle(&user, "Instabox.app", Some(ID));
        // A folder that does not exist has none.
        let none = dir.path().join("nowhere");
        assert_eq!(find(&[system, user, none], ID, &new), [a, b]);
    }

    #[test]
    fn the_bundle_this_app_runs_from_is_never_old() {
        // An update in place keeps the folder's old name.
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let running = bundle(&apps, "Instabox.app", Some(ID));
        assert!(!is_old_copy(&running, ID, &running));
        // Named by another path to the same folder.
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&apps, &link).unwrap();
            assert!(!is_old_copy(&link.join("Instabox.app"), ID, &running));
            assert!(!is_old_copy(&running, ID, &link.join("Instabox.app")));
        }
        assert!(find(&[apps], ID, &running).is_empty());
    }

    #[test]
    fn nothing_else_is_an_old_copy() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let new = bundle(&apps, "InstaADE.app", Some(ID));
        // Another identifier, the preview's, none, or an Info.plist that is not a plist.
        let other = bundle(
            &dir.path().join("a"),
            "Instabox.app",
            Some("com.example.other"),
        );
        let preview = bundle(
            &dir.path().join("b"),
            "Instabox.app",
            Some("sh.instabox.bridge.preview"),
        );
        let unknown = bundle(&dir.path().join("c"), "Instabox.app", None);
        let broken = bundle(&dir.path().join("d"), "Instabox.app", None);
        std::fs::write(broken.join("Contents").join("Info.plist"), ID).unwrap();
        // Another name, even with the identifier.
        let renamed = bundle(&dir.path().join("e"), "Instabox 2.app", Some(ID));
        let preview_name = bundle(&dir.path().join("f"), "Instabox Preview.app", Some(ID));
        // A file, not a folder.
        let file = dir.path().join("g").join("Instabox.app");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();
        let missing = dir.path().join("h").join("Instabox.app");
        for b in [
            &other,
            &preview,
            &unknown,
            &broken,
            &renamed,
            &preview_name,
            &file,
            &missing,
        ] {
            assert!(!is_old_copy(b, ID, &new), "{b:?}");
        }
        // The preview looks for nothing, but the identifier check stands on its own too.
        let old = bundle(&dir.path().join("i"), "Instabox.app", Some(ID));
        assert!(!is_old_copy(&old, "sh.instabox.bridge.preview", &new));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_named_like_the_old_app_is_not_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let new = bundle(&dir.path().join("Applications"), "InstaADE.app", Some(ID));
        let real = bundle(&dir.path().join("elsewhere"), "Instabox.app", Some(ID));
        let apps = dir.path().join("home").join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::os::unix::fs::symlink(&real, apps.join("Instabox.app")).unwrap();
        assert!(find(&[apps], ID, &new).is_empty());
    }

    #[test]
    fn a_running_copy_is_found_by_its_program() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let new = bundle(&apps, "InstaADE.app", Some(ID));
        let old = bundle(&apps, "Instabox.app", Some(ID));
        assert!(runs_from(&old, &[exe(&new), exe(&old)]));
        assert!(!runs_from(&old, &[exe(&new)]));
        assert!(!runs_from(&old, &[]));
        // A program elsewhere, or one that is not in a bundle.
        assert!(!runs_from(&old, &[PathBuf::from("/usr/bin/true")]));
        assert!(!runs_from(
            &old,
            &[old.join("Contents").join("Resources").join("x")]
        ));
    }

    #[test]
    fn move_to_trash_moves_each_old_copy() {
        let offered = [
            PathBuf::from("/A/Instabox.app"),
            PathBuf::from("/B/Instabox.app"),
        ];
        let moved = RefCell::new(Vec::new());
        let r = move_to_trash(
            &offered,
            |_| true,
            |_| false,
            |p| {
                moved.borrow_mut().push(p.to_path_buf());
                Ok(())
            },
        );
        assert_eq!(r, Ok(2));
        assert_eq!(*moved.borrow(), offered);
    }

    #[test]
    fn move_to_trash_leaves_what_is_no_longer_an_old_copy() {
        let offered = [
            PathBuf::from("/A/Instabox.app"),
            PathBuf::from("/B/Instabox.app"),
        ];
        let moved = RefCell::new(Vec::new());
        let r = move_to_trash(
            &offered,
            |p| p.starts_with("/B"),
            |_| false,
            |p| {
                moved.borrow_mut().push(p.to_path_buf());
                Ok(())
            },
        );
        assert_eq!(r, Ok(1));
        assert_eq!(*moved.borrow(), [PathBuf::from("/B/Instabox.app")]);
        // Gone by now: nothing to move.
        let r = move_to_trash(&offered, |_| false, |_| false, |p| panic!("moved {p:?}"));
        assert_eq!(r, Ok(0));
    }

    #[test]
    fn move_to_trash_moves_nothing_while_a_copy_runs() {
        let offered = [
            PathBuf::from("/A/Instabox.app"),
            PathBuf::from("/B/Instabox.app"),
        ];
        let r = move_to_trash(
            &offered,
            |_| true,
            |p| p.starts_with("/B"),
            |p| panic!("moved {p:?} while a copy runs"),
        );
        assert_eq!(
            r,
            Err("Quit Instabox, then click Move to Trash again.".into())
        );
    }

    #[test]
    fn move_to_trash_stops_at_the_first_failure() {
        let offered = [
            PathBuf::from("/A/Instabox.app"),
            PathBuf::from("/B/Instabox.app"),
        ];
        let tries = RefCell::new(0);
        let r = move_to_trash(
            &offered,
            |_| true,
            |_| false,
            |_| {
                *tries.borrow_mut() += 1;
                Err("no permission".into())
            },
        );
        assert_eq!(
            r,
            Err("Could not move Instabox to the Trash: no permission".into())
        );
        assert_eq!(*tries.borrow(), 1);
    }

    #[test]
    fn a_dev_build_offers_nothing() {
        assert_eq!(look(ID, false), None);
    }
}
