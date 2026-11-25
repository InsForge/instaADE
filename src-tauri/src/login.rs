//! Start at login. A release build turns it on at its first launch, once. After that the
//! person's choice stands. A debug build never touches the login item by itself, so
//! `bun tauri dev` never makes a dev binary start at login.
//!
//! On macOS 13 and later the login item is the app itself, registered through SMAppService
//! (`mainApp`). A LaunchAgent an older build wrote moves to it at startup. Older macOS and
//! Windows keep the autostart plugin's item: a LaunchAgent, or the Run registry key.

use std::path::Path;

use tauri::{AppHandle, Runtime};
use tauri_plugin_autostart::ManagerExt as _;

/// The file in the app config dir that says the default was applied.
pub const DEFAULT_MARKER: &str = "start-at-login-default";

/// The argument the autostart plugin's item passes, so a start at login does not open the
/// popover. SMAppService cannot pass arguments: see [`at_login`].
pub const AT_LOGIN: &str = "--at-login";

// Four-character codes from the Apple event headers: AppleEvents.h and AERegistry.h.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt"); // kCoreEventClass
const OPEN_APPLICATION: u32 = u32::from_be_bytes(*b"oapp"); // kAEOpenApplication
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PROP_DATA: u32 = u32::from_be_bytes(*b"prdt"); // keyAEPropData
const LAUNCHED_AS_LOGIN_ITEM: u32 = u32::from_be_bytes(*b"lgit"); // keyAELaunchedAsLogInItem

/// What startup does with the login item.
#[derive(Debug, PartialEq, Eq)]
pub enum Startup {
    /// Leave it as it is.
    Keep,
    /// Write it again, so one an older build made gets `--at-login`. Only the autostart
    /// plugin's item has arguments; [`rewrite`] leaves the SMAppService item alone.
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

/// The Apple event that launched the app on macOS.
#[derive(Debug, Clone, Copy)]
pub struct LaunchEvent {
    pub class: u32,
    pub id: u32,
    /// The enum code of its `keyAEPropData` parameter, when it has one.
    pub prop: Option<u32>,
}

/// Whether this launch is a start at login. `flag`: the arguments hold [`AT_LOGIN`], which
/// the autostart plugin's item passes. `event`: the Apple event that launched the app. macOS
/// marks the open-application event of a login item with `keyAELaunchedAsLogInItem`
/// (AERegistry.h). A launch by hand (Finder, Spotlight, `open`) sends it without the mark.
pub fn at_login(flag: bool, event: Option<LaunchEvent>) -> bool {
    flag || event.is_some_and(|e| {
        e.class == CORE_EVENT_CLASS
            && e.id == OPEN_APPLICATION
            && e.prop == Some(LAUNCHED_AS_LOGIN_ITEM)
    })
}

/// A LaunchAgent an older build wrote, as macOS 13 and later sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Legacy {
    /// There is none, or the file is not this app's.
    None,
    /// It is there and macOS runs it at login.
    On,
    /// It is there, but the person turned it off in System Settings.
    Off,
}

/// What startup does with a legacy LaunchAgent.
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Leave everything as it is.
    Nothing,
    /// Register the app as its own login item. Remove the LaunchAgent only once that worked.
    Register,
    /// Remove the LaunchAgent and register nothing.
    Remove,
}

/// `release`: this is a release build. `registered`: the app is its own login item and on.
/// A debug build moves nothing, as it never touches the login item by itself.
pub fn migration(release: bool, legacy: Legacy, registered: bool) -> Migration {
    match (release, legacy, registered) {
        (false, _, _) | (true, Legacy::None, _) => Migration::Nothing,
        (true, Legacy::On, false) => Migration::Register,
        // Already moved, or the person turned it off: keep their choice.
        (true, Legacy::On, true) | (true, Legacy::Off, _) => Migration::Remove,
    }
}

