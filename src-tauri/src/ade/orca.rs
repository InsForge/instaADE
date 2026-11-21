//! Orca, a desktop app for coding agents. It reaches a sandbox over SSH by itself and installs
//! its own server there under `~/.orca-remote` on the first connect, so InstaBox installs
//! nothing and no versions must match. Orca has no link or command that adds an SSH host, but
//! it imports the `Host` blocks of `~/.ssh/config`. So InstaBox writes the sandbox as a host
//! there (`ssh_config`), starts Orca, and the user connects the host from Orca's SSH menu.
//! On this computer: find the real Orca app and read its version. On macOS that is its bundle
//! identifier, its signature and its `Info.plist`; on Windows the installed `Orca.exe` and the
//! version in its file header.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ade::{Platform, SshHost};
use crate::process::{command, run_bounded};

pub const ID: &str = "orca";
pub const NAME: &str = "Orca";
pub const SHORT_NAME: &str = "Orca";
pub const PLATFORMS: &[Platform] = &[Platform::MacOs, Platform::Windows];
pub const DOWNLOAD_URL: &str = "https://github.com/stablyai/orca/releases/latest";
/// Orca takes no links, so the app opens none for it.
pub const URL_SCHEME: &str = "";

pub const SSH_HOST: SshHost = SshHost { notice };

/// The sandbox's home, which the user opens in Orca once the host is connected.
const SANDBOX_HOME: &str = "/data/home";

/// What the user does in Orca once InstaBox wrote the host `alias` and started Orca.
pub fn notice(alias: &str) -> String {
    format!("In Orca, connect {alias} from the SSH menu, then open {SANDBOX_HOME}.")
}

// ---- this computer: macOS ----

const BUNDLE: &str = "Orca.app";
const VERSION_KEY: &str = "CFBundleShortVersionString";
const ID_KEY: &str = "CFBundleIdentifier";

/// The bundle identifier of the Orca app.
pub const BUNDLE_ID: &str = "com.stablyai.orca";
/// The code requirement the Orca app must meet: Orca's identifier, signed with the Developer ID
/// of Orca's publisher (team 6CX3WHS9HZ). Only that app is started.
pub const REQUIREMENT: &str = r#"identifier "com.stablyai.orca" and anchor apple generic and certificate leaf[subject.OU] = "6CX3WHS9HZ""#;
/// A signature check that takes longer counts as failed.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the Orca app is: on macOS a bundle in `/Applications` or `~/Applications` that
/// `verify` accepts, on Windows the installed `Orca.exe`.
pub fn locate() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let mut dirs = vec![PathBuf::from("/Applications")];
        dirs.extend(dirs::home_dir().map(|h| h.join("Applications")));
        locate_in(&dirs, verify)
    } else if cfg!(windows) {
        locate_exe_in(&exe_paths())
    } else {
        None
    }
}

/// The Orca bundle in the first of `dirs` that has one `verify` accepts.
pub fn locate_in(dirs: &[PathBuf], verify: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(BUNDLE))
        .filter(|app| app.join("Contents").join("Info.plist").is_file())
        .find(|app| verify(app))
}

/// True when the app at `app` is the real Orca. macOS: its `Info.plist` names `BUNDLE_ID`, and
/// codesign finds its signature valid and meeting `REQUIREMENT`; blocks for a few seconds.
/// Windows: the file is still there. Its signature is not checked.
pub fn verify(app: &Path) -> bool {
    if cfg!(windows) {
        app.is_file()
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

/// The version of the Orca app at `app`, such as `1.4.269`: from the bundle's `Info.plist` on
/// macOS, from the exe's version resource on Windows.
pub fn version(app: &Path) -> Result<String> {
    let v = if cfg!(windows) {
        exe_version(app)?
    } else {
        plist_string(app, VERSION_KEY)?
    };
    let ok = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'));
    if !ok {
        bail!("unrecognized Orca version: {v}");
    }
    Ok(v)
}

/// Test only: a bundle named Orca.app in `dir` whose `Info.plist` has `id` and `version`. It
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

/// Where the Orca installer puts `Orca.exe`: for the user, then for every user.
fn exe_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        paths.push(
            PathBuf::from(d)
                .join("Programs")
                .join("Orca")
                .join("Orca.exe"),
        );
    }
    if let Some(d) = std::env::var_os("ProgramFiles") {
        paths.push(PathBuf::from(d).join("Orca").join("Orca.exe"));
    }
    paths
}

