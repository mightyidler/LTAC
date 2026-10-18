use serde::Serialize;
use std::{fs, path::PathBuf, time::Duration};
use tauri::{Manager, State, ipc::Channel};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;

#[derive(Default)]
pub struct UpdateLock(Mutex<()>);

#[derive(Serialize)]
pub struct UpdateStatus {
    version: String,
    completed: bool,
}

#[derive(Serialize)]
pub struct UpdateResult {
    status: &'static str,
    version: String,
}

fn pending_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("update-pending-version"))
}

#[tauri::command]
pub fn update_status(app: tauri::AppHandle) -> Result<UpdateStatus, String> {
    let version = app.package_info().version.to_string();
    let pending = pending_path(&app)?;
    let completed = fs::read_to_string(&pending).is_ok_and(|target| target.trim() == version);
    if completed {
        let _ = fs::remove_file(pending);
    }
    Ok(UpdateStatus { version, completed })
}

#[tauri::command]
pub async fn check_for_updates_manual(
    app: tauri::AppHandle,
    state: State<'_, UpdateLock>,
    on_progress: Channel<&'static str>,
) -> Result<UpdateResult, String> {
    let _guard = state
        .0
        .try_lock()
        .map_err(|_| "업데이트가 이미 진행 중이에요.".to_owned())?;
    let updater = app
        .updater_builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let Some(update) = updater
        .check()
        .await
        .map_err(|_| "업데이트를 확인하지 못했어요. 다시 시도해주세요.".to_owned())?
    else {
        return Ok(UpdateResult {
            status: "latest",
            version: app.package_info().version.to_string(),
        });
    };
    let _ = on_progress.send("installing");
    // download verifies the Tauri signature before any installer is launched.
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|_| "업데이트를 내려받지 못했어요. 다시 시도해주세요.".to_owned())?;
    let pending = pending_path(&app)?;
    fs::create_dir_all(pending.parent().expect("Application data directory"))
        .map_err(|e| e.to_string())?;
    fs::write(&pending, &update.version).map_err(|e| e.to_string())?;
    // Windows starts the installer and exits here. Show completion only after
    // the newly installed version starts and matches the pending version.
    if update.install(bytes).is_err() {
        let _ = fs::remove_file(pending);
        return Err("업데이트를 설치하지 못했어요. 다시 시도해주세요.".into());
    }
    Ok(UpdateResult {
        status: "installed",
        version: update.version,
    })
}
