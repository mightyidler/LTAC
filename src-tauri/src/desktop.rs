use anyhow::{Context, Result};
use ltac::{
    config::{Config, default_path},
    connection, controller,
    discovery::{self, Device},
    journal::Journal,
    ssap::PowerState,
    sync::{Event, Initial, Notice},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{
    Emitter, Manager, State,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::ManagerExt;
use tokio::sync::Mutex;

#[path = "ui_commands.rs"]
mod ui;

#[derive(Clone, Serialize)]
pub struct Snapshot {
    configured: bool,
    name: String,
    stage: String,
    choices: Vec<Choice>,
    manual_allowed: bool,
    wake_available: bool,
    wake_verified: bool,
    power: String,
    automatic: bool,
    autostart: bool,
    service_mode: bool,
    busy: bool,
    checked_at: Option<u64>,
    error: Option<String>,
    devices: Vec<ui::SavedDevice>,
    shortcut_enabled: bool,
    shortcut: Option<String>,
}
#[derive(Clone, Serialize)]
struct Choice {
    id: String,
    name: String,
    model: String,
}
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    automatic: bool,
    shortcut_enabled: bool,
    shortcut: Option<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            automatic: true,
            shortcut_enabled: false,
            shortcut: None,
        }
    }
}

pub struct Desktop {
    path: PathBuf,
    journal: Journal,
    view: Arc<RwLock<Snapshot>>,
    operation: Mutex<()>,
    watcher: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    events: RwLock<EventRoute>,
    _source: Option<ltac::windows_events::Source>,
    service_mode: bool,
    candidates: Mutex<Vec<Device>>,
    pair_cancel: tokio::sync::Notify,
    discovery_cancel: tokio::sync::Notify,
    hotkey: ui::Hotkey,
    _lock: File,
}

