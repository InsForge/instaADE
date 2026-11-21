pub mod ade;
pub mod agents;
pub mod api;
pub mod app;
pub mod auth;
pub mod commands;
pub mod connected;
pub mod errors;
pub mod gate;
pub mod login;
pub mod mcp;
pub mod process;
pub mod sandbox;
pub mod ssh;
pub mod tray;

use std::sync::Arc;

use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_opener::OpenerExt;

use crate::app::{AppState, Core, Opener, api_base};
use crate::connected::FileConnectedStore;
use crate::mcp::KeyringKeyStore;
use crate::ssh::KeyringSshStore;

/// The argument the login item passes, so a start at login does not open the popover.
const AT_LOGIN: &str = "--at-login";

pub fn run() {
    let app = tauri::Builder::default()
        // Single instance first: a second launch opens the running popover.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_popover(app, None);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![AT_LOGIN]),
        ))
        .manage(tray::TrayMemo::default())
        .manage(tray::HiddenAt::default())
        .setup(|app| {
            let handle = app.handle().clone();
            // A menu-bar app: no Dock icon.
            #[cfg(target_os = "macos")]
            let _ = handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
            // With an app, the opener runs `open -a <app> -- <url>` on macOS: only that app
            // gets the URL, whatever else handles its scheme.
            let opener: Opener = {
                let h = handle.clone();
                Arc::new(move |url: &str, app: Option<&std::path::Path>| {
                    h.opener()
                        .open_url(url, app.map(|p| p.to_string_lossy().into_owned()))
                        .map_err(|e| e.to_string())
                })
            };
            let emit = {
                let h = handle.clone();
                Arc::new(move |state: &AppState| {
                    let _ = h.emit("state", state);
                    tray::update(&h, state);
                })
            };
            let core = Arc::new(Core::new(
                api_base(std::env::var("INSTABOX_API_URL").ok()),
                Arc::new(KeyringKeyStore),
                Arc::new(KeyringSshStore),
                Arc::new(FileConnectedStore::new(
                    handle
                        .path()
                        .app_config_dir()
                        .unwrap_or_default()
                        .join("connected.json"),
                )),
                opener,
                emit,
                dirs::home_dir().unwrap_or_default(),
            ));
            let launch = handle.autolaunch();
            // Without a config dir there is nowhere to save the marker, so no default.
            let marker = handle
                .path()
                .app_config_dir()
                .ok()
                .map(|d| d.join(login::DEFAULT_MARKER));
            let marked = marker.as_ref().is_none_or(|m| m.exists());
            let enabled = launch.is_enabled().unwrap_or(false);
            match login::startup(!cfg!(debug_assertions), marked, enabled) {
                login::Startup::Keep => {}
                login::Startup::Rewrite => {
                    if let Err(e) = launch.enable() {
                        eprintln!("start at login: could not update the login item: {e}");
                    }
                }
                login::Startup::TurnOn => {
                    // Marked only once the item is on, so a failed attempt is tried again next
                    // launch. A default that cannot be marked is undone, so it never turns the
                    // item back on after the person turned it off.
                    if let Some(m) = marker.as_deref() {
                        match launch.enable() {
                            Ok(()) => {
                                if let Err(e) = login::mark(m) {
                                    eprintln!("start at login: could not save the default: {e}");
                                    let _ = launch.disable();
                                }
                            }
                            Err(e) => eprintln!("start at login: could not turn it on: {e}"),
                        }
                    }
                }
            }
            let at_login = launch.is_enabled().unwrap_or(false);
            #[cfg(windows)]
            if at_login {
                login::quote_windows_run_entry(&handle.package_info().name);
            }
            core.set_start_at_login(at_login);
            app.manage(core.clone());
            tray::build(&handle, core.clone())?;
            // Opened by hand: show where the app lives. Started at login: stay in the menu bar.
            if !std::env::args().any(|a| a == AT_LOGIN) {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    // Give the menu bar time to lay out the icon.
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    tray::show_popover(&h, None);
                });
            }

            let starter = core.clone();
            tauri::async_runtime::spawn(async move { starter.start().await });
            let h = handle.clone();
            tauri::async_runtime::spawn(core.run_loop(move || {
                h.get_webview_window(tray::MAIN_WINDOW)
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false)
            }));
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Closing the popover keeps the app in the menu bar.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Focused(false) => tray::hide_on_blur(window),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::sign_in,
            commands::sign_out,
            commands::switch_org,
            commands::create_sandbox,
            commands::connect_sandbox,
            commands::dismiss_progress,
            commands::refresh_t3,
            commands::open_in_t3,
            commands::pair_again,
            commands::pause,
            commands::resume,
            commands::delete,
            commands::resize,
            commands::set_auto_sleep,
            commands::reinstall_t3,
            commands::agent_sign_in,
            commands::agent_status,
            commands::ssh_command,
            commands::open_ssh,
            commands::add_mcp,
            commands::set_start_at_login,
            commands::open_url,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Instabox");

    app.run(|_app, _event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            tray::show_popover(_app, None);
        }
    });
}
