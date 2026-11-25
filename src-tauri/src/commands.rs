//! The commands the UI calls. Each one parses its arguments and hands off to `Core`.
//! Errors are `ErrorView` values the UI can branch on.
//!
//! An optional `ade` argument picks the ADE (`"t3"`, `"cmux"`, `"orca"`, `"paradev"`). Without
//! it a command uses the sandbox's own ADE (the first in its mark), else the default (T3);
//! `create_sandbox` without it creates a plain sandbox, with no ADE.

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::ade::Ade;
use crate::agents::Agent;
use crate::app::{AppState, Core, CreateArgs, Notice, parse_mcp_client, parse_provider};
use crate::errors::ErrorView;
use crate::login;

type Shared<'a> = State<'a, Arc<Core>>;
type Res<T> = Result<T, ErrorView>;

#[tauri::command]
pub fn get_state(core: Shared<'_>) -> AppState {
    core.state()
}

#[tauri::command]
pub async fn sign_in(core: Shared<'_>, provider: String) -> Res<()> {
    let Some(p) = parse_provider(&provider) else {
        return core.settle(Err(ErrorView::other("Unknown sign-in provider")));
    };
    core.inner().clone().sign_in(p).await
}

#[tauri::command]
pub async fn sign_out(core: Shared<'_>) -> Res<()> {
    core.sign_out().await
}

#[tauri::command]
pub fn switch_org(core: Shared<'_>, org_id: String) -> Res<()> {
    core.switch_org(&org_id)
}

#[tauri::command]
#[allow(non_snake_case)]
pub async fn create_sandbox(
    core: Shared<'_>,
    name: String,
    snapshot_id: Option<String>,
    vcpus: u32,
    memMiB: u32,
    auto_suspend_after_seconds: Option<i64>,
    ade: Option<Ade>,
) -> Res<Notice> {
    core.inner()
        .clone()
        .create_sandbox(CreateArgs {
            name,
            snapshot_id,
            vcpus,
            mem_mib: memMiB,
            auto_suspend_after_seconds,
            ade,
        })
        .await
}

#[tauri::command]
pub async fn connect_sandbox(core: Shared<'_>, id: String, ade: Option<Ade>) -> Res<Notice> {
    core.inner().clone().connect_sandbox(&id, ade).await
}

/// Closes a failed create or connect.
#[tauri::command]
pub fn dismiss_progress(core: Shared<'_>) {
    core.dismiss_progress();
}

/// Looks for the ADEs' desktop apps (T3 Code, cmux, Paradev) on this computer again, now.
#[tauri::command]
pub async fn refresh_t3(core: Shared<'_>) -> Res<()> {
    core.inner().clone().refresh_ades().await
}

/// Opens the sandbox in the ADE's app: T3 Code (a connected sandbox is woken, synced and
/// paired again), cmux (its link, with an SSH token), Orca (its SSH host, then the app; the
/// answer says what to do next in Orca) or Paradev (the sandbox is woken, then its host
/// opens). A sandbox without the ADE gets it first.
#[tauri::command]
pub async fn open_in_t3(core: Shared<'_>, id: String, ade: Option<Ade>) -> Res<Notice> {
    core.inner().clone().open_in_ade(&id, ade).await
}

/// A new pairing link for a connected sandbox, opened in the ADE's app.
#[tauri::command]
pub async fn pair_again(core: Shared<'_>, id: String, ade: Option<Ade>) -> Res<()> {
    core.inner().clone().pair_again(&id, ade).await
}

#[tauri::command]
pub async fn pause(core: Shared<'_>, id: String) -> Res<()> {
    core.inner().clone().pause(&id).await
}

#[tauri::command]
pub async fn resume(core: Shared<'_>, id: String) -> Res<()> {
    core.inner().clone().resume(&id).await
}

#[tauri::command]
pub async fn delete(core: Shared<'_>, id: String) -> Res<()> {
    core.inner().clone().delete(&id).await
}

#[tauri::command]
#[allow(non_snake_case)]
pub async fn resize(core: Shared<'_>, id: String, vcpus: u32, memMiB: u32) -> Res<()> {
    core.inner().clone().resize(&id, vcpus, memMiB).await
}

#[tauri::command]
pub async fn set_auto_sleep(core: Shared<'_>, id: String, seconds: i64) -> Res<()> {
    core.inner().clone().set_auto_sleep(&id, seconds).await
}

/// Sets the ADE (T3) up again on the sandbox. cmux installs nothing there to set up again.
#[tauri::command]
pub async fn reinstall_t3(core: Shared<'_>, id: String, ade: Option<Ade>) -> Res<()> {
    core.inner().clone().reinstall(&id, ade).await
}

#[tauri::command]
pub async fn agent_sign_in(core: Shared<'_>, id: String, agent: Agent) -> Res<()> {
    core.inner().clone().agent_sign_in(&id, agent).await
}

/// Whether the agent is signed in on the sandbox. False without any exec when the
/// sandbox is not running.
#[tauri::command]
pub async fn agent_status(core: Shared<'_>, id: String, agent: Agent) -> Res<bool> {
    core.agent_status(&id, agent).await
}

#[tauri::command]
pub async fn ssh_command(core: Shared<'_>, id: String) -> Res<String> {
    core.ssh_command(&id).await
}

#[tauri::command]
pub async fn open_ssh(core: Shared<'_>, id: String) -> Res<()> {
    core.open_ssh(&id).await
}

#[tauri::command]
pub async fn add_mcp(core: Shared<'_>, client: String) -> Res<()> {
    let Some(c) = parse_mcp_client(&client) else {
        return core.settle(Err(ErrorView::other("Unknown client")));
    };
    core.add_mcp(c).await
}

#[tauri::command]
pub fn set_start_at_login(app: AppHandle, core: Shared<'_>, on: bool) -> Res<()> {
    let r = if on {
        login::enable(&app)
    } else {
        login::disable(&app)
    };
    if let Err(e) = r {
        return core.settle(Err(ErrorView::other(e)));
    }
    #[cfg(windows)]
    if on {
        login::quote_windows_run_entry(&app.package_info().name);
    }
    // macOS 13 and later: registered, but the person turned the app off in System Settings
    // before. Only they can allow it again there.
    if on && login::needs_approval(&app) {
        login::open_login_items();
        core.set_start_at_login(false);
        return core.settle(Err(ErrorView::other(format!(
            "Allow {} in System Settings, under Login Items.",
            app.package_info().name
        ))));
    }
    core.set_start_at_login(on);
    core.settle(Ok(()))
}

#[tauri::command]
pub fn open_url(core: Shared<'_>, url: String) -> Res<()> {
    core.open_url(&url)
}