#[derive(Default)]
struct EventRoute {
    watcher: Option<tokio::sync::mpsc::UnboundedSender<Notice>>,
    display: Option<Event>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn update(view: &RwLock<Snapshot>, app: &tauri::AppHandle, edit: impl FnOnce(&mut Snapshot)) {
    if let Ok(mut snapshot) = view.write() {
        edit(&mut snapshot);
        let _ = app.emit("ltac-state", snapshot.clone());
    }
}
fn read(state: &Desktop) -> std::result::Result<Snapshot, String> {
    state
        .view
        .read()
        .map(|s| s.clone())
        .map_err(|_| "상태를 읽을 수 없습니다.".into())
}
fn fail(state: &Desktop, app: &tauri::AppHandle, error: &str) {
    let _ = state
        .journal
        .clone()
        .record("desktop_error", json!({"error":error}));
    update(&state.view, app, |s| {
        s.busy = false;
        s.error = Some(
            "요청을 완료하지 못했어요. TV 전원과 네트워크를 확인하고 다시 시도해 주세요.".into(),
        );
    });
}
fn apply_config(s: &mut Snapshot, config: &Config) {
    s.configured = config.protected_key.is_some();
    s.name = config
        .device
        .as_ref()
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "LG webOS TV".into());
    s.wake_available = config.mac.is_some();
    s.wake_verified = config.wake_verified;
    s.stage = if s.configured { "ready" } else { "choose" }.into();
    if config.device.is_none() {
        s.name = config.ip.to_string();
    }
}
fn preferences_path(config: &Path) -> PathBuf {
    config.with_file_name("desktop.json")
}
fn save_preferences(config: &Path, automatic: bool) -> Result<()> {
    let path = preferences_path(config);
    let mut preferences: Preferences = fs::read(&path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default();
    preferences.automatic = automatic;
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("No settings directory")?)?;
    file.write_all(&serde_json::to_vec_pretty(&preferences)?)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn observe(view: &RwLock<Snapshot>, app: &tauri::AppHandle, event: &str, data: &Value) {
    match event {
        "connection_saved" => update(view, app, |s| {
            s.power = data["state"]["kind"].as_str().unwrap_or("unknown").into();
            s.checked_at = Some(now());
        }),
        "reconnect" if data["state"]["kind"] == "on" => update(view, app, |s| {
            s.power = "on".into();
            s.checked_at = Some(now());
            s.error = None;
        }),
        "off_evidence" => update(view, app, |s| {
            s.power = if data["response"] == "already_reported_standby" {
                "standby"
            } else {
                "off_requested"
            }
            .into();
            s.checked_at = Some(now());
            s.error = None;
        }),
        "sync_action_started" => update(view, app, |s| {
            s.busy = true;
            s.error = None;
            s.power = if data["target"] == "on" {
                "waking"
            } else {
                "turning_off"
            }
            .into();
        }),
        "sync_action_finished" => update(view, app, |s| {
            s.busy = false;
            if data["success"] == false {
                s.power = "unknown".into();
                s.error = Some(
                    if data["target"] == "on" {
                        "TV를 자동으로 켤 수 없어요. TV의 네트워크 켜기 설정을 확인해 주세요."
                    } else {
                        "TV를 끄지 못했어요. 연결 상태를 확인해 주세요."
                    }
                    .into(),
                );
            }
        }),
        "idle_connection_closed" => update(view, app, |s| {
            s.power = "unknown".into();
        }),
        _ => {}
    }
}

async fn stop_watcher(state: &Desktop) {
    if let Ok(mut route) = state.events.write() {
        route.watcher = None;
    }
    if let Some(task) = state.watcher.lock().await.take() {
        task.abort();
        let _ = task.await;
    }
}

async fn pause_devices(state: &Desktop) -> Result<()> {
    if state.service_mode {
        ltac::service_ipc::request(ltac::service_ipc::Request::Automatic { enabled: false })
            .await?;
    } else {
        stop_watcher(state).await;
    }
    Ok(())
}

async fn configure_service(state: &Desktop, enabled: bool) -> Result<()> {
    let devices = ui::configs(&state.path)?
        .iter()
        .map(Config::for_service)
        .collect::<Result<Vec<_>>>()?;
    ltac::service_ipc::request(ltac::service_ipc::Request::Configure { devices }).await?;
    ltac::service_ipc::request(ltac::service_ipc::Request::Automatic { enabled }).await?;
    Ok(())
}
async fn start_watcher(
    state: &Desktop,
    app: &tauri::AppHandle,
    initial: Option<Initial>,
) -> Result<()> {
    anyhow::ensure!(
        !state.service_mode,
        "Windows service owns automatic synchronization"
    );
    anyhow::ensure!(!ltac::lifecycle::is_ending(), "Windows session is ending");
    let configs = ui::configs(&state.path)?;
    anyhow::ensure!(!configs.is_empty(), "연결된 기기가 없어요.");
    for config in &configs {
        config.key()?;
        anyhow::ensure!(
            config.mac.is_some(),
            "기기 정보에서 MAC 주소를 확인해주세요."
        );
    }
    let journal = state.journal.clone();
    let view = state.view.clone();
    let handle = app.clone();
    let path = state.path.clone();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    {
        let mut route = state
            .events
            .write()
            .map_err(|_| anyhow::anyhow!("Event route poisoned"))?;
        route.watcher = Some(sender.clone());
        if let Some(event) = route.display {
            let _ = sender.send(Notice {
                event,
                suspend_ack: None,
            });
        }
    }
    let task = tauri::async_runtime::spawn(async move {
        let mut receiver = receiver;
        let mut tasks = tokio::task::JoinSet::new();
        let mut senders = Vec::new();
        let mut initial = initial;
        for config in configs {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            senders.push(sender);
            let mut journal = journal.clone();
            let initial = initial.take();
            tasks.spawn(async move {
                ltac::sync::run_with_initial(&config, &mut journal, receiver, false, initial).await
            });
        }
        let result: Result<()> = async {
            loop {
                tokio::select! {
                    notice = receiver.recv() => {
                        let Some(notice): Option<Notice> = notice else { return Ok(()); };
                        let mut acknowledgements = Vec::new();
                        for sender in &senders {
                            let suspend_ack = notice.suspend_ack.as_ref().map(|_| {
                                let (ack, receiver) = std::sync::mpsc::sync_channel(1);
                                acknowledgements.push(receiver); ack
                            });
                            let _ = sender.send(Notice { event: notice.event, suspend_ack });
                        }
                        if let Some(ack) = notice.suspend_ack {
                            tokio::task::spawn_blocking(move || {
                                let deadline = std::time::Instant::now() + Duration::from_millis(1400);
                                for receiver in acknowledgements { let _ = receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())); }
                                let _ = ack.try_send(());
                            });
                        }
                    }
                    result = tasks.join_next() => {
                        let Some(result) = result else { return Ok(()); };
                        result??;
                        anyhow::bail!("기기 동기화가 종료되었어요.");
                    }
                }
            }
        }.await;
        if let Err(error) = result {
            let mut journal = journal;
            let _ = journal.record("desktop_sync_failed", json!({"error":format!("{error:#}")}));
            let _ = save_preferences(&path, false);
            update(&view, &handle, |s| {
                s.automatic = false;
                s.busy = false;
                s.error = Some("자동 동기화를 시작하지 못했어요. TV 연결을 확인해 주세요.".into());
            });
        }
    });
    *state.watcher.lock().await = Some(task);
    Ok(())
}

