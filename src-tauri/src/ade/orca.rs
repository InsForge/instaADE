//! Orca, a desktop app for coding agents. It reaches a sandbox over SSH by itself and installs
//! its own server there under `~/.orca-remote` on the first connect, so no versions must match.
//! InstaADE installs only an activity probe there (`setup_script`). Orca has no link or command
//! that adds an SSH host, but it imports the `Host` blocks of `~/.ssh/config`. So InstaADE
//! writes the sandbox as a host there (`ssh_config`), starts Orca, and the user connects the
//! host under Orca's Settings, SSH Hosts, after Import when Orca was already running.
//! On this computer: find the real Orca app and read its version. On macOS that is its bundle
//! identifier, its signature and its `Info.plist`; on Windows the installed `Orca.exe` and the
//! version in its file header.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ade::{ACTIVITY_DIR, Platform, Probe, SshHost};
use crate::process::{command, run_bounded};

pub const ID: &str = "orca";
pub const NAME: &str = "Orca";
pub const SHORT_NAME: &str = "Orca";
pub const PLATFORMS: &[Platform] = &[Platform::MacOs, Platform::Windows];
pub const DOWNLOAD_URL: &str = "https://github.com/stablyai/orca/releases/latest";
/// Orca takes no links, so the app opens none for it.
pub const URL_SCHEME: &str = "";

pub const SSH_HOST: SshHost = SshHost {
    notice,
    address: None,
    setup: Some(setup_script),
};

/// The sandbox's home, which the user opens in Orca once the host is connected.
const SANDBOX_HOME: &str = "/data/home";

/// What the user does in Orca once InstaADE wrote the host `alias` and started Orca. Orca
/// imports the hosts when it starts, and else only when the user clicks Import under Settings,
/// SSH Hosts, so an Orca that already ran does not list a new host yet.
pub fn notice(alias: &str) -> String {
    format!(
        "In Orca, open Settings, then SSH Hosts. Click Import if {alias} is not listed, then connect it and open {SANDBOX_HOME}."
    )
}

// ---- the sandbox ----

/// The activity probe, installed as `ACTIVITY_DIR/orca`. It asks Orca's server on the sandbox
/// whether an agent works, so the sandbox does not pause under an agent that keeps working
/// after the Orca app quit.
pub const PROBE: Probe = Probe {
    name: "orca",
    script: include_str!("../../assets/orca-probe"),
};

const PROBE_EOF: &str = "INSTABOX_PROBE_EOF";

/// Stages the probe and writes it only when it differs from the one in place, or that one is
/// not executable (the guest agent runs only executable files). Orca works without the probe,
/// so a sandbox where it cannot be written (no sudo) still gets Orca: the script says so on
/// stderr and exits 0, and the sandbox sleeps by its terminals and CPU, as before.
const SETUP_TEMPLATE: &str = r##"probe=__PROBE_PATH__
new=$(mktemp)
trap 'rm -f "$new"' EXIT
cat > "$new" <<'__PROBE_EOF__'
__PROBE__
__PROBE_EOF__
if ! cmp -s "$new" "$probe" || [ ! -x "$probe" ]; then
  { sudo install -d -m 0755 __ACTIVITY_DIR__ && sudo install -m 0755 "$new" "$probe"; } ||
    echo "The Orca activity probe was not installed" >&2
fi
exit 0"##;

/// What Install and Reconnect run on the sandbox before the mark (`SshHost::setup`): it
/// installs the probe. The desktop version does not matter, since Orca brings its own server.
pub fn setup_script(_desktop_version: &str) -> String {
    assert!(
        !PROBE.script.contains(PROBE_EOF),
        "{PROBE_EOF} is in the probe"
    );
    let probe = PROBE.script.strip_suffix('\n').unwrap_or(PROBE.script);
    SETUP_TEMPLATE
        .replace("__PROBE_PATH__", &PROBE.path())
        .replace("__ACTIVITY_DIR__", ACTIVITY_DIR)
        .replace("__PROBE_EOF__", PROBE_EOF)
        .replace("__PROBE__", probe)
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
        locate_exe_in(&exe_paths(), verify_exe)
    } else {
        None
    }
}

/// The newest Orca bundle in `dirs` that `verify` accepts (`ade::newest_app`).
pub fn locate_in(dirs: &[PathBuf], verify: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let apps = dirs
        .iter()
        .map(|d| d.join(BUNDLE))
        .filter(|app| app.join("Contents").join("Info.plist").is_file());
    crate::ade::newest_app(apps, |app| plist_string(app, VERSION_KEY).ok(), verify)
}

