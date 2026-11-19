pub mod agents;
pub mod api;
pub mod app;
pub mod auth;
pub mod commands;
pub mod connected;
pub mod errors;
pub mod gate;
pub mod mcp;
pub mod process;
pub mod sandbox;
pub mod ssh;
pub mod t3;
pub mod tray;

use std::sync::Arc;

use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_opener::OpenerExt;

use crate::app::{AppState, Core, Opener, api_base};
use crate::connected::FileConnectedStore;
use crate::mcp::KeyringKeyStore;
use crate::ssh::KeyringSshStore;

pub fn run() {
    let app = tauri::Builder::default()
        // Single instance first: a second launch focuses the running window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(tray::TrayMemo::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let opener: Opener = {
                let h = handle.clone();
                Arc::new(move |url| {
                    h.opener()
                        .open_url(url, None::<&str>)
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
            core.set_start_at_login(handle.autolaunch().is_enabled().unwrap_or(false));
            app.manage(core.clone());
            tray::build(&handle, core.clone())?;

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
        // Closing the window keeps the app in the menu bar.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
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
        .expect("error while building Instabox Bridge");

    app.run(|_app, _event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            tray::show_main(_app);
        }
    });
}