/// The keys auto-launch 0.6 writes into the autostart plugin's LaunchAgent. Every release
/// since v0.1.0 wrote this shape.
const AGENT_KEYS: [&str; 4] = [
    "Label",
    "AssociatedBundleIdentifiers",
    "ProgramArguments",
    "RunAtLoad",
];

/// Whether `plist` is the LaunchAgent the autostart plugin wrote for this app. Any other file
/// is never touched. `label`: the app's name. `exe`: this process's executable.
/// `identifier`: this app's bundle identifier.
///
/// The plist must have exactly the plugin's shape, with `ProgramArguments` of
/// `[<program>, "--at-login"]`, and `<program>` must be `<X>.app/Contents/MacOS/<exe name>`.
/// Then it is ours when `<program>` is this executable, when the bundle at `<X>.app` has this
/// app's identifier (the app moved), or when there is no bundle at `<X>.app` (the app moved
/// or was deleted, so the job can run nothing).
pub fn is_our_agent(plist: &plist::Value, label: &str, exe: &Path, identifier: &str) -> bool {
    let Some(program) = agent_program(plist, label).map(Path::new) else {
        return false;
    };
    let Some(bundle) = exe.file_name().and_then(|name| bundle_of(program, name)) else {
        return false;
    };
    // The plugin writes the canonical path of the executable.
    if program == exe || exe.canonicalize().is_ok_and(|e| e == program) {
        return true;
    }
    match std::fs::symlink_metadata(bundle) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(_) => bundle_identifier(bundle).as_deref() == Some(identifier),
    }
}

/// The program of `plist` when it has exactly the shape the autostart plugin writes for an
/// app named `label`.
fn agent_program<'a>(plist: &'a plist::Value, label: &str) -> Option<&'a str> {
    let dict = plist.as_dictionary()?;
    if dict.len() != AGENT_KEYS.len() || !AGENT_KEYS.iter().all(|k| dict.contains_key(k)) {
        return None;
    }
    let shape = dict.get("Label")?.as_string()? == label
        && dict
            .get("AssociatedBundleIdentifiers")?
            .as_array()?
            .is_empty()
        && dict.get("RunAtLoad")?.as_boolean()?;
    if !shape {
        return None;
    }
    match dict.get("ProgramArguments")?.as_array()?.as_slice() {
        [program, flag] if flag.as_string() == Some(AT_LOGIN) => program.as_string(),
        _ => None,
    }
}

/// `<X>.app` when `program` is the absolute path `<X>.app/Contents/MacOS/<exe_name>`.
fn bundle_of<'a>(program: &'a Path, exe_name: &std::ffi::OsStr) -> Option<&'a Path> {
    use std::path::Component;
    let plain = program
        .components()
        .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if !program.is_absolute() || !plain || program.file_name()? != exe_name {
        return None;
    }
    let macos = program.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let shape = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    shape.then_some(bundle)
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

/// Whether unloading the LaunchAgent's job is safe. Unloading a job stops its process, so not
/// when launchd started this very process from it: then it passed `--at-login` (`flag`), and
/// launchd sets `XPC_SERVICE_NAME` to the job's label. The job then ends at logout.
pub fn may_unload(flag: bool, xpc_service_name: Option<&str>, label: &str) -> bool {
    !flag && xpc_service_name != Some(label)
}

/// Whether the arguments hold [`AT_LOGIN`].
fn has_flag() -> bool {
    std::env::args().any(|a| a == AT_LOGIN)
}

/// Whether this launch is a start at login. Call it first in setup: on macOS the launch's
/// Apple event is current only during `applicationDidFinishLaunching`, which runs setup.
pub fn launched_at_login() -> bool {
    #[cfg(target_os = "macos")]
    let event = macos::launch_event();
    #[cfg(not(target_os = "macos"))]
    let event = None;
    at_login(has_flag(), event)
}

/// Whether the app starts at login now.
pub fn is_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    #[cfg(target_os = "macos")]
    if let Some(item) = macos::Item::new(app) {
        return item.is_enabled();
    }
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Turns start at login on.
pub fn enable<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if let Some(item) = macos::Item::new(app) {
        return item.enable();
    }
    app.autolaunch().enable().map_err(|e| e.to_string())
}