#[tauri::command]
fn snapshot(state: State<'_, Desktop>) -> std::result::Result<Snapshot, String> {
    read(&state)
}

async fn do_refresh(
    state: &Desktop,
    app: &tauri::AppHandle,
) -> std::result::Result<Snapshot, String> {
    if state.service_mode {
        return service_request(state, app, ltac::service_ipc::Request::Refresh).await;
    }
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.".to_owned())?;
    if read(state)?.busy {
        return read(state);
    }
    let config = Config::load(&state.path).map_err(|e| format!("{e:#}"))?;
    update(&state.view, app, |s| {
        s.busy = true;
        s.error = None;
    });
    let result = controller::status(&config).await;
    match result {
        Ok((endpoint, power)) => {
            let _ = state.journal.clone().record(
                "desktop_status",
                json!({"endpoint":endpoint, "state":power}),
            );
            update(&state.view, app, |s| {
                if let Ok(current) = Config::load(&state.path) {
                    apply_config(s, &current);
                }
                s.power = match power {
                    PowerState::On => "on",
                    PowerState::Standby => "standby",
                    PowerState::ScreenOff => "screen_off",
                    PowerState::Unknown(_) => "unknown",
                }
                .into();
                s.checked_at = Some(now());
                s.busy = false;
            });
        }
        Err(error) => {
            let _ = state.journal.clone().record(
                "desktop_status_failed",
                json!({"error":format!("{error:#}")}),
            );
            update(&state.view, app, |s| {
                s.busy = false;
                s.power = "unknown".into();
                s.error = Some(
                    "TV에 연결할 수 없어요. TV 전원과 네트워크를 확인하고 다시 시도해 주세요."
                        .into(),
                );
            });
        }
    }
    read(state)
}
#[tauri::command]
async fn refresh_status(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
) -> std::result::Result<Snapshot, String> {
    do_refresh(&state, &app).await
}

async fn do_automatic(
    state: &Desktop,
    app: &tauri::AppHandle,
    enabled: bool,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.".to_owned())?;
    if state.service_mode {
        let previous = read(state)?.automatic;
        let result = service_request(
            state,
            app,
            ltac::service_ipc::Request::Automatic { enabled },
        )
        .await?;
        if let Err(error) = save_preferences(&state.path, enabled) {
            let _ = service_request(
                state,
                app,
                ltac::service_ipc::Request::Automatic { enabled: previous },
            )
            .await;
            return Err(error.to_string());
        }
        return Ok(result);
    }
    stop_watcher(state).await;
    if enabled && let Err(error) = start_watcher(state, app, None).await {
        fail(state, app, &format!("자동 동기화: {error:#}"));
        update(&state.view, app, |s| s.automatic = false);
        let _ = save_preferences(&state.path, false);
        return read(state);
    }
    if let Err(error) = save_preferences(&state.path, enabled) {
        stop_watcher(state).await;
        update(&state.view, app, |s| s.automatic = false);
        fail(state, app, &format!("자동 동기화 설정 저장: {error:#}"));
    } else {
        update(&state.view, app, |s| {
            s.automatic = enabled;
            s.busy = false;
            s.error = None;
        });
    }
    read(state)
}
#[tauri::command]
async fn set_automatic(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
    enabled: bool,
) -> std::result::Result<Snapshot, String> {
    do_automatic(&state, &app, enabled).await
}