/// True when the app at `app` is the real Orca, read from disk each time. macOS: its
/// `Info.plist` names `BUNDLE_ID`, and codesign finds its signature valid and meeting
/// `REQUIREMENT`; blocks for a few seconds. Windows: `verify_exe`, with no signature check yet.
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

/// The newest of `paths` by its version resource that is a file `verify` accepts
/// (`ade::newest_app`). Paseo uses it too.
pub fn locate_exe_in(paths: &[PathBuf], verify: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let exes = paths.iter().filter(|p| p.is_file()).cloned();
    crate::ade::newest_app(exes, |exe| exe_version(exe).ok(), verify)
}

/// The product name Orca's `Orca.exe` (and its installer) has in its version resource.
const EXE_PRODUCT_NAME: &str = "Orca";

/// The Windows check of `verify`: `exe` is a PE file whose version resource names the product
/// `Orca` and holds a version. It reads the file each time and runs nothing. It checks no
/// signature, so another program built to look like Orca would pass.
pub fn verify_exe(exe: &Path) -> bool {
    exe_is_product(exe, EXE_PRODUCT_NAME)
}

/// True when `exe` is a PE file whose version resource names the product `product` exactly and
/// holds a version. Read from the file each time; nothing runs. Paseo's check uses it too.
pub fn exe_is_product(exe: &Path, product: &str) -> bool {
    let Ok(mut f) = File::open(exe) else {
        return false;
    };
    let Ok(resources) = resource_section(&mut f) else {
        return false;
    };
    product_name(&resources).as_deref() == Some(product) && fixed_version(&resources).is_some()
}

