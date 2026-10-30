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

#[path = "screen.rs"]
mod screen;
#[path = "screen_input.rs"]
mod screen_input;
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
    keep_tv_on: bool,
    autostart: bool,
    service_mode: bool,
    busy: bool,
    checked_at: Option<u64>,
    error: Option<String>,
    devices: Vec<ui::SavedDevice>,
    shortcut_enabled: bool,
    shortcut: Option<String>,
    screen_enabled: bool,
    screen_timeout_seconds: u32,
    screen_shortcut_enabled: bool,
    screen_shortcut: Option<String>,
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
    keep_tv_on: bool,
    shortcut_enabled: bool,
    shortcut: Option<String>,
    screen_enabled: bool,
    screen_timeout_seconds: u32,
    screen_shortcut_enabled: bool,
    screen_shortcut: Option<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            automatic: true,
            keep_tv_on: false,
            shortcut_enabled: false,
            shortcut: None,
            screen_enabled: false,
            screen_timeout_seconds: ltac::screen_protection::DEFAULT_SECONDS,
            screen_shortcut_enabled: false,
            screen_shortcut: None,
        }
    }
}
impl From<&Snapshot> for Preferences {
    fn from(s: &Snapshot) -> Self {
        Self {
            automatic: s.automatic,
            keep_tv_on: s.keep_tv_on,
            shortcut_enabled: s.shortcut_enabled,
            shortcut: s.shortcut.clone(),
            screen_enabled: s.screen_enabled,
            screen_timeout_seconds: s.screen_timeout_seconds,
            screen_shortcut_enabled: s.screen_shortcut_enabled,
            screen_shortcut: s.screen_shortcut.clone(),
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
    screen_hotkey: ui::Hotkey,
    screen_toggle: tokio::sync::mpsc::Sender<()>,
    screen_changed: tokio::sync::Notify,
    screen_epoch: std::sync::atomic::AtomicU64,
    screen_sequence: std::sync::atomic::AtomicU64,
    screen_source: u64,
    screen_allowed: std::sync::atomic::AtomicBool,
    screen_session: std::sync::Mutex<ltac::screen_protection::SessionPolicy>,
    shortcut_sleep: std::sync::atomic::AtomicBool,
    _lock: File,
}

#[derive(Default)]
struct EventRoute {
    watcher: Option<ltac::sync::NoticeSender>,
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
        .map(|d| d.display_name())
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
    write_preferences(config, &preferences)
}
fn write_preferences(config: &Path, preferences: &Preferences) -> Result<()> {
    let path = preferences_path(config);
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("No settings directory")?)?;
    file.write_all(&serde_json::to_vec_pretty(&preferences)?)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn initial_view(
    path: &Path,
    autostart: bool,
    service_mode: bool,
) -> Result<(Snapshot, Preferences)> {
    let legacy = Config::load(path).ok();
    let mut saved = ui::configs(path)?;
    if saved.is_empty()
        && let Some(config) = legacy.as_ref().filter(|c| c.protected_key.is_some())
    {
        ui::remember(path, config)?;
        saved = ui::configs(path)?;
    }
    // Persist the first launch even if the user closes before registering a TV.
    // Existing preferences also identify returning users of older releases.
    let first_run = !preferences_path(path).exists();
    let preferences: Preferences = fs::read(preferences_path(path))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if first_run {
        save_preferences(path, preferences.automatic)?;
    }
    let devices = ui::devices(path)?;
    let configured = !devices.is_empty();
    let mut initial = Snapshot {
        configured,
        name: "LG webOS TV".into(),
        stage: "searching".into(),
        choices: Vec::new(),
        manual_allowed: false,
        wake_available: false,
        wake_verified: false,
        power: "unknown".into(),
        automatic: preferences.automatic,
        keep_tv_on: preferences.keep_tv_on,
        autostart,
        service_mode,
        busy: false,
        checked_at: None,
        error: None,
        devices,
        shortcut_enabled: preferences.shortcut_enabled,
        shortcut: preferences.shortcut.clone(),
        screen_enabled: preferences.screen_enabled,
        screen_timeout_seconds: if ltac::screen_protection::valid_seconds(
            preferences.screen_timeout_seconds,
        ) {
            preferences.screen_timeout_seconds
        } else {
            ltac::screen_protection::DEFAULT_SECONDS
        },
        screen_shortcut_enabled: preferences.screen_shortcut_enabled,
        screen_shortcut: preferences.screen_shortcut.clone(),
    };
    if let Some(config) = saved.first().or(legacy.as_ref()) {
        apply_config(&mut initial, config);
    }
    initial.configured = configured;
    initial.stage = if configured || !first_run {
        "ready"
    } else {
        "searching"
    }
    .into();
    Ok((initial, preferences))
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
    state
        .screen_epoch
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
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
    ltac::service_ipc::request(ltac::service_ipc::Request::KeepTvOn {
        enabled: read(state).map_err(anyhow::Error::msg)?.keep_tv_on,
    })
    .await?;
    let devices = ui::configs(&state.path)?
        .iter()
        .map(Config::for_service)
        .collect::<Result<Vec<_>>>()?;
    ltac::service_ipc::request(ltac::service_ipc::Request::Configure { devices }).await?;
    ltac::service_ipc::request(ltac::service_ipc::Request::Automatic { enabled }).await?;
    Ok(())
}

fn next_initial(initial: &mut Option<Initial>) -> Option<Initial> {
    initial.as_mut().map(|initial| Initial {
        target: initial.target,
        session: initial.session.take(),
    })
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
    let keep_tv_on = read(state).map_err(anyhow::Error::msg)?.keep_tv_on;
    let power_automatic = read(state).map_err(anyhow::Error::msg)?.automatic;
    let configs = ui::configs(&state.path)?;
    anyhow::ensure!(!configs.is_empty(), "연결된 기기가 없어요.");
    for config in &configs {
        config.key()?;
        anyhow::ensure!(
            !power_automatic || config.mac.is_some(),
            "기기 정보에서 MAC 주소를 확인해주세요."
        );
    }
    let journal = state.journal.clone();
    let view = state.view.clone();
    let handle = app.clone();
    let path = state.path.clone();
    let (sender, receiver) = ltac::sync::notice_channel();
    {
        let mut route = state
            .events
            .write()
            .map_err(|_| anyhow::anyhow!("Event route poisoned"))?;
        route.watcher = Some(sender.clone());
        if let Some(event) = route.display {
            let _ = sender.send(Notice {
                preceding: None,
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
            let (sender, receiver) = ltac::sync::notice_channel();
            senders.push(sender);
            let mut journal = journal.clone();
            let initial = next_initial(&mut initial);
            tasks.spawn(async move {
                ltac::sync::run_with_screen_policy(
                    &config,
                    &mut journal,
                    receiver,
                    false,
                    initial,
                    keep_tv_on,
                    power_automatic,
                )
                .await
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
                            let _ = sender.send(Notice { preceding: notice.preceding, event: notice.event, suspend_ack });
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
    state.screen_changed.notify_one();
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
        let remote = ltac::service_ipc::request(ltac::service_ipc::Request::Automatic { enabled })
            .await
            .map_err(|e| e.to_string())?;
        apply_service(state, app, remote);
        if let Err(error) = save_preferences(&state.path, enabled) {
            let _ = service_request(
                state,
                app,
                ltac::service_ipc::Request::Automatic { enabled: previous },
            )
            .await;
            return Err(error.to_string());
        }
        update(&state.view, app, |s| s.automatic = enabled);
        return read(state);
    }
    stop_watcher(state).await;
    update(&state.view, app, |s| s.automatic = enabled);
    if (enabled || read(state)?.screen_enabled)
        && let Err(error) = start_watcher(state, app, None).await
    {
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
        Ok(mut devices) => {
            let saved = ui::configs(&state.path).map_err(|e| e.to_string())?;
            devices.retain(|device| !ui::already_saved(device, &saved));
            let _ = state
                .journal
                .clone()
                .record("desktop_discovery_finished", json!({"devices":devices}));
            let choices = devices
                .iter()
                .map(|d| Choice {
                    id: d.identity.id.clone(),
                    name: d.identity.display_name(),
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
    home: Option<bool>,
) -> std::result::Result<Snapshot, String> {
    if read(&state)?.stage == "pairing" {
        state.pair_cancel.notify_waiters();
    }
    if read(&state)?.stage == "searching" {
        state.discovery_cancel.notify_waiters();
    }
    let _operation = state.operation.lock().await;
    let config = Config::load(&state.path).ok();
    update(&state.view, &app, |s| {
        if let Some(config) = &config {
            apply_config(s, config);
        }
        s.configured = !s.devices.is_empty();
        s.stage = if s.configured || home.unwrap_or(false) {
            "ready"
        } else {
            "choose"
        }
        .into();
        s.busy = false;
        s.error = None;
    });
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
    if device
        .as_ref()
        .is_some_and(|device| ui::already_saved(device, &saved_devices))
        || ip.as_ref().is_some_and(|ip| {
            saved_devices
                .iter()
                .any(|saved| saved.ip.to_string() == *ip)
        })
    {
        return Err("이미 연결된 TV입니다. 기기 목록에서 확인해주세요.".into());
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
                // Without a simple one-TV mapping, leave the new TV for manual HDMI selection.
                if !saved_devices.is_empty() {
                    config.apply_hdmi_detection(None);
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
    update(&state.view, app, |s| s.automatic = enabled);
    if (enabled || read(state).is_ok_and(|s| s.screen_enabled))
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
    if let Some(window) = app.get_webview_window("main") {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = handle.state::<Desktop>();
            // Keep a live pairing visible rather than waiting on its operation
            // lock while the TV is asking for approval.
            if read(&state).is_ok_and(|s| s.stage != "pairing") {
                state.discovery_cancel.notify_waiters();
                let _operation = state.operation.lock().await;
                update(&state.view, &handle, |s| {
                    s.stage = "ready".into();
                    s.busy = false;
                });
                if let Ok(snapshot) = read(&state) {
                    let _ = handle.emit("ltac-opened", snapshot);
                }
            }
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        });
        return;
    }
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
    update(&state.view, app, |s| apply_service_snapshot(s, remote));
}

fn apply_service_snapshot(s: &mut Snapshot, remote: ltac::service_ipc::Snapshot) {
    s.power = remote.power;
    // The saved desktop preference owns input protection. Subscription frames
    // can precede startup configuration or arrive after a newer command.
    if s.configured {
        s.automatic = remote.automatic;
    }
    s.busy = remote.busy;
    s.checked_at = remote.checked_at;
    s.error = remote
        .error
        .map(|_| "전원 서비스가 요청을 완료하지 못했어요. TV와 네트워크를 확인해 주세요.".into());
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
    app.autolaunch().enable()?;
    let (initial_view, preferences) =
        initial_view(&path, app.autolaunch().is_enabled()?, service_mode)?;
    let configured = initial_view.configured;
    let discover_on_start = initial_view.stage == "searching";
    let view = Arc::new(RwLock::new(initial_view));
    let observer_view = view.clone();
    let observer_app = app.handle().clone();
    let mut journal = Journal::open(&directory.join("events.jsonl"))?
        .best_effort()
        .with_observer(move |event, data| observe(&observer_view, &observer_app, event, data));
    journal.record(
        "desktop_started",
        json!({"automatic":view.read().map(|s|s.automatic).unwrap_or(false)}),
    )?;
    let (native_sender, mut native_events) = ltac::sync::notice_channel();
    // Register once for the entire desktop lifetime, including manual commands
    // and intervals where automatic synchronization is disabled.
    // The desktop also needs interactive resume for a one-shot sleep shortcut
    // when the service's automatic policy is disabled.
    let source = Some(tauri::async_runtime::block_on(
        ltac::windows_events::Source::start(native_sender, true),
    )?);
    if let Some(source) = &source {
        source.set_shutdown_relay(service_mode && preferences.keep_tv_on);
    }
    let hotkey = ui::Hotkey::start(app.handle().clone(), false);
    let screen_hotkey = ui::Hotkey::start(app.handle().clone(), true);
    if preferences.screen_enabled
        && preferences.screen_shortcut_enabled
        && let Err(error) = screen_hotkey.set(preferences.screen_shortcut.clone())
    {
        update(&view, app.handle(), |s| {
            s.screen_shortcut_enabled = false;
            s.error = Some(error);
        });
    }
    let (screen_toggle, screen_toggles) = tokio::sync::mpsc::channel(4);
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
        screen_hotkey,
        screen_toggle,
        screen_changed: tokio::sync::Notify::new(),
        screen_epoch: std::sync::atomic::AtomicU64::new(0),
        screen_sequence: std::sync::atomic::AtomicU64::new(1),
        screen_source: now().saturating_mul(1_000_000) + u64::from(std::process::id()),
        screen_allowed: std::sync::atomic::AtomicBool::new(true),
        screen_session: std::sync::Mutex::new(Default::default()),
        shortcut_sleep: std::sync::atomic::AtomicBool::new(false),
        _lock: lock,
    });
    screen::start(app.handle().clone(), screen_toggles);

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

    let (topology_sender, mut topology_events) = tokio::sync::watch::channel(0u64);
    let topology_app = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        while topology_events.changed().await.is_ok() {
            let app = topology_app.clone();
            let state = app.state::<Desktop>();
            let _operation = state.operation.lock().await;
            topology_events.borrow_and_update();
            if ltac::lifecycle::is_ending() {
                return;
            }
            let result: Result<()> = async {
                if ui::refresh_pc_inputs(&state.path, &state.journal)? {
                    let automatic = read(&state).map_err(anyhow::Error::msg)?.automatic;
                    if !state.service_mode {
                        stop_watcher(&state).await;
                    }
                    ui::publish_hdmi_config(&state, &app, automatic).await?;
                }
                ui::refresh_devices(&state, &app)
            }
            .await;
            if let Err(error) = result {
                fail(&state, &app, &format!("HDMI: {error:#}"));
            }
        }
    });
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        while let Some(notice) = native_events.recv().await {
            let state = handle.state::<Desktop>();
            let phase = match notice.event {
                Event::ShutdownQuery => Some(ltac::service_ipc::ShutdownPhase::Query),
                Event::ShutdownCancelled => Some(ltac::service_ipc::ShutdownPhase::Cancel),
                Event::ShutdownConfirmed => Some(ltac::service_ipc::ShutdownPhase::Confirm),
                _ => None,
            };
            if let Some(phase) = phase {
                if state.service_mode {
                    let result = tokio::time::timeout(
                        Duration::from_millis(250),
                        ltac::service_ipc::request(ltac::service_ipc::Request::Shutdown { phase }),
                    )
                    .await;
                    if !matches!(result, Ok(Ok(_))) {
                        let _ = state
                            .journal
                            .clone()
                            .record("shutdown_relay_failed", json!({"phase":phase}));
                    }
                }
                if let Some(ack) = notice.suspend_ack {
                    let _ = ack.try_send(());
                }
                continue;
            }
            screen::native_event(&state, notice.event);
            if notice.event == Event::DisplayTopologyChanged {
                topology_sender.send_modify(|generation| *generation = generation.wrapping_add(1));
            }
            if ui::take_shortcut_resume(
                notice.event,
                &state.shortcut_sleep,
                read(&state).is_ok_and(|s| s.automatic),
            ) {
                let app = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<Desktop>();
                    if let Err(error) = ui::restore_after_sleep(&state, &app).await {
                        fail(&state, &app, &format!("sleep resume: {error:#}"));
                    }
                });
            }
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
                if !state.service_mode
                    && notice.event == Event::SessionEnding
                    && read(&state).is_ok_and(|s| s.automatic)
                {
                    let result = tokio::time::timeout(Duration::from_millis(1400), async {
                        let keep_tv_on = read(&state).map_err(anyhow::Error::msg)?.keep_tv_on;
                        let mut tasks = tokio::task::JoinSet::new();
                        for config in ui::configs(&state.path)? {
                            let mut journal = state.journal.clone();
                            tasks.spawn(async move {
                                if keep_tv_on {
                                    controller::off_protected(&config, &mut journal).await
                                } else {
                                    controller::off(&config, &mut journal).await
                                }
                            });
                        }
                        while let Some(result) = tasks.join_next().await {
                            result??;
                        }
                        Ok::<_, anyhow::Error>(())
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
            if discover_on_start {
                let _ = do_discover(&state, &handle).await;
            }
            return;
        }
        if state.service_mode {
            resume_automatic(&state, &handle, preferences.automatic).await;
            return;
        }
        if read(&state).is_ok_and(|s| s.automatic || s.screen_enabled)
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
    app.manage(UiMenu {
        open,
        automatic,
        quit,
    });
    let language = app
        .path()
        .app_config_dir()
        .ok()
        .and_then(|path| fs::read_to_string(path.join("ui-language.txt")).ok())
        .unwrap_or_else(system_locale);
    let _ = set_ui_language(app.handle().clone(), language);
    // Select the original-derived ICO entry at the tray's native pixel size.
    // The window's default icon uses the larger taskbar size.
    let tray_size = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
            windows_sys::Win32::UI::WindowsAndMessaging::SM_CXSMICON,
        )
    }
    .max(16) as u32;
    TrayIconBuilder::with_id("ltac")
        .icon(tauri::image::Image::from_app_icon_resource(tray_size)?)
        .tooltip("LTAC")
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

struct UiMenu {
    open: MenuItem<tauri::Wry>,
    automatic: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}

#[tauri::command]
fn set_ui_language(app: tauri::AppHandle, language: String) -> std::result::Result<(), String> {
    let code = language.split(['-', '_']).next().unwrap_or("en");
    let labels = match code {
        "ko" => ["LTAC 열기", "자동 동기화 켜기 / 끄기", "종료"],
        "ja" => ["LTACを開く", "自動同期のオン／オフ", "終了"],
        _ => ["Open LTAC", "Toggle automatic sync", "Quit"],
    };
    let menu = app.state::<UiMenu>();
    menu.open.set_text(labels[0]).map_err(|e| e.to_string())?;
    menu.automatic
        .set_text(labels[1])
        .map_err(|e| e.to_string())?;
    menu.quit.set_text(labels[2]).map_err(|e| e.to_string())?;
    if let Ok(path) = app.path().app_config_dir() {
        fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        fs::write(path.join("ui-language.txt"), code).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn system_locale() -> String {
    let mut name = [0u16; 85];
    // The Windows display language can differ from the regional date/number locale.
    let length = unsafe {
        windows_sys::Win32::Globalization::LCIDToLocaleName(
            windows_sys::Win32::Globalization::GetUserDefaultUILanguage() as u32,
            name.as_mut_ptr(),
            name.len() as i32,
            0,
        )
    };
    if length > 1 {
        String::from_utf16_lossy(&name[..length as usize - 1])
    } else {
        "en".into()
    }
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
            system_locale,
            set_ui_language,
            snapshot,
            refresh_status,
            set_automatic,
            pair_tv,
            discover_tvs,
            cancel_connection,
            ui::edit_device,
            ui::set_device_hdmi,
            ui::remove_device,
            ui::set_shortcut,
            screen::set_screen_protection,
            ui::set_keep_tv_on,
            crate::updater::update_status,
            crate::updater::check_for_updates_manual
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn shortcut_off_intention_is_retained_for_every_watcher() {
        let mut initial = Some(Initial {
            target: ltac::sync::Target::Off,
            session: None,
        });
        for _ in 0..16 {
            assert_eq!(
                next_initial(&mut initial).unwrap().target,
                ltac::sync::Target::Off
            );
        }
        assert!(next_initial(&mut None).is_none());
    }

    #[test]
    fn service_status_cannot_reset_saved_input_protection() {
        for enabled in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("config.json");
            let prefs = Preferences {
                keep_tv_on: enabled,
                ..Default::default()
            };
            write_preferences(&path, &prefs).unwrap();
            let (mut view, _) = initial_view(&path, true, true).unwrap();
            // Subscription can deliver an old policy before configuration or
            // after a newer command response. Neither changes user intent.
            for power in ["unknown", "on", "standby"] {
                apply_service_snapshot(
                    &mut view,
                    ltac::service_ipc::Snapshot {
                        power: power.into(),
                        automatic: false,
                        keep_tv_on: !enabled,
                        busy: false,
                        checked_at: Some(123),
                        error: None,
                    },
                );
                assert_eq!(view.keep_tv_on, enabled);
                assert_eq!(view.power, power);
                assert_eq!(view.checked_at, Some(123));
            }
            // Another preference change must persist the selected protection.
            write_preferences(
                &path,
                &Preferences {
                    automatic: view.automatic,
                    keep_tv_on: view.keep_tv_on,
                    shortcut_enabled: true,
                    shortcut: Some("F8".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            let (reopened, _) = initial_view(&path, true, true).unwrap();
            assert_eq!(reopened.keep_tv_on, enabled);
            assert!(reopened.shortcut_enabled);
        }
    }

    fn config(paired: bool) -> Config {
        Config {
            ip: "192.0.2.1".parse().unwrap(),
            mac: None,
            device: None,
            wake_verified: false,
            pc_hdmi: None,
            pc_hdmi_source: Default::default(),
            source_path: None,
            // Opaque sentinel: startup tests never decrypt a key or contact a TV.
            protected_key: paired.then(|| vec![1]),
            certificate_sha256: None,
            broadcast: "192.0.2.255".parse().unwrap(),
            transport: ltac::config::Transport::Auto,
        }
    }

    #[test]
    fn only_the_first_unconfigured_launch_opens_discovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let (first, _) = initial_view(&path, true, true).unwrap();
        assert_eq!(first.stage, "searching");
        assert!(!first.configured);
        assert!(preferences_path(&path).exists());
        let (second, _) = initial_view(&path, true, true).unwrap();
        assert_eq!(second.stage, "ready");
        assert!(!second.configured);
        assert!(second.devices.is_empty());
    }

    #[test]
    fn saved_devices_open_home_without_a_usable_legacy_config() {
        for legacy in [None, Some("unpaired"), Some("invalid")] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("config.json");
            ui::remember(&path, &config(true)).unwrap();
            match legacy {
                Some("unpaired") => config(false).save(&path).unwrap(),
                Some("invalid") => fs::write(&path, b"invalid").unwrap(),
                _ => {}
            }
            let before = fs::read_dir(path.with_file_name("devices"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let bytes = fs::read(&before).unwrap();
            let (view, _) = initial_view(&path, true, true).unwrap();
            assert_eq!(view.stage, "ready");
            assert!(view.configured);
            assert_eq!(view.devices.len(), 1);
            assert_eq!(fs::read(&before).unwrap(), bytes);
        }
    }

    #[test]
    fn paired_legacy_config_is_migrated_and_opens_home() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        config(true).save(&path).unwrap();
        let (view, _) = initial_view(&path, false, false).unwrap();
        assert!(view.configured);
        assert_eq!(view.stage, "ready");
        assert_eq!(view.devices.len(), 1);
    }

    #[test]
    fn unpaired_legacy_config_is_not_registered_as_a_device() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        config(false).save(&path).unwrap();
        let (view, _) = initial_view(&path, false, false).unwrap();
        assert!(!view.configured);
        assert_eq!(view.stage, "searching");
        assert!(view.devices.is_empty());
    }

    #[test]
    fn returning_empty_home_preserves_existing_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let bytes = br#"{"automatic":false,"shortcut_enabled":true,"shortcut":"Ctrl+K"}"#;
        fs::write(preferences_path(&path), bytes).unwrap();
        let (view, _) = initial_view(&path, true, true).unwrap();
        assert_eq!(view.stage, "ready");
        assert!(!view.automatic);
        assert!(view.shortcut_enabled);
        assert_eq!(view.shortcut.as_deref(), Some("Ctrl+K"));
        assert_eq!(fs::read(preferences_path(&path)).unwrap(), bytes);
    }
}
