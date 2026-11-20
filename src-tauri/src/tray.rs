//! The menu-bar tray. A left click opens the popover window under the icon; a right click
//! shows a menu with one item per sandbox, then Open and Quit.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, LogicalPosition, Manager, Rect, WebviewWindow, Window};

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

/// When the popover last hid because it lost focus.
#[derive(Default)]
pub struct HiddenAt(Mutex<Option<Instant>>);

/// A rectangle in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Space between the popover and the icon, and between the popover and the screen edge.
const GAP: f64 = 4.0;
const EDGE: f64 = 8.0;

/// The popover's top-left corner: centered on the icon, kept on the icon's screen. It opens
/// under an icon in the top half of the screen (the macOS menu bar) and above one in the
/// bottom half (a Windows taskbar).
pub fn popover_origin(icon: Bounds, window: (f64, f64), screen: Bounds) -> (f64, f64) {
    let min = screen.x + EDGE;
    let max = screen.x + screen.w - window.0 - EDGE;
    let x = (icon.x + icon.w / 2.0 - window.0 / 2.0)
        .max(min)
        .min(max.max(min));
    let y = if icon.y + icon.h / 2.0 < screen.y + screen.h / 2.0 {
        icon.y + icon.h + GAP
    } else {
        (icon.y - window.1 - GAP).max(screen.y + EDGE)
    };
    (x, y)
}

/// A click on the icon also takes focus from an open popover, which hides it first. Without
/// this check the same click would open it again.
pub fn hid_just_now(last: Option<Instant>, now: Instant) -> bool {
    last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(300))
}

fn place(app: &AppHandle, window: &WebviewWindow, icon: Rect) -> tauri::Result<()> {
    let p = icon.position.to_physical::<f64>(1.0);
    let Some(screen) = app.monitor_from_point(p.x, p.y)?.or(app.primary_monitor()?) else {
        return Ok(());
    };
    let scale = screen.scale_factor();
    let pos = icon.position.to_logical::<f64>(scale);
    let size = icon.size.to_logical::<f64>(scale);
    if size.width == 0.0 {
        // The icon is not laid out yet; the window keeps its last place.
        return Ok(());
    }
    let spos = screen.position().to_logical::<f64>(scale);
    let ssize = screen.size().to_logical::<f64>(scale);
    let win = window
        .outer_size()?
        .to_logical::<f64>(window.scale_factor()?);
    let (x, y) = popover_origin(
        Bounds {
            x: pos.x,
            y: pos.y,
            w: size.width,
            h: size.height,
        },
        (win.width, win.height),
        Bounds {
            x: spos.x,
            y: spos.y,
            w: ssize.width,
            h: ssize.height,
        },
    );
    window.set_position(LogicalPosition::new(x, y))
}

/// Shows the popover under the icon. `icon` is the clicked icon's place; None looks it up.
pub fn show_popover(app: &AppHandle, icon: Option<Rect>) {
    let Some(w) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    let icon = icon.or_else(|| {
        app.tray_by_id(TRAY_ID)
            .and_then(|t| t.rect().ok().flatten())
    });
    if let Some(icon) = icon {
        let _ = place(app, &w, icon);
    }
    let _ = w.show();
    let _ = w.set_focus();
}

fn toggle_popover(app: &AppHandle, icon: Rect) {
    let Some(w) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
        return;
    }
    let last = *app
        .state::<HiddenAt>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !hid_just_now(last, Instant::now()) {
        show_popover(app, Some(icon));
    }
}

/// The popover closes when the user clicks anywhere else.
pub fn hide_on_blur(window: &Window) {
    if window.label() != MAIN_WINDOW {
        return;
    }
    let _ = window.hide();
    let app = window.app_handle();
    *app.state::<HiddenAt>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
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
    menu.append(&item(app, "show".into(), "Open Instabox")?)?;
    menu.append(&item(app, "quit".into(), "Quit")?)?;
    Ok(menu)
}

pub fn build(app: &AppHandle, core: Arc<Core>) -> tauri::Result<()> {
    let menu = build_menu(app, &[])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Instabox")
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_popover(tray.app_handle(), rect);
            }
        })
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref().to_string();
            match id.as_str() {
                "show" => show_popover(app, None),
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
    #[cfg(target_os = "macos")]
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    // Elsewhere nothing tints it, and the black template would vanish on a dark taskbar.
    #[cfg(not(target_os = "macos"))]
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;
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

    const SCREEN: Bounds = Bounds {
        x: 0.0,
        y: 0.0,
        w: 1440.0,
        h: 900.0,
    };

    #[test]
    fn popover_is_centered_under_the_icon() {
        let icon = Bounds {
            x: 700.0,
            y: 0.0,
            w: 24.0,
            h: 24.0,
        };
        assert_eq!(popover_origin(icon, (360.0, 600.0), SCREEN), (532.0, 28.0));
    }

    #[test]
    fn popover_stays_on_the_icon_screen() {
        let right = Bounds {
            x: 1420.0,
            y: 0.0,
            w: 24.0,
            h: 24.0,
        };
        assert_eq!(popover_origin(right, (360.0, 600.0), SCREEN).0, 1072.0);
        let left = Bounds {
            x: 2.0,
            y: 0.0,
            w: 24.0,
            h: 24.0,
        };
        assert_eq!(popover_origin(left, (360.0, 600.0), SCREEN).0, 8.0);
        let second = Bounds {
            x: 1440.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        };
        let icon = Bounds {
            x: 3340.0,
            y: 0.0,
            w: 24.0,
            h: 24.0,
        };
        assert_eq!(popover_origin(icon, (360.0, 600.0), second).0, 2992.0);
    }

    #[test]
    fn popover_opens_above_an_icon_in_a_bottom_taskbar() {
        let screen = Bounds {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        };
        let icon = Bounds {
            x: 1700.0,
            y: 1040.0,
            w: 24.0,
            h: 40.0,
        };
        assert_eq!(
            popover_origin(icon, (360.0, 600.0), screen),
            (1532.0, 436.0)
        );
        let corner = Bounds { x: 1900.0, ..icon };
        assert_eq!(popover_origin(corner, (360.0, 600.0), screen).0, 1552.0);
        // A second screen below the first: the icon's own screen decides.
        let lower = Bounds {
            y: 1080.0,
            ..screen
        };
        let icon = Bounds { y: 2120.0, ..icon };
        assert_eq!(popover_origin(icon, (360.0, 600.0), lower).1, 1516.0);
    }

    #[test]
    fn a_short_screen_keeps_the_popover_top_on_screen() {
        let screen = Bounds {
            x: 0.0,
            y: 0.0,
            w: 1093.0,
            h: 614.0,
        };
        let icon = Bounds {
            x: 900.0,
            y: 578.0,
            w: 24.0,
            h: 36.0,
        };
        assert_eq!(popover_origin(icon, (360.0, 600.0), screen).1, 8.0);
    }

    #[test]
    fn a_click_right_after_a_blur_does_not_reopen() {
        let now = Instant::now();
        assert!(!hid_just_now(None, now));
        assert!(hid_just_now(Some(now), now + Duration::from_millis(100)));
        assert!(!hid_just_now(Some(now), now + Duration::from_millis(400)));
    }
}
