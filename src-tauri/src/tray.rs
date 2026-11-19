//! The menu-bar tray: one item per sandbox, then Open and Quit.

use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

use crate::app::{AppState, Core};
use crate::sandbox::view::ViewState;

const TRAY_ID: &str = "main";
pub const MAIN_WINDOW: &str = "main";

/// One sandbox in the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayRow {
    pub id: String,
    pub label: String,
    pub connected: bool,
    pub action: Option<TrayAction>,
}

/// The Pause or Resume item of a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Pause,
    Resume,
}

/// The row title: a filled dot for a sandbox that is up or changing, an empty one for a
/// paused or stopped sandbox.
pub fn row_label(name: &str, state: ViewState) -> String {
    let dot = match state {
        ViewState::Paused | ViewState::Stopped => '\u{25CB}',
        _ => '\u{25CF}',
    };
    format!("{dot} {name}")
}

/// The submenu item that opens T3 Code. An unconnected sandbox is connected first.
pub fn open_label(connected: bool) -> &'static str {
    if connected {
        "Open in T3 Code"
    } else {
        "Connect to T3 Code"
    }
}

pub fn tray_rows(state: &AppState) -> Vec<TrayRow> {
    state
        .sandboxes
        .iter()
        .map(|s| TrayRow {
            id: s.id.clone(),
            label: row_label(&s.name, s.state),
            connected: s.connected,
            action: match s.state {
                ViewState::Running => Some(TrayAction::Pause),
                ViewState::Paused | ViewState::Stopped => Some(TrayAction::Resume),
                _ => None,
            },
        })
        .collect()
}

/// What the menu shows now, so an unchanged menu is not rebuilt on every refresh.
#[derive(Default)]
pub struct TrayMemo(Mutex<Option<Vec<TrayRow>>>);

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn item(app: &AppHandle, id: String, text: &str) -> tauri::Result<MenuItem<tauri::Wry>> {
    MenuItem::with_id(app, id, text, true, None::<&str>)
}

fn build_menu(app: &AppHandle, rows: &[TrayRow]) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for r in rows {
        let sub = Submenu::with_id(app, format!("row:{}", r.id), &r.label, true)?;
        match r.action {
            Some(TrayAction::Pause) => {
                sub.append(&item(app, format!("pause:{}", r.id), "Pause")?)?
            }
            Some(TrayAction::Resume) => {
                sub.append(&item(app, format!("resume:{}", r.id), "Resume")?)?
            }
            None => {}
        }
        sub.append(&item(
            app,
            format!("open:{}", r.id),
            open_label(r.connected),
        )?)?;
        menu.append(&sub)?;
    }
    if !rows.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    menu.append(&item(app, "show".into(), "Open Instabox Bridge")?)?;
    menu.append(&item(app, "quit".into(), "Quit")?)?;
    Ok(menu)
}

pub fn build(app: &AppHandle, core: Arc<Core>) -> tauri::Result<()> {
    let menu = build_menu(app, &[])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Instabox Bridge")
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref().to_string();
            match id.as_str() {
                "show" => show_main(app),
                "quit" => app.exit(0),
                other => {
                    let Some((verb, sandbox)) = other.split_once(':') else {
                        return;
                    };
                    let (verb, sandbox, core) =
                        (verb.to_string(), sandbox.to_string(), core.clone());
                    tauri::async_runtime::spawn(async move {
                        // A failure shows in the app's error line.
                        let _ = match verb.as_str() {
                            "pause" => core.pause(&sandbox).await,
                            "resume" => core.resume(&sandbox).await,
                            "open" => core.open_in_t3(&sandbox).await,
                            _ => Ok(()),
                        };
                    });
                }
            }
        });
    // Template image: macOS tints it for light and dark menu bars.
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    builder = builder.icon(icon).icon_as_template(true);
    builder.build(app)?;
    Ok(())
}

/// Rebuilds the menu when the sandbox rows changed.
pub fn update(app: &AppHandle, state: &AppState) {
    let rows = tray_rows(state);
    {
        let memo = app.state::<TrayMemo>();
        let mut last = memo.0.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&rows) {
            return;
        }
        *last = Some(rows.clone());
    }
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    if let Ok(menu) = build_menu(app, &rows) {
        let _ = tray.set_menu(Some(menu));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::view::{AutoSleep, SandboxView};

    fn row(id: &str, state: ViewState) -> SandboxView {
        SandboxView {
            id: id.into(),
            name: format!("box {id}"),
            state,
            size: "4 vCPU / 8 GB".into(),
            connected: true,
            t3_version: None,
            free: false,
            auto_sleep: AutoSleep {
                seconds: None,
                label: "Default".into(),
            },
            run_left_minutes: None,
            cooldown_minutes: None,
            update_pending: false,
            desktop_behind: false,
        }
    }

    #[test]
    fn one_row_per_sandbox_with_pause_or_resume() {
        let state = AppState {
            sandboxes: vec![
                row("a", ViewState::Running),
                row("b", ViewState::Paused),
                row("c", ViewState::Waking),
            ],
            ..AppState::default()
        };
        let rows = tray_rows(&state);
        assert_eq!(rows[0].label, "\u{25CF} box a");
        assert_eq!(rows[1].label, "\u{25CB} box b");
        assert_eq!(rows[2].label, "\u{25CF} box c");
        assert_eq!(rows[0].action, Some(TrayAction::Pause));
        assert_eq!(rows[1].action, Some(TrayAction::Resume));
        assert_eq!(rows[2].action, None);
    }

    #[test]
    fn open_item_follows_connected() {
        assert_eq!(open_label(true), "Open in T3 Code");
        assert_eq!(open_label(false), "Connect to T3 Code");
        let mut r = row("a", ViewState::Running);
        r.connected = false;
        let state = AppState {
            sandboxes: vec![r],
            ..AppState::default()
        };
        assert!(!tray_rows(&state)[0].connected);
        assert_eq!(row_label("x", ViewState::Stopped), "\u{25CB} x");
    }
}