async fn do_discover(
    state: &Desktop,
    app: &tauri::AppHandle,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.".to_owned())?;
    let cancelled = state.discovery_cancel.notified();
    tokio::pin!(cancelled);
    cancelled.as_mut().enable();
    update(&state.view, app, |s| {
        s.busy = true;
        s.stage = "searching".into();
        s.error = None;
        s.choices.clear();
        s.manual_allowed = false;
    });
    state.candidates.lock().await.clear();
    let result = tokio::select! {
        result = discovery::discover() => result,
        _ = &mut cancelled => return read(state),
    };
    match result {
        Ok(devices) => {
            let _ = state
                .journal
                .clone()
                .record("desktop_discovery_finished", json!({"devices":devices}));
            let choices = devices
                .iter()
                .map(|d| Choice {
                    id: d.identity.id.clone(),
                    name: d.identity.name.clone(),
                    model: d.identity.model.clone(),
                })
                .collect::<Vec<_>>();
            let empty = choices.is_empty();
            *state.candidates.lock().await = devices;
            update(&state.view, app, |s| {
                s.choices = choices;
                s.busy = false;
                s.stage = if empty { "not_found" } else { "choose" }.into();
                s.manual_allowed = empty;
            });
        }
        Err(error) => {
            fail(state, app, &format!("discovery: {error:#}"));
            update(&state.view, app, |s| {
                s.stage = "not_found".into();
                s.manual_allowed = true;
                s.error = None;
            });
        }
    }
    read(state)
}
#[tauri::command]
async fn discover_tvs(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
) -> std::result::Result<Snapshot, String> {
    do_discover(&state, &app).await
}

#[tauri::command]
async fn cancel_connection(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
) -> std::result::Result<Snapshot, String> {
    if read(&state)?.stage == "pairing" {
        state.pair_cancel.notify_waiters();
    }
    if read(&state)?.stage == "searching" {
        state.discovery_cancel.notify_waiters();
    }
    let _operation = state.operation.lock().await;
    if let Ok(config) = Config::load(&state.path) {
        update(&state.view, &app, |s| {
            apply_config(s, &config);
            s.busy = false;
            s.error = None;
        });
    } else {
        update(&state.view, &app, |s| {
            s.stage = "choose".into();
            s.busy = false;
            s.error = None;
        });
    }
    read(&state)
}

#[tauri::command]
async fn pair_tv(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
    device_id: Option<String>,
    ip: Option<String>,
    reset: Option<bool>,
    mac: Option<String>,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.".to_owned())?;
    let device = if let Some(id) = device_id {
        Some(
            state
                .candidates
                .lock()
                .await
                .iter()
                .find(|d| d.identity.id == id)
                .cloned()
                .ok_or("다시 검색한 뒤 TV를 선택해 주세요.")?,
        )
    } else {
        None
    };
    if device.is_none() && ip.is_none() {
        return Err("검색된 TV를 선택해 주세요.".into());
    }
    let parsed_mac = mac
        .as_deref()
        .map(str::parse::<ltac::wol::MacAddress>)
        .transpose()?;
    let previous = read(&state)?;
    let automatic = if previous.configured {
        previous.automatic
    } else {
        fs::read(preferences_path(&state.path))
            .ok()
            .and_then(|data| serde_json::from_slice::<Preferences>(&data).ok())
            .unwrap_or_default()
            .automatic
    };
    let pairing_directory =
        tempfile::tempdir_in(state.path.parent().ok_or("설정 경로를 확인해주세요.")?)
            .map_err(|e| e.to_string())?;
    let pairing_path = pairing_directory.path().join("config.json");
    let saved_devices = ui::configs(&state.path).map_err(|e| e.to_string())?;
    if let Some(saved) = saved_devices.iter().find(|saved| {
        device
            .as_ref()
            .is_some_and(|d| saved.device.as_ref().is_some_and(|s| s.id == d.identity.id))
            || ip
                .as_ref()
                .is_some_and(|address| address == &saved.ip.to_string())
    }) {
        saved.save(&pairing_path).map_err(|e| e.to_string())?;
    }
    let cancelled = state.pair_cancel.notified();
    tokio::pin!(cancelled);
    cancelled.as_mut().enable();
    pause_devices(&state).await.map_err(|e| e.to_string())?;
    update(&state.view, &app, |s| {
        s.busy = true;
        s.power = "unknown".into();
        s.stage = "pairing".into();
        s.error = None;
    });
    let mut journal = state.journal.clone();
    let pairing = async {
        if let Some(device) = device {
            connection::pair_device(&device, &pairing_path, reset.unwrap_or(false), &mut journal)
                .await
        } else {
            connection::pair_manual(
                ip.context("Missing fallback address")?.parse()?,
                &pairing_path,
                &mut journal,
            )
            .await
        }
    };
    let result = tokio::select! {
        result = pairing => result,
        _ = &mut cancelled => {
            update(&state.view, &app, |s| { s.busy = false; s.stage = if s.configured { "ready" } else { "choose" }.into(); });
            resume_automatic(&state, &app, automatic).await;
            return read(&state);
        }
    };
    match result {
        Ok(mut config) => {
            let saved: Result<()> = (|| {
                if let Some(mac) = parsed_mac {
                    config.mac = Some(mac);
                }
                ui::remember(&state.path, &config)?;
                config.source_path = Some(state.path.clone());
                config.save(&state.path)?;
                ui::refresh_devices(&state, &app)?;
                Ok(())
            })();
            if let Err(error) = saved {
                fail(&state, &app, &format!("save paired device: {error:#}"));
                update(&state.view, &app, |s| s.stage = "pair_error".into());
                resume_automatic(&state, &app, automatic).await;
                return read(&state);
            }
            update(&state.view, &app, |s| {
                apply_config(s, &config);
                s.busy = false;
            });
        }
        Err(error) => {
            fail(&state, &app, &format!("pair: {error:#}"));
            update(&state.view, &app, |s| {
                s.stage = "pair_error".into();
                s.error = None;
            });
        }
    }
    resume_automatic(&state, &app, automatic).await;
    read(&state)
}