/// The first of `paths` that is a file.
pub fn locate_exe_in(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.is_file()).cloned()
}

/// The resource section is read whole; anything larger is not an Orca.exe.
const MAX_RESOURCES: u32 = 64 << 20;
/// `VS_FIXEDFILEINFO.dwSignature`.
const FIXED_INFO_SIGNATURE: [u8; 4] = 0xFEEF_04BDu32.to_le_bytes();
/// The key of the version resource, `VS_VERSIONINFO.szKey`, which the fixed info follows.
const VERSION_INFO_KEY: &str = "VS_VERSION_INFO\0";

/// The product version in an exe's version resource, such as `1.4.269` for 1.4.269.0. It
/// reads the headers and the resource section only, never the whole file, and runs nothing.
pub fn exe_version(exe: &Path) -> Result<String> {
    let mut f = File::open(exe).with_context(|| format!("could not open {}", exe.display()))?;
    let resources = resource_section(&mut f)
        .with_context(|| format!("could not read the resources of {}", exe.display()))?;
    fixed_version(&resources).with_context(|| format!("no version in {}", exe.display()))
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The raw `.rsrc` section of a PE file.
fn resource_section(f: &mut (impl Read + Seek)) -> Result<Vec<u8>> {
    let mut dos = [0u8; 64];
    f.read_exact(&mut dos)?;
    if &dos[..2] != b"MZ" {
        bail!("not an exe");
    }
    let pe = u64::from(u32_at(&dos, 0x3c));
    f.seek(SeekFrom::Start(pe))?;
    let mut header = [0u8; 24];
    f.read_exact(&mut header)?;
    if &header[..4] != b"PE\0\0" {
        bail!("not a PE file");
    }
    let sections = u16_at(&header, 6);
    let optional = u64::from(u16_at(&header, 20));
    f.seek(SeekFrom::Start(pe + 24 + optional))?;
    for _ in 0..sections {
        let mut s = [0u8; 40];
        f.read_exact(&mut s)?;
        if &s[..8] != b".rsrc\0\0\0" {
            continue;
        }
        let (size, at) = (u32_at(&s, 16), u32_at(&s, 20));
        if size > MAX_RESOURCES {
            bail!("the resource section is too large");
        }
        f.seek(SeekFrom::Start(u64::from(at)))?;
        let mut data = vec![0u8; size as usize];
        f.read_exact(&mut data)?;
        return Ok(data);
    }
    bail!("no resource section")
}

/// The product version in the `VS_FIXEDFILEINFO` of the version resource in `resources`: the
/// one right after the key `VS_VERSION_INFO` (UTF-16) and its padding. A fourth part shows only
/// when it is not 0.
fn fixed_version(resources: &[u8]) -> Option<String> {
    let key: Vec<u8> = VERSION_INFO_KEY
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut from = 0;
    while let Some(k) = resources[from..]
        .windows(key.len())
        .position(|w| w == key)
        .map(|k| from + k)
    {
        from = k + 1;
        let after = k + key.len();
        // Up to 3 bytes of padding align the fixed info.
        let Some(pad) = resources
            .get(after..after + 7)
            .and_then(|w| w.windows(4).position(|s| s == FIXED_INFO_SIGNATURE))
        else {
            continue;
        };
        let Some(info) = resources.get(after + pad..after + pad + 24) else {
            break;
        };
        let (ms, ls) = (u32_at(info, 16), u32_at(info, 20));
        let parts = [ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff];
        let shown = if parts[3] == 0 {
            &parts[..3]
        } else {
            &parts[..]
        };
        let text: Vec<String> = shown.iter().map(u32::to_string).collect();
        return Some(text.join("."));
    }
    None
}

/// Test only: a minimal PE file whose resource section holds a `VS_FIXEDFILEINFO` with the
/// product version `v` (and a file version that differs, to tell them apart).
#[cfg(test)]
pub(crate) fn test_exe(v: [u16; 4]) -> Vec<u8> {
    let mut f = vec![0u8; 0x200];
    f[..2].copy_from_slice(b"MZ");
    f[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    f[0x80..0x84].copy_from_slice(b"PE\0\0");
    // Two sections, and an optional header of 0xf0 bytes.
    f[0x86..0x88].copy_from_slice(&2u16.to_le_bytes());
    f[0x94..0x96].copy_from_slice(&0xf0u16.to_le_bytes());
    let table = 0x80 + 24 + 0xf0;
    let mut section = |at: usize, name: &[u8], size: u32, ptr: u32| {
        f[at..at + name.len()].copy_from_slice(name);
        f[at + 16..at + 20].copy_from_slice(&size.to_le_bytes());
        f[at + 20..at + 24].copy_from_slice(&ptr.to_le_bytes());
    };
    section(table, b".text", 0x10, 0x200);
    section(table + 40, b".rsrc", 0x100, 0x210);
    f.resize(0x310, 0);
    // A stray signature first, then the key and 2 bytes of padding before the fixed info. Its
    // structure version is 0, as in Orca's installer.
    f[0x214..0x218].copy_from_slice(&FIXED_INFO_SIGNATURE);
    let key: Vec<u8> = VERSION_INFO_KEY
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    f[0x230..0x250].copy_from_slice(&key);
    let info = 0x252;
    f[info..info + 4].copy_from_slice(&FIXED_INFO_SIGNATURE);
    f[info + 8..info + 12].copy_from_slice(&0x0009_0009u32.to_le_bytes());
    let ms = (u32::from(v[0]) << 16) | u32::from(v[1]);
    let ls = (u32::from(v[2]) << 16) | u32::from(v[3]);
    f[info + 16..info + 20].copy_from_slice(&ms.to_le_bytes());
    f[info + 20..info + 24].copy_from_slice(&ls.to_le_bytes());
    f
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::create_dir_all(user.path().join(BUNDLE)).unwrap();
        assert_eq!(locate_in(&dirs, real), None, "a folder is not the app");
        let app = test_bundle(user.path(), BUNDLE_ID, "1.4.269");
        assert_eq!(locate_in(&dirs, real), Some(app.clone()), "~/Applications");
        test_bundle(system.path(), "com.example.orca", "9.9.9");
        assert_eq!(
            locate_in(&dirs, real),
            Some(app),
            "the lookalike is skipped"
        );
        let app = test_bundle(system.path(), BUNDLE_ID, "1.4.269");
        assert_eq!(locate_in(&dirs, real), Some(app), "/Applications first");
        assert_eq!(locate_in(&dirs, |p| verify_with(p, signed(false))), None);
    }

    #[test]
    fn only_orca_bundle_id_and_a_good_signature_pass() {
        let dir = tempfile::tempdir().unwrap();
        let good = test_bundle(dir.path(), BUNDLE_ID, "1.4.269");
        assert!(verify_with(&good, signed(true)));
        assert!(!verify_with(&good, signed(false)), "a bad signature");
        let dir = tempfile::tempdir().unwrap();
        let other = test_bundle(dir.path(), "com.stablyai.orca.evil", "1.4.269");
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
    fn the_requirement_names_orca_and_its_developer() {
        assert!(REQUIREMENT.contains(r#"identifier "com.stablyai.orca""#));
        assert!(REQUIREMENT.contains("anchor apple generic"));
        assert!(REQUIREMENT.contains(r#"certificate leaf[subject.OU] = "6CX3WHS9HZ""#));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn codesign_turns_down_an_unsigned_lookalike() {
        let dir = tempfile::tempdir().unwrap();
        let fake = test_bundle(dir.path(), BUNDLE_ID, "1.4.269");
        assert!(!codesign_meets_requirement(&fake));
        assert!(!verify(&fake));
    }

    /// The real check against the installed Orca, when this Mac has it. It runs codesign only;
    /// it does not open the app.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_installed_orca_passes_the_real_check() {
        let app = Path::new("/Applications/Orca.app");
        if app.join("Contents").join("Info.plist").is_file() {
            assert!(verify(app), "{} did not verify", app.display());
            assert!(version(app).is_ok());
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn reads_the_version_from_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_bundle(dir.path(), BUNDLE_ID, "1.4.269");
        assert_eq!(version(&app).unwrap(), "1.4.269");
        let dir = tempfile::tempdir().unwrap();
        let app = test_bundle(dir.path(), BUNDLE_ID, "1.0; rm -rf /");
        assert!(version(&app).is_err());
        assert!(version(&dir.path().join("none.app")).is_err());
    }

    #[test]
    fn reads_the_product_version_of_an_exe() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Orca.exe");
        std::fs::write(&exe, test_exe([1, 4, 269, 0])).unwrap();
        assert_eq!(exe_version(&exe).unwrap(), "1.4.269");
        std::fs::write(&exe, test_exe([2, 0, 1, 7])).unwrap();
        assert_eq!(exe_version(&exe).unwrap(), "2.0.1.7");
    }

    #[test]
    fn an_exe_without_a_version_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Orca.exe");
        assert!(exe_version(&exe).is_err(), "missing");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        assert!(exe_version(&exe).is_err(), "not an exe");
        // The resources hold only the stray signature.
        let mut bytes = test_exe([1, 4, 269, 0]);
        bytes[0x252..0x256].fill(0);
        std::fs::write(&exe, &bytes).unwrap();
        assert!(exe_version(&exe).is_err());
        // No resource section.
        let mut bytes = test_exe([1, 4, 269, 0]);
        let table = 0x80 + 24 + 0xf0 + 40;
        bytes[table..table + 5].copy_from_slice(b".data");
        std::fs::write(&exe, &bytes).unwrap();
        assert!(exe_version(&exe).is_err());
        // A truncated file.
        std::fs::write(&exe, &test_exe([1, 4, 269, 0])[..0x250]).unwrap();
        assert!(exe_version(&exe).is_err());
    }

    #[test]
    fn finds_the_exe_for_the_user_before_the_one_for_everyone() {
        let user = tempfile::tempdir().unwrap();
        let all = tempfile::tempdir().unwrap();
        let paths = [user.path().join("Orca.exe"), all.path().join("Orca.exe")];
        assert_eq!(locate_exe_in(&paths), None);
        std::fs::write(&paths[1], b"MZ").unwrap();
        assert_eq!(locate_exe_in(&paths), Some(paths[1].clone()));
        std::fs::write(&paths[0], b"MZ").unwrap();
        assert_eq!(locate_exe_in(&paths), Some(paths[0].clone()));
    }

    #[test]
    fn the_notice_names_the_host_and_the_sandbox_home() {
        assert_eq!(
            notice("instabox-dev-1"),
            "In Orca, connect instabox-dev-1 from the SSH menu, then open /data/home."
        );
        assert_eq!((SSH_HOST.notice)("x"), notice("x"));
    }

    #[test]
    fn locate_and_the_signature_check_need_macos_or_windows() {
        if !cfg!(target_os = "macos") {
            assert!(!codesign_meets_requirement(Path::new("Orca.app")));
        }
        if !cfg!(any(target_os = "macos", windows)) {
            assert_eq!(locate(), None);
        }
    }
}