/// Turns start at login off.
pub fn disable<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if let Some(item) = macos::Item::new(app) {
        return item.disable();
    }
    app.autolaunch().disable().map_err(|e| e.to_string())
}

/// Writes the autostart plugin's item again. The SMAppService item has nothing to rewrite.
pub fn rewrite<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if macos::Item::new(app).is_some() {
        return Ok(());
    }
    app.autolaunch().enable().map_err(|e| e.to_string())
}

/// Whether the app is its own login item but macOS waits for the person to allow it in
/// System Settings. Always false where the autostart plugin's item is used.
pub fn needs_approval<R: Runtime>(app: &AppHandle<R>) -> bool {
    #[cfg(target_os = "macos")]
    if let Some(item) = macos::Item::new(app) {
        return item.needs_approval();
    }
    let _ = app;
    false
}

/// Opens Login Items in System Settings, on macOS 13 and later.
pub fn open_login_items() {
    #[cfg(target_os = "macos")]
    macos::open_login_items();
}

/// Moves a LaunchAgent an older build wrote to the SMAppService item, keeping it on or off
/// as the person had it. Runs on every launch of macOS 13 and later; once the LaunchAgent is
/// gone there is nothing left to do.
pub fn migrate<R: Runtime>(app: &AppHandle<R>, release: bool) {
    #[cfg(target_os = "macos")]
    if let Some(item) = macos::Item::new(app) {
        item.migrate(release);
    }
    let _ = (app, release);
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
    // The path must end where the arguments begin: `<exe>.bak` is another program.
    if rest.starts_with(|c: char| !c.is_whitespace()) {
        return None;
    }
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

/// SMAppService and the launch's Apple event.
#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2::{available, msg_send};
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSString, NSURL};
    use objc2_service_management::{
        SMAppService, SMAppServiceStatus, kSMErrorAlreadyRegistered, kSMErrorJobNotFound,
    };
    use tauri::{AppHandle, Runtime};

    use super::{
        LaunchEvent, Legacy, Migration, PROP_DATA, has_flag, is_our_agent, may_unload, migration,
    };

    /// The Apple event macOS launched the app with, while it is current.
    pub fn launch_event() -> Option<LaunchEvent> {
        let event = NSAppleEventManager::sharedAppleEventManager().currentAppleEvent()?;
        Some(describe(&event))
    }

    fn describe(event: &NSAppleEventDescriptor) -> LaunchEvent {
        // These three take or return four-character codes (UInt32), which objc2-foundation
        // binds only with the CoreServices crate.
        let class: u32 = unsafe { msg_send![event, eventClass] };
        let id: u32 = unsafe { msg_send![event, eventID] };
        let prop: Option<Retained<NSAppleEventDescriptor>> =
            unsafe { msg_send![event, paramDescriptorForKeyword: PROP_DATA] };
        LaunchEvent {
            class,
            id,
            prop: prop.map(|p| p.enumCodeValue()),
        }
    }

    pub fn open_login_items() {
        if available!(macos = 13.0) {
            unsafe { SMAppService::openSystemSettingsLoginItems() };
        }
    }

    /// The app as its own login item, with the LaunchAgent an older build may have left.
    pub struct Item {
        service: Retained<SMAppService>,
        /// The app's name: the LaunchAgent's label and file name.
        label: String,
        /// The app's bundle identifier.
        identifier: String,
    }

    impl Item {
        /// None before macOS 13, which has no SMAppService.
        pub fn new<R: Runtime>(app: &AppHandle<R>) -> Option<Self> {
            if !available!(macos = 13.0) {
                return None;
            }
            Some(Self {
                service: unsafe { SMAppService::mainAppService() },
                label: app.package_info().name.clone(),
                identifier: app.config().identifier.clone(),
            })
        }

        fn status(&self) -> SMAppServiceStatus {
            unsafe { self.service.status() }
        }

        pub fn is_enabled(&self) -> bool {
            // A LaunchAgent that could not move yet still starts the app.
            self.status() == SMAppServiceStatus::Enabled || self.legacy() == Legacy::On
        }

        pub fn needs_approval(&self) -> bool {
            self.status() == SMAppServiceStatus::RequiresApproval
        }

        pub fn enable(&self) -> Result<(), String> {
            self.register()?;
            // One login item only: a LaunchAgent left too would start the app twice.
            self.remove_legacy();
            Ok(())
        }

        pub fn disable(&self) -> Result<(), String> {
            let r = match self.status() {
                SMAppServiceStatus::NotRegistered | SMAppServiceStatus::NotFound => Ok(()),
                _ => match unsafe { self.service.unregisterAndReturnError() } {
                    Err(e) if e.code() != kSMErrorJobNotFound as isize => {
                        Err(e.localizedDescription().to_string())
                    }
                    _ => Ok(()),
                },
            };
            self.remove_legacy();
            r
        }

        pub fn migrate(&self, release: bool) {
            let registered = self.status() == SMAppServiceStatus::Enabled;
            match migration(release, self.legacy(), registered) {
                Migration::Nothing => {}
                Migration::Register => match self.register() {
                    Ok(()) => self.remove_legacy(),
                    // The LaunchAgent stays, so the app still starts at login; the next
                    // launch tries again.
                    Err(e) => eprintln!("start at login: could not move the LaunchAgent: {e}"),
                },
                Migration::Remove => self.remove_legacy(),
            }
        }

        /// Registers the app. Already registered counts as done.
        fn register(&self) -> Result<(), String> {
            if self.status() == SMAppServiceStatus::Enabled {
                return Ok(());
            }
            match unsafe { self.service.registerAndReturnError() } {
                Err(e) if e.code() != kSMErrorAlreadyRegistered as isize => {
                    Err(e.localizedDescription().to_string())
                }
                _ => Ok(()),
            }
        }

        /// `~/Library/LaunchAgents/<label>.plist`, the file the autostart plugin writes.
        fn legacy_path(&self) -> Option<PathBuf> {
            let dir = dirs::home_dir()?.join("Library").join("LaunchAgents");
            Some(dir.join(format!("{}.plist", self.label)))
        }

        /// The LaunchAgent, when it is there and is this app's.
        fn legacy_plist(&self) -> Option<PathBuf> {
            let path = self.legacy_path()?;
            let value = plist::Value::from_file(&path).ok()?;
            let exe = std::env::current_exe().ok()?;
            is_our_agent(&value, &self.label, &exe, &self.identifier).then_some(path)
        }

        fn legacy(&self) -> Legacy {
            let Some(path) = self.legacy_plist() else {
                return Legacy::None;
            };
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            match unsafe { SMAppService::statusForLegacyURL(&url) } {
                SMAppServiceStatus::RequiresApproval => Legacy::Off,
                _ => Legacy::On,
            }
        }

        /// Deletes this app's LaunchAgent, then unloads its job when that is safe.
        fn remove_legacy(&self) {
            let Some(path) = self.legacy_plist() else {
                return;
            };
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("start at login: could not remove the LaunchAgent: {e}");
                return;
            }
            let xpc = std::env::var("XPC_SERVICE_NAME").ok();
            if may_unload(has_flag(), xpc.as_deref(), &self.label) {
                let uid = unsafe { libc::getuid() };
                // Fails when the job is not loaded, which is fine.
                let _ = std::process::Command::new("/bin/launchctl")
                    .arg("bootout")
                    .arg(format!("gui/{uid}/{}", self.label))
                    .output();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2::{ClassType, msg_send};
        use objc2_foundation::NSAppleEventDescriptor;

        use super::super::{LAUNCHED_AS_LOGIN_ITEM, PROP_DATA, at_login};
        use super::describe;

        /// An open-application Apple event, built the way loginwindow or Finder sends one.
        fn open_app(prop: Option<u32>) -> Retained<NSAppleEventDescriptor> {
            let target: Option<&AnyObject> = None;
            let event: Retained<NSAppleEventDescriptor> = unsafe {
                msg_send![
                    NSAppleEventDescriptor::class(),
                    appleEventWithEventClass: u32::from_be_bytes(*b"aevt"),
                    eventID: u32::from_be_bytes(*b"oapp"),
                    targetDescriptor: target,
                    returnID: -1i16, // kAutoGenerateReturnID
                    transactionID: 0i32 // kAnyTransactionID
                ]
            };
            if let Some(code) = prop {
                let value = NSAppleEventDescriptor::descriptorWithEnumCode(code);
                let _: () = unsafe {
                    msg_send![&*event, setParamDescriptor: &*value, forKeyword: PROP_DATA]
                };
            }
            event
        }

        #[test]
        fn the_launch_event_is_read_from_a_real_apple_event() {
            let login = describe(&open_app(Some(LAUNCHED_AS_LOGIN_ITEM)));
            assert_eq!(login.prop, Some(LAUNCHED_AS_LOGIN_ITEM));
            assert!(at_login(false, Some(login)));

            let by_hand = describe(&open_app(None));
            assert_eq!(by_hand.prop, None);
            assert!(!at_login(false, Some(by_hand)));
        }
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
    fn the_four_character_codes_match_the_headers() {
        // The hex values AERegistry.h and AppleEvents.h give.
        assert_eq!(PROP_DATA, 0x7072_6474);
        assert_eq!(LAUNCHED_AS_LOGIN_ITEM, 0x6C67_6974);
        assert_eq!(CORE_EVENT_CLASS, 0x6165_7674);
        assert_eq!(OPEN_APPLICATION, 0x6F61_7070);
    }

    fn open_app(prop: Option<&[u8; 4]>) -> Option<LaunchEvent> {
        Some(LaunchEvent {
            class: CORE_EVENT_CLASS,
            id: OPEN_APPLICATION,
            prop: prop.map(|p| u32::from_be_bytes(*p)),
        })
    }

    #[test]
    fn a_login_item_launch_stays_in_the_menu_bar() {
        // macOS opened the app as a login item (SMAppService).
        assert!(at_login(false, open_app(Some(b"lgit"))));
        // The autostart plugin's item passed the flag.
        assert!(at_login(true, None));
        assert!(at_login(true, open_app(None)));
    }

    #[test]
    fn a_launch_by_hand_opens_the_popover() {
        // Finder, Spotlight and `open` send the open-application event without the mark.
        assert!(!at_login(false, open_app(None)));
        // A service item is not a login item.
        assert!(!at_login(false, open_app(Some(b"svit"))));
        // No Apple event at all, as on Windows.
        assert!(!at_login(false, None));
        // The mark counts only on the open-application event.
        let reopen = LaunchEvent {
            class: CORE_EVENT_CLASS,
            id: u32::from_be_bytes(*b"rapp"),
            prop: Some(LAUNCHED_AS_LOGIN_ITEM),
        };
        assert!(!at_login(false, Some(reopen)));
        let other_class = LaunchEvent {
            class: u32::from_be_bytes(*b"GURL"),
            id: OPEN_APPLICATION,
            prop: Some(LAUNCHED_AS_LOGIN_ITEM),
        };
        assert!(!at_login(false, Some(other_class)));
    }

    #[test]
    fn a_debug_build_moves_no_launch_agent() {
        for legacy in [Legacy::None, Legacy::On, Legacy::Off] {
            for registered in [false, true] {
                assert_eq!(migration(false, legacy, registered), Migration::Nothing);
            }
        }
    }

    #[test]
    fn a_launch_agent_that_was_on_moves_to_the_app_item() {
        assert_eq!(migration(true, Legacy::On, false), Migration::Register);
        // Moved before, but the file is back (an older build ran again): only remove it.
        assert_eq!(migration(true, Legacy::On, true), Migration::Remove);
    }

    #[test]
    fn a_launch_agent_turned_off_in_system_settings_stays_off() {
        assert_eq!(migration(true, Legacy::Off, false), Migration::Remove);
        assert_eq!(migration(true, Legacy::Off, true), Migration::Remove);
    }

    #[test]
    fn without_a_launch_agent_nothing_moves() {
        assert_eq!(migration(true, Legacy::None, false), Migration::Nothing);
        assert_eq!(migration(true, Legacy::None, true), Migration::Nothing);
    }

    const EXE: &str = "instabox-bridge";
    const ID: &str = "sh.instabox.bridge";

    /// Makes `<dir>/<name>.app` with its executable, and an Info.plist with `id` when given.
    /// Returns the executable's path.
    fn bundle(dir: &Path, name: &str, id: Option<&str>) -> std::path::PathBuf {
        let contents = dir.join(format!("{name}.app")).join("Contents");
        let macos = contents.join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = macos.join(EXE);
        std::fs::write(&exe, "").unwrap();
        if let Some(id) = id {
            let mut info = plist::Dictionary::new();
            info.insert("CFBundleIdentifier".into(), id.into());
            plist::Value::Dictionary(info)
                .to_file_xml(contents.join("Info.plist"))
                .unwrap();
        }
        exe
    }

    /// The LaunchAgent auto-launch 0.6 writes for the autostart plugin, byte for byte.
    fn agent(label: &str, program: &Path) -> plist::Value {
        let xml = format!(
            "{}\n{}\n\
            <plist version=\"1.0\">\n  \
            <dict>\n  \
                <key>Label</key>\n  \
                <string>{label}</string>\n  \
                <key>AssociatedBundleIdentifiers</key>\n  \
                <array></array>\n  \
                <key>ProgramArguments</key>\n  \
                <array><string>{}</string><string>--at-login</string></array>\n  \
                <key>RunAtLoad</key>\n  \
                <true/>\n  \
            </dict>\n\
            </plist>",
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#,
            program.display(),
        );
        plist::Value::from_reader_xml(xml.as_bytes()).unwrap()
    }

    fn edit(mut plist: plist::Value, f: impl FnOnce(&mut plist::Dictionary)) -> plist::Value {
        f(plist.as_dictionary_mut().unwrap());
        plist
    }

    fn ours(plist: &plist::Value, exe: &Path) -> bool {
        is_our_agent(plist, "Instabox", exe, ID)
    }

    /// The executable of a moved app, and the path its old bundle had, which is now gone.
    fn moved(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let exe = bundle(&dir.join("new"), "Instabox", Some(ID));
        let old = dir.join("old/Instabox.app/Contents/MacOS").join(EXE);
        (exe, old)
    }

    #[test]
    fn the_agent_of_this_executable_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        // No Info.plist needed: it runs this very executable.
        let exe = bundle(dir.path(), "Instabox", None);
        // The plugin writes the canonical path (/private/var for /var on macOS).
        assert!(ours(&agent("Instabox", &exe.canonicalize().unwrap()), &exe));
        assert!(ours(&agent("Instabox", &exe), &exe));
        // Instabox Preview has its own file and label.
        assert!(!is_our_agent(
            &agent("Instabox", &exe),
            "Instabox Preview",
            &exe,
            "sh.instabox.bridge.preview"
        ));
    }

    #[test]
    fn the_agent_of_a_moved_app_whose_old_bundle_is_gone_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, old) = moved(dir.path());
        assert!(ours(&agent("Instabox", &old), &exe));
    }

    #[test]
    fn the_agent_of_our_bundle_at_another_path_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, _) = moved(dir.path());
        let copy = bundle(&dir.path().join("copy"), "Instabox 2", Some(ID));
        assert!(ours(&agent("Instabox", &copy), &exe));
    }

    #[test]
    fn another_bundle_with_an_executable_of_the_same_name_is_not_ours() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, _) = moved(dir.path());
        let other = bundle(&dir.path().join("a"), "Instabox", Some("com.example.other"));
        assert!(!ours(&agent("Instabox", &other), &exe));
        let preview = bundle(
            &dir.path().join("b"),
            "Instabox",
            Some("sh.instabox.bridge.preview"),
        );
        assert!(!ours(&agent("Instabox", &preview), &exe));
        // A bundle whose identity cannot be read.
        let unknown = bundle(&dir.path().join("c"), "Instabox", None);
        assert!(!ours(&agent("Instabox", &unknown), &exe));
    }

    #[test]
    fn a_program_outside_an_app_bundle_is_not_ours() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, _) = moved(dir.path());
        for program in [
            std::path::PathBuf::from("/usr/local/bin").join(EXE),
            dir.path().join("Instabox/Contents/MacOS").join(EXE),
            dir.path().join("Instabox.app/Contents").join(EXE),
            dir.path().join("Instabox.app/Contents/Resources").join(EXE),
            std::path::PathBuf::from("Instabox.app/Contents/MacOS").join(EXE),
            dir.path()
                .join("x/../Instabox.app/Contents/MacOS")
                .join(EXE),
            // Another executable name in a bundle that is gone.
            dir.path().join("gone/Instabox.app/Contents/MacOS/other"),
        ] {
            assert!(!ours(&agent("Instabox", &program), &exe), "{program:?}");
        }
    }

    #[test]
    fn a_plist_of_another_shape_is_not_ours() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, old) = moved(dir.path());
        let good = agent("Instabox", &old);
        assert!(ours(&good, &exe));
        let extra = edit(good.clone(), |d| {
            d.insert("KeepAlive".into(), true.into());
        });
        assert!(!ours(&extra, &exe));
        for key in AGENT_KEYS {
            let missing = edit(good.clone(), |d| {
                d.remove(key);
            });
            assert!(!ours(&missing, &exe), "without {key}");
        }
        assert!(!is_our_agent(&good, "Other", &exe, ID));
        let not_at_load = edit(good.clone(), |d| {
            d.insert("RunAtLoad".into(), false.into());
        });
        assert!(!ours(&not_at_load, &exe));
        let associated = edit(good.clone(), |d| {
            let ids = vec![plist::Value::from(ID)];
            d.insert("AssociatedBundleIdentifiers".into(), ids.into());
        });
        assert!(!ours(&associated, &exe));
        assert!(!ours(&plist::Value::String("Instabox".into()), &exe));
    }

    #[test]
    fn a_wrong_argument_list_is_not_ours() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, old) = moved(dir.path());
        let program = plist::Value::from(old.display().to_string());
        for args in [
            vec![program.clone(), "--hidden".into()],
            vec![program.clone()],
            vec![program.clone(), AT_LOGIN.into(), "--hidden".into()],
            vec![AT_LOGIN.into(), program.clone()],
        ] {
            let plist = edit(agent("Instabox", &old), |d| {
                d.insert("ProgramArguments".into(), args.clone().into());
            });
            assert!(!ours(&plist, &exe), "{args:?}");
        }
    }

    #[test]
    fn the_job_that_started_this_process_is_not_unloaded() {
        // Opened by hand while the job from login is still loaded.
        assert!(may_unload(
            false,
            Some("application.sh.instabox.bridge.1.2"),
            "Instabox"
        ));
        assert!(may_unload(false, None, "Instabox"));
        // Started by the job: by its flag, or by the label launchd sets.
        assert!(!may_unload(true, None, "Instabox"));
        assert!(!may_unload(false, Some("Instabox"), "Instabox"));
        assert!(!may_unload(
            false,
            Some("Instabox Preview"),
            "Instabox Preview"
        ));
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
        assert_eq!(
            quoted_run_command(&format!("{exe}.bak --at-login"), exe),
            None
        );
        assert_eq!(quoted_run_command(exe, exe), Some(format!("\"{exe}\"")));
    }

    #[test]
    fn mark_makes_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join(DEFAULT_MARKER);
        mark(&path).unwrap();
        assert!(path.exists());
    }
}