async fn resume_automatic(state: &Desktop, app: &tauri::AppHandle, enabled: bool) {
    if state.service_mode {
        let result = configure_service(state, enabled).await;
        if let Err(error) = result {
            let _ = ltac::service_ipc::request(ltac::service_ipc::Request::Automatic {
                enabled: false,
            })
            .await;
            let _ = save_preferences(&state.path, false);
            fail(state, app, &format!("서비스 설정 반영: {error:#}"));
            update(&state.view, app, |s| s.automatic = false);
        } else {
            let _ = save_preferences(&state.path, enabled);
            update(&state.view, app, |s| s.automatic = enabled);
        }
        return;
    }
    let configured = read(state).is_ok_and(|s| s.configured);
    if enabled
        && configured
        && let Err(error) = start_watcher(state, app, None).await
    {
        fail(state, app, &format!("자동 동기화 재개: {error:#}"));
        update(&state.view, app, |s| s.automatic = false);
        let _ = save_preferences(&state.path, false);
        return;
    }
    update(&state.view, app, |s| s.automatic = enabled);
    if let Err(error) = save_preferences(&state.path, enabled) {
        stop_watcher(state).await;
        update(&state.view, app, |s| s.automatic = false);
        fail(state, app, &format!("동기화 설정 저장: {error:#}"));
    }
}