/// The `ProductName` string of the version resource in `resources`: the UTF-16 text after the
/// key and its padding.
fn product_name(resources: &[u8]) -> Option<String> {
    let key: Vec<u8> = "ProductName\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let at = resources.windows(key.len()).position(|w| w == key)? + key.len();
    let units: Vec<u16> = (at..resources.len().saturating_sub(1))
        .step_by(2)
        .map(|i| u16_at(resources, i))
        .skip_while(|&u| u == 0)
        .take_while(|&u| u != 0)
        .take(256)
        .collect();
    String::from_utf16(&units).ok().filter(|n| !n.is_empty())
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

/// Test only: a minimal PE file named Orca whose resource section holds a `VS_FIXEDFILEINFO`
/// with the product version `v` (and a file version that differs, to tell them apart).
#[cfg(test)]
pub(crate) fn test_exe(v: [u16; 4]) -> Vec<u8> {
    test_exe_named(v, EXE_PRODUCT_NAME)
}

/// Test only: `test_exe` with the product name `product`.
#[cfg(test)]
pub(crate) fn test_exe_named(v: [u16; 4], product: &str) -> Vec<u8> {
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
    // The product name, as a String of StringFileInfo: key, padding, value.
    let name: Vec<u8> = format!("ProductName\0\0{product}\0")
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    f[0x280..0x280 + name.len()].copy_from_slice(&name);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(ok: bool) -> impl Fn(&Path) -> bool {
        move |_| ok
    }

    #[test]
    fn finds_the_newest_real_app_and_skips_lookalikes() {
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
        // An older copy in /Applications does not hide a newer one in ~/Applications.
        test_bundle(system.path(), BUNDLE_ID, "1.4.31");
        assert_eq!(locate_in(&dirs, real), Some(user.path().join(BUNDLE)));
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
    fn finds_the_newest_exe_and_on_a_tie_the_one_for_the_user() {
        let user = tempfile::tempdir().unwrap();
        let all = tempfile::tempdir().unwrap();
        let paths = [user.path().join("Orca.exe"), all.path().join("Orca.exe")];
        assert_eq!(locate_exe_in(&paths, verify_exe), None);
        std::fs::write(&paths[1], test_exe([1, 4, 269, 0])).unwrap();
        assert_eq!(locate_exe_in(&paths, verify_exe), Some(paths[1].clone()));
        // A file at the user's path that is not Orca does not hide the real one.
        std::fs::write(&paths[0], b"MZ").unwrap();
        assert_eq!(locate_exe_in(&paths, verify_exe), Some(paths[1].clone()));
        std::fs::write(&paths[0], test_exe([1, 4, 269, 0])).unwrap();
        assert_eq!(locate_exe_in(&paths, verify_exe), Some(paths[0].clone()));
        // A newer one for every user wins over an older one for the user.
        std::fs::write(&paths[1], test_exe([1, 4, 270, 0])).unwrap();
        assert_eq!(locate_exe_in(&paths, verify_exe), Some(paths[1].clone()));
    }

    #[test]
    fn the_windows_check_wants_a_pe_named_orca_with_a_version() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Orca.exe");
        assert!(!verify_exe(&exe), "missing");
        std::fs::write(&exe, test_exe([1, 4, 269, 0])).unwrap();
        assert!(verify_exe(&exe));
        // Read from the file each time: a file changed since fails at once.
        let full = test_exe([1, 4, 269, 0]);
        std::fs::write(&exe, &full[..0x250]).unwrap();
        assert!(!verify_exe(&exe), "truncated");
        std::fs::write(&exe, test_exe_named([1, 4, 269, 0], "Notepad")).unwrap();
        assert!(!verify_exe(&exe), "another program");
        std::fs::write(&exe, test_exe_named([1, 4, 269, 0], "Orca2")).unwrap();
        assert!(!verify_exe(&exe), "only the exact name");
        let mut no_version = test_exe([1, 4, 269, 0]);
        no_version[0x252..0x256].fill(0);
        std::fs::write(&exe, &no_version).unwrap();
        assert!(!verify_exe(&exe), "no version");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        assert!(!verify_exe(&exe), "not an exe");
        if cfg!(windows) {
            std::fs::write(&exe, test_exe([1, 4, 269, 0])).unwrap();
            assert!(verify(&exe), "the production check on Windows");
            std::fs::write(&exe, &full[..0x250]).unwrap();
            assert!(!verify(&exe));
        }
    }

    #[test]
    fn the_notice_names_the_host_and_the_sandbox_home() {
        assert_eq!(
            notice("instabox-dev-1"),
            "In Orca, open Settings, then SSH Hosts. Click Import if instabox-dev-1 is not \
             listed, then connect it and open /data/home."
        );
        assert_eq!((SSH_HOST.notice)("x"), notice("x"));
    }

    #[test]
    fn the_setup_installs_the_probe_where_instabox_runs_it() {
        let s = setup_script("1.4.279");
        assert_eq!(
            s,
            setup_script("ignored"),
            "the desktop version does not matter"
        );
        assert_eq!(SSH_HOST.setup.map(|f| f("1.4.279")), Some(s.clone()));
        assert_eq!(PROBE.path(), "/usr/local/lib/instabox/activity.d/orca");
        assert!(s.starts_with("probe=/usr/local/lib/instabox/activity.d/orca\n"));
        let probe = PROBE.script.strip_suffix('\n').unwrap();
        assert!(s.contains(&format!(
            "cat > \"$new\" <<'{PROBE_EOF}'\n{probe}\n{PROBE_EOF}\n"
        )));
        assert!(s.contains("sudo install -d -m 0755 /usr/local/lib/instabox/activity.d"));
        assert!(s.contains("sudo install -m 0755 \"$new\" \"$probe\""));
        assert!(s.ends_with("\nexit 0"));
    }

    #[test]
    fn the_probe_is_python_with_unix_line_ends() {
        assert!(PROBE.script.starts_with("#!/usr/bin/env python3\n"));
        assert!(PROBE.script.contains("\"agent\", \"list\", \"--json\""));
        // A CRLF checkout (Windows) would break the script on the sandbox.
        assert!(!PROBE.script.contains('\r'));
        assert!(!PROBE.script.contains(PROBE_EOF));
    }

    #[cfg(unix)]
    fn write_exe(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Runs the setup with the activity directory in `root` and a `sudo` that runs
    /// `sudo_body`. Returns whether it exited 0, and its stderr.
    #[cfg(unix)]
    fn run_setup(root: &Path, sudo_body: &str) -> (bool, String) {
        let bin = root.join("bin");
        write_exe(&bin.join("sudo"), &format!("#!/bin/sh\n{sudo_body}\n"));
        let dir = root.join("activity.d");
        let script = setup_script("x").replace(ACTIVITY_DIR, &dir.display().to_string());
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        (out.status.success(), err)
    }

    #[cfg(unix)]
    #[test]
    fn the_setup_writes_the_probe_only_when_it_differs_and_never_fails_orca() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("sudo.log");
        let sudo = format!("echo \"$*\" >> '{}'\nexec \"$@\"", log.display());
        let calls = || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .count()
        };
        let probe = root.path().join("activity.d").join("orca");
        let (ok, err) = run_setup(root.path(), &sudo);
        assert!(ok, "{err}");
        assert_eq!(std::fs::read_to_string(&probe).unwrap(), PROBE.script);
        let mode = std::fs::metadata(&probe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(calls(), 2, "the directory, then the probe");
        // The same probe in place: nothing is written.
        assert!(run_setup(root.path(), &sudo).0);
        assert_eq!(calls(), 2);
        // An older probe is replaced.
        std::fs::write(&probe, "#!/usr/bin/env python3\n").unwrap();
        assert!(run_setup(root.path(), &sudo).0);
        assert_eq!(std::fs::read_to_string(&probe).unwrap(), PROBE.script);
        assert_eq!(calls(), 4);
        // The same probe, but not executable, so the guest agent skips it: written again.
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(run_setup(root.path(), &sudo).0);
        let mode = std::fs::metadata(&probe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(calls(), 6);
        // Without a sudo that works, Orca still installs: the setup exits 0 and says why.
        std::fs::remove_file(&probe).unwrap();
        let (ok, err) = run_setup(root.path(), "exit 1");
        assert!(ok);
        assert!(
            err.contains("The Orca activity probe was not installed"),
            "{err}"
        );
        assert!(!probe.exists());
    }

    /// The probe run as instabox-guest-agent runs it: no environment but its own, in the home.
    #[cfg(unix)]
    struct Probed {
        ok: bool,
        stdout: String,
        took: Duration,
    }

    #[cfg(unix)]
    impl Probed {
        fn answer(&self) -> serde_json::Value {
            assert!(self.ok, "the probe failed: {}", self.stdout);
            assert_eq!(self.stdout.lines().count(), 1, "{}", self.stdout);
            assert!(self.stdout.len() <= 4096);
            serde_json::from_str(&self.stdout).unwrap()
        }
    }

    /// Runs the probe with `home` as HOME. None without python3.
    #[cfg(unix)]
    fn run_probe(home: &Path) -> Option<Probed> {
        let Ok(python) = which::which("python3") else {
            eprintln!("python3 not found: skipped");
            return None;
        };
        let script = home.join("probe.py");
        std::fs::write(&script, PROBE.script).unwrap();
        let start = std::time::Instant::now();
        let out = std::process::Command::new(python)
            .arg(&script)
            .env_clear()
            .env("HOME", home)
            .env("SHELL", "/bin/bash")
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("XDG_RUNTIME_DIR", "/run/user/1000")
            .current_dir(home)
            .output()
            .unwrap();
        Some(Probed {
            ok: out.status.success(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            took: start.elapsed(),
        })
    }

    /// Orca's server data in `home/data`, as orcad leaves it: `orca-runtime.json` naming `pid`,
    /// and its CLI launcher, which here runs `cli_body` and logs each call to `home/cli.log`.
    #[cfg(unix)]
    fn orcad(home: &Path, data: &str, pid: u32, cli_body: &str) {
        let data = home.join(data);
        std::fs::create_dir_all(&data).unwrap();
        let runtime = serde_json::json!({
            "runtimeId": "r1", "pid": pid, "transports": [], "authToken": "secret",
            "startedAt": 1
        });
        std::fs::write(data.join("orca-runtime.json"), runtime.to_string()).unwrap();
        let log = home.join("cli.log");
        let cli = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n{cli_body}\n",
            log.display()
        );
        write_exe(&data.join("cli").join("bin").join("orca"), &cli);
    }

    /// A CLI body that prints what `orca agent list --json` prints for these `agents`.
    #[cfg(unix)]
    fn lists(agents: serde_json::Value, truncated: bool) -> String {
        let answer = serde_json::json!({
            "id": "q1", "ok": true,
            "result": {"agents": agents, "truncated": truncated,
                "hostScope": {"hostIds": ["local"], "omittedHostIds": []}},
            "_meta": {"runtimeId": "r1"}
        });
        format!(
            "cat <<'EOF'\n{}\nEOF",
            serde_json::to_string_pretty(&answer).unwrap()
        )
    }

    /// One row of `orca agent list --json`, as Orca 1.4.279 prints it.
    #[cfg(unix)]
    fn agent(state: &str, updated_at: i64) -> serde_json::Value {
        serde_json::json!({
            "orcaSessionId": null, "kind": "terminal", "agent": "claude", "state": state,
            "worktreeId": "w1", "workspace": "repo", "paneKey": "tab:leaf", "title": null,
            "lastAssistantMessage": null, "updatedAt": updated_at
        })
    }

    #[cfg(unix)]
    fn cli_calls(home: &Path) -> usize {
        std::fs::read_to_string(home.join("cli.log"))
            .unwrap_or_default()
            .lines()
            .inspect(|l| assert_eq!(*l, "agent list --json"))
            .count()
    }

    /// A process id that ran and is gone.
    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_is_idle_without_a_running_orca_server_and_never_calls_one() {
        let home = tempfile::tempdir().unwrap();
        let Some(p) = run_probe(home.path()) else {
            return;
        };
        assert_eq!(
            p.answer(),
            serde_json::json!({"busy": false, "reason": "no orca server"})
        );
        // A runtime file left by an orcad that is gone, with a working agent on record.
        let working = lists(serde_json::json!([agent("working", 5)]), false);
        orcad(home.path(), ".orca", dead_pid(), &working);
        assert_eq!(run_probe(home.path()).unwrap().answer()["busy"], false);
        orcad(home.path(), ".orca", 0, &working);
        assert_eq!(run_probe(home.path()).unwrap().answer()["busy"], false);
        assert_eq!(cli_calls(home.path()), 0, "the CLI never runs");
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_is_busy_only_while_an_agent_works() {
        let home = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let cases = [
            (
                serde_json::json!([]),
                false,
                false,
                "no active agents (0 idle)",
                0,
            ),
            (
                serde_json::json!([agent("done", 7), agent("working", 9)]),
                false,
                true,
                "1 agents active (working)",
                9,
            ),
            // Waiting on a person or done is idle.
            (
                serde_json::json!([agent("waiting", 11), agent("blocked", 3), agent("done", 2)]),
                false,
                false,
                "no active agents (3 idle)",
                11,
            ),
            // A state this probe does not know may be work.
            (
                serde_json::json!([agent("done", 1), agent("compacting", 4)]),
                false,
                true,
                "1 agents active (compacting)",
                4,
            ),
            // A list Orca cut short may leave out a working agent.
            (
                serde_json::json!([agent("done", 6)]),
                true,
                true,
                "orca listed only some agents",
                6,
            ),
        ];
        for (n, (agents, truncated, busy, reason, seq)) in cases.into_iter().enumerate() {
            orcad(home.path(), ".orca", me, &lists(agents, truncated));
            let Some(p) = run_probe(home.path()) else {
                return;
            };
            assert_eq!(
                p.answer(),
                serde_json::json!({"busy": busy, "reason": reason, "seq": seq}),
                "case {n}"
            );
            assert_eq!(cli_calls(home.path()), n + 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_finds_a_server_whose_data_orca_keyed_by_machine() {
        let home = tempfile::tempdir().unwrap();
        let idle = lists(serde_json::json!([agent("done", 3)]), false);
        orcad(home.path(), ".orca", dead_pid(), &idle);
        let working = lists(serde_json::json!([agent("working", 8)]), false);
        orcad(home.path(), ".orca-0123abcd", std::process::id(), &working);
        // Orca's install folder has no runtime file.
        std::fs::create_dir_all(home.path().join(".orca-remote")).unwrap();
        let Some(p) = run_probe(home.path()) else {
            return;
        };
        assert_eq!(
            p.answer(),
            serde_json::json!({"busy": true, "reason": "1 agents active (working)", "seq": 8})
        );
        assert_eq!(cli_calls(home.path()), 1, "only the server that runs");
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_fails_when_the_server_does_not_answer_in_time() {
        let home = tempfile::tempdir().unwrap();
        let me = std::process::id();
        // An Orca without `agent list`: the CLI fails, and so does the probe (idle).
        orcad(
            home.path(),
            ".orca",
            me,
            "echo 'unknown command' >&2\nexit 1",
        );
        let Some(p) = run_probe(home.path()) else {
            return;
        };
        assert!(!p.ok);
        assert_eq!(p.stdout, "");
        orcad(home.path(), ".orca", me, "echo 'Orca is starting'");
        let p = run_probe(home.path()).unwrap();
        assert!(!p.ok && p.stdout.is_empty(), "not JSON");
        // A CLI that hangs is cut off within the guest agent's 5 s.
        orcad(home.path(), ".orca", me, "exec sleep 30");
        let p = run_probe(home.path()).unwrap();
        assert!(!p.ok && p.stdout.is_empty());
        assert!(p.took < Duration::from_secs(5), "{:?}", p.took);
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