fn show(app: &tauri::AppHandle) {
    if app.get_webview_window("main").is_none() {
        let result = app
            .config()
            .app
            .windows
            .first()
            .context("Missing main window configuration")
            .and_then(|config| Ok(tauri::WebviewWindowBuilder::from_config(app, config)?.build()?));
        if let Err(error) = result {
            if let Some(state) = app.try_state::<Desktop>() {
                fail(&state, app, &format!("창 생성: {error:#}"));
            }
            return;
        }
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn apply_service(state: &Desktop, app: &tauri::AppHandle, remote: ltac::service_ipc::Snapshot) {
    update(&state.view, app, |s| {
        s.power = remote.power;
        s.automatic = remote.automatic;
        s.busy = remote.busy;
        s.checked_at = remote.checked_at;
        s.error = remote.error.map(|_| {
            "전원 서비스가 요청을 완료하지 못했어요. TV와 네트워크를 확인해 주세요.".into()
        });
    });
}

async fn service_request(
    state: &Desktop,
    app: &tauri::AppHandle,
    request: ltac::service_ipc::Request,
) -> std::result::Result<Snapshot, String> {
    match ltac::service_ipc::request(request).await {
        Ok(remote) => apply_service(state, app, remote),
        Err(error) => {
            fail(state, app, &format!("전원 서비스: {error:#}"));
            update(&state.view, app, |s| {
                s.error = Some(
                    "전원 서비스에 요청하지 못했어요. 서비스 실행 상태를 확인해 주세요.".into(),
                )
            });
        }
    }
    read(state)
}

fn setup(app: &mut tauri::App) -> Result<()> {
    let started = std::time::Instant::now();
    let service_mode = ltac::service_ipc::installed()?;
    let path = default_path()?;
    let directory = path.parent().context("No user data directory")?;
    fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("command.lock"))?;
    lock.try_lock()
        .context("다른 LTAC가 실행 중입니다. CLI를 종료한 뒤 다시 열어 주세요.")?;
    let config = Config::load(&path).ok();
    if let Some(config) = &config
        && ui::configs(&path)?.is_empty()
    {
        ui::remember(&path, config)?;
    }
    let preferences: Preferences = fs::read(preferences_path(&path))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    app.autolaunch().enable()?;
    let configured = config.as_ref().is_some_and(|c| c.protected_key.is_some());
    let mut initial_view = Snapshot {
        configured,
        name: "LG webOS TV".into(),
        stage: "searching".into(),
        choices: Vec::new(),
        manual_allowed: false,
        wake_available: false,
        wake_verified: false,
        power: "unknown".into(),
        automatic: preferences.automatic && configured,
        autostart: app.autolaunch().is_enabled()?,
        service_mode,
        busy: false,
        checked_at: None,
        error: None,
        devices: ui::devices(&path)?,
        shortcut_enabled: preferences.shortcut_enabled,
        shortcut: preferences.shortcut.clone(),
    };
    if let Some(config) = &config {
        apply_config(&mut initial_view, config);
    }
    initial_view.automatic &= initial_view.wake_available;
    let view = Arc::new(RwLock::new(initial_view));
    let observer_view = view.clone();
    let observer_app = app.handle().clone();
    let mut journal = Journal::open(&directory.join("events.jsonl"))?
        .with_observer(move |event, data| observe(&observer_view, &observer_app, event, data));
    journal.record(
        "desktop_started",
        json!({"automatic":view.read().map(|s|s.automatic).unwrap_or(false)}),
    )?;
    let (native_sender, mut native_events) = tokio::sync::mpsc::unbounded_channel();
    // Register once for the entire desktop lifetime, including manual commands
    // and intervals where automatic synchronization is disabled.
    let source = if service_mode {
        None
    } else {
        Some(tauri::async_runtime::block_on(
            ltac::windows_events::Source::start(native_sender, true),
        )?)
    };
    let hotkey = ui::Hotkey::start(app.handle().clone());
    if preferences.shortcut_enabled
        && let Err(error) = hotkey.set(preferences.shortcut.clone())
    {
        update(&view, app.handle(), |s| {
            s.shortcut_enabled = false;
            s.error = Some(error);
        });
    }
    app.manage(Desktop {
        path,
        journal,
        view,
        operation: Mutex::new(()),
        watcher: Mutex::new(None),
        events: RwLock::new(EventRoute::default()),
        _source: source,
        service_mode,
        candidates: Mutex::new(Vec::new()),
        pair_cancel: tokio::sync::Notify::new(),
        discovery_cancel: tokio::sync::Notify::new(),
        hotkey,
        _lock: lock,
    });

    if service_mode {
        let handle = app.handle().clone();
        tauri::async_runtime::spawn(async move {
            let mut retry = 1u64;
            loop {
                let state = handle.state::<Desktop>();
                let result = ltac::service_ipc::subscribe(|remote| {
                    retry = 1;
                    apply_service(&state, &handle, remote);
                })
                .await;
                if let Err(error) = result {
                    let _ = state.journal.clone().record(
                        "service_connection_lost",
                        json!({"error":error.to_string()}),
                    );
                    update(&state.view, &handle, |s| {
                        s.busy = false;
                        s.power = "unknown".into();
                        s.error =
                            Some("전원 서비스 연결이 끊어졌어요. 다시 연결하고 있습니다.".into());
                    });
                }
                tokio::time::sleep(Duration::from_secs(retry)).await;
                retry = (retry * 2).min(30);
            }
        });
    }

    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        while let Some(notice) = native_events.recv().await {
            let state = handle.state::<Desktop>();
            let mut pending = Some(notice);
            if let Ok(mut route) = state.events.write() {
                let notice = pending.as_ref().expect("Pending native event");
                if matches!(
                    notice.event,
                    Event::DisplayOn | Event::DisplayOff | Event::DisplayDimmed
                ) {
                    route.display = Some(notice.event);
                }
                if let Some(sender) = &route.watcher {
                    pending = sender
                        .send(pending.take().expect("Pending native event"))
                        .err()
                        .map(|e| e.0);
                }
            }
            if let Some(notice) = pending {
                // Manual power commands temporarily own the TV connection.
                // A confirmed shutdown still cancels ON and attempts OFF.
                if notice.event == Event::SessionEnding && read(&state).is_ok_and(|s| s.automatic) {
                    let result = tokio::time::timeout(Duration::from_millis(1400), async {
                        let config = Config::load(&state.path)?;
                        controller::off(&config, &mut state.journal.clone()).await
                    })
                    .await;
                    let _ = state.journal.clone().record(
                        "shutdown_off_fallback",
                        json!({
                            "success": matches!(result, Ok(Ok(_))),
                            "physical_state": "not_observed"
                        }),
                    );
                }
                if let Some(ack) = notice.suspend_ack {
                    let _ = ack.try_send(());
                }
            }
        }
    });

    // Arm control before constructing tray menus or initializing WebView2.
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let state = handle.state::<Desktop>();
        let _operation = state.operation.lock().await;
        if !configured {
            // Discovery owns this lock itself. Do not gate first-run discovery
            // on service configuration or recursively acquire the same lock.
            drop(_operation);
            let _ = do_discover(&state, &handle).await;
            return;
        }
        if state.service_mode {
            resume_automatic(&state, &handle, preferences.automatic).await;
            return;
        }
        if read(&state).is_ok_and(|s| s.automatic)
            && let Err(error) = start_watcher(&state, &handle, None).await
        {
            let _ = save_preferences(&state.path, false);
            update(&state.view, &handle, |s| s.automatic = false);
            fail(&state, &handle, &format!("자동 동기화: {error:#}"));
        }
        let _ = state.journal.clone().record(
            "desktop_control_ready",
            json!({
                "elapsed_ms": started.elapsed().as_millis(),
                "webview_deferred": true
            }),
        );
    });

    let open = MenuItem::with_id(app, "open", "LTAC 열기", true, None::<&str>)?;
    let automatic = MenuItem::with_id(
        app,
        "automatic",
        "자동 동기화 켜기 / 끄기",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &automatic, &quit])?;
    TrayIconBuilder::with_id("ltac")
        .icon(
            app.default_window_icon()
                .context("Missing LTAC icon")?
                .clone(),
        )
        .tooltip("LTAC · LG TV Auto Control")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| {
            let handle = app.clone();
            let id = event.id.as_ref().to_owned();
            if id == "open" {
                show(app);
                return;
            }
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<Desktop>();
                match id.as_str() {
                    "automatic" => {
                        if let Ok(snapshot) = read(&state) {
                            let _ = do_automatic(&state, &handle, !snapshot.automatic).await;
                        }
                    }
                    "quit" => {
                        stop_watcher(&state).await;
                        let _ = state.journal.clone().record("desktop_stopped", json!({}));
                        handle.exit(0);
                    }
                    _ => {}
                }
            });
        })
        .build(app)?;
    if !std::env::args().any(|arg| arg == "--background") {
        show(app.handle());
    }
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let state = handle.state::<Desktop>();
        if state.service_mode || !configured {
            return;
        }
        // Automatic startup is already armed above; no redundant status lookup.
        if read(&state).is_ok_and(|s| s.automatic) {
            return;
        }
        if configured {
            let _ = do_refresh(&state, &handle).await;
        } else {
            let _ = do_discover(&state, &handle).await;
        }
    });
    Ok(())
}

pub fn run() -> Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(crate::updater::UpdateLock::default())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .setup(|app| setup(app).map_err(Into::into))
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            refresh_status,
            set_automatic,
            pair_tv,
            discover_tvs,
            cancel_connection,
            ui::edit_device,
            ui::remove_device,
            ui::set_shortcut,
            crate::updater::update_status,
            crate::updater::check_for_updates_manual
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
