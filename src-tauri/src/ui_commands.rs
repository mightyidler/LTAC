use super::*;
use std::sync::mpsc;

#[derive(Clone, Serialize)]
pub(super) struct SavedDevice {
    id: String,
    name: String,
    ip: String,
    mac: Option<String>,
    pc_hdmi: Option<u8>,
}

pub(super) fn configs(path: &Path) -> Result<Vec<Config>> {
    let folder = path.with_file_name("devices");
    let mut paths = if folder.exists() {
        fs::read_dir(folder)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    paths.sort();
    paths.into_iter().map(|path| Config::load(&path)).collect()
}

pub(super) fn already_saved(device: &Device, saved: &[Config]) -> bool {
    saved.iter().any(|config| {
        config.ip == device.ip
            || config
                .device
                .as_ref()
                .is_some_and(|identity| identity.id.eq_ignore_ascii_case(&device.identity.id))
    })
}

pub(super) fn remember(path: &Path, config: &Config) -> Result<()> {
    let existing = configs(path)?;
    if let Some(previous) = existing.iter().find(|previous| {
        previous.ip == config.ip
            || (previous.device.is_some()
                && previous.device.as_ref().map(|d| &d.id) == config.device.as_ref().map(|d| &d.id))
    }) {
        let mut updated = config.clone();
        // Re-pairing cannot discard a previously selected PC input.
        if previous.hdmi_selection_pinned() || updated.pc_hdmi.is_none() {
            updated.pc_hdmi = previous.pc_hdmi;
            updated.pc_hdmi_source = previous.pc_hdmi_source;
        }
        return updated.save(
            previous
                .source_path
                .as_ref()
                .context("Missing device path")?,
        );
    }
    anyhow::ensure!(existing.len() < 16, "TV는 최대 16대까지 연결할 수 있어요.");
    let id = config
        .device
        .as_ref()
        .map(|d| d.id.clone())
        .unwrap_or_else(|| config.ip.to_string());
    let file = id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let folder = path.with_file_name("devices");
    fs::create_dir_all(&folder)?;
    config.save(&folder.join(format!("{file}.json")))
}

pub(super) fn devices(path: &Path) -> Result<Vec<SavedDevice>> {
    configs(path)?
        .iter()
        .map(|config| {
            Ok(SavedDevice {
                id: config
                    .source_path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .context("Missing device ID")?
                    .to_string_lossy()
                    .into(),
                name: config
                    .device
                    .as_ref()
                    .map(|d| d.display_name())
                    .unwrap_or_else(|| config.ip.to_string()),
                ip: config.ip.to_string(),
                mac: config.mac.map(|m| m.to_string()),
                pc_hdmi: config.pc_hdmi,
            })
        })
        .collect()
}

pub(super) fn refresh_devices(state: &Desktop, app: &tauri::AppHandle) -> Result<()> {
    let devices = devices(&state.path)?;
    update(&state.view, app, |s| {
        s.configured = !devices.is_empty();
        s.devices = devices;
    });
    Ok(())
}

struct HdmiChange {
    device: Config,
    legacy: Option<Config>,
}

impl HdmiChange {
    fn restore(&self, path: &Path) -> Result<()> {
        self.device.save(
            self.device
                .source_path
                .as_deref()
                .context("Missing device path")?,
        )?;
        if let Some(legacy) = &self.legacy {
            legacy.save(path)?;
        }
        Ok(())
    }
}

fn save_device_hdmi(path: &Path, id: &str, port: u8) -> Result<HdmiChange> {
    anyhow::ensure!(
        (1..=4).contains(&port),
        "HDMI 번호는 1~4 중에서 선택해주세요."
    );
    let device = configs(path)?
        .into_iter()
        .find(|config| {
            config
                .source_path
                .as_ref()
                .and_then(|p| p.file_stem())
                .is_some_and(|stem| stem == id)
        })
        .context("기기를 찾을 수 없어요.")?;
    let legacy = Config::load(path)
        .ok()
        .filter(|active| active.ip == device.ip);
    let change = HdmiChange { device, legacy };
    let mut updated = change.device.clone();
    updated.set_manual_hdmi(port);
    updated.save(
        updated
            .source_path
            .as_deref()
            .context("Missing device path")?,
    )?;
    if let Some(legacy) = &change.legacy {
        let mut updated = legacy.clone();
        updated.set_manual_hdmi(port);
        if let Err(error) = updated.save(path) {
            change.restore(path)?;
            return Err(error);
        }
    }
    Ok(change)
}

pub(super) async fn publish_hdmi_config(
    state: &Desktop,
    app: &tauri::AppHandle,
    automatic: bool,
) -> Result<()> {
    if state.service_mode {
        let devices = configs(&state.path)?
            .iter()
            .map(Config::for_service)
            .collect::<Result<Vec<_>>>()?;
        let remote =
            ltac::service_ipc::request(ltac::service_ipc::Request::Configure { devices }).await?;
        apply_service(state, app, remote);
    } else if automatic && read(state).map_err(anyhow::Error::msg)?.configured {
        start_watcher(state, app, None).await?;
    }
    Ok(())
}

#[tauri::command]
pub(super) async fn set_device_hdmi(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
    id: String,
    pc_hdmi: u8,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.")?;
    let result: Result<()> = async {
        let automatic = read(&state).map_err(anyhow::Error::msg)?.automatic;
        // Validate and persist only the selected TV. Existing automatic policy
        // and other preferences are not changed by an input selection.
        let change = save_device_hdmi(&state.path, &id, pc_hdmi)?;
        if !state.service_mode {
            stop_watcher(&state).await;
        }
        if let Err(error) = publish_hdmi_config(&state, &app, automatic).await {
            change.restore(&state.path)?;
            if !state.service_mode {
                stop_watcher(&state).await;
            }
            if let Err(restore_error) = publish_hdmi_config(&state, &app, automatic).await {
                return Err(error.context(format!("HDMI rollback failed: {restore_error:#}")));
            }
            return Err(error);
        }
        refresh_devices(&state, &app)?;
        update(&state.view, &app, |s| s.error = None);
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = refresh_devices(&state, &app);
        fail(&state, &app, &format!("HDMI selection: {error:#}"));
    }
    read(&state)
}

#[tauri::command]
pub(super) async fn edit_device(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
    id: String,
    ip: String,
    mac: Option<String>,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.")?;
    let result: Result<()> = async {
        let mut config = configs(&state.path)?
            .into_iter()
            .find(|c| {
                c.source_path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .is_some_and(|stem| stem == id.as_str())
            })
            .context("기기를 찾을 수 없어요.")?;
        let previous_ip = config.ip;
        config.ip = ip.parse().context("IP 주소를 확인해주세요.")?;
        config.broadcast = discovery::routed_broadcast(config.ip)?;
        let new_mac = mac
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(anyhow::Error::msg)?;
        if new_mac != config.mac || previous_ip != config.ip {
            config.wake_verified = false;
        }
        config.mac = new_mac;
        config.validate()?;
        anyhow::ensure!(
            !configs(&state.path)?
                .iter()
                .any(|other| other.ip == config.ip && other.source_path != config.source_path),
            "이미 연결된 TV의 IP 주소입니다."
        );
        let automatic = read(&state).unwrap().automatic;
        pause_devices(&state).await?;
        config.save(config.source_path.as_ref().context("Missing device path")?)?;
        // Keep the legacy config synchronized for CLI/service interoperability.
        let active = Config::load(&state.path)?;
        if active.ip == previous_ip {
            config.save(&state.path)?;
        }
        refresh_devices(&state, &app)?;
        update(&state.view, &app, |s| s.error = None);
        resume_automatic(&state, &app, automatic).await;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = pause_devices(&state).await;
        let _ = save_preferences(&state.path, false);
        update(&state.view, &app, |s| s.automatic = false);
        fail(&state, &app, &format!("edit device: {error:#}"));
    }
    read(&state)
}

#[tauri::command]
pub(super) async fn remove_device(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
    id: String,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.")?;
    let result: Result<()> = async {
        let all = configs(&state.path)?;
        let config = all
            .iter()
            .find(|c| {
                c.source_path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .is_some_and(|stem| stem == id.as_str())
            })
            .context("기기를 찾을 수 없어요.")?;
        let automatic = read(&state).unwrap().automatic;
        pause_devices(&state).await?;
        fs::remove_file(config.source_path.as_ref().context("Missing device path")?)?;
        let remaining = configs(&state.path)?;
        if let Some(next) = remaining.first() {
            next.save(&state.path)?;
        } else {
            if state.path.exists() {
                fs::remove_file(&state.path)?;
            }
            save_preferences(&state.path, automatic)?;
        }
        refresh_devices(&state, &app)?;
        update(&state.view, &app, |s| {
            s.stage = "ready".into();
            s.automatic = automatic;
            s.error = None;
        });
        resume_automatic(&state, &app, automatic).await;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = pause_devices(&state).await;
        let _ = save_preferences(&state.path, false);
        update(&state.view, &app, |s| s.automatic = false);
        fail(&state, &app, &format!("remove device: {error:#}"));
    }
    read(&state)
}

enum HotkeyCommand {
    Shortcut(Option<String>),
    TrackInput(bool),
}
type HotkeyRequest = (
    HotkeyCommand,
    mpsc::SyncSender<std::result::Result<(), String>>,
);
pub(super) struct Hotkey {
    sender: mpsc::SyncSender<HotkeyRequest>,
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
    input: Option<Arc<screen_input::Activity>>,
}
impl Hotkey {
    pub(super) fn start(app: tauri::AppHandle, screen: bool) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<HotkeyRequest>(1);
        let (ready, started) = mpsc::sync_channel(1);
        let input = screen.then(screen_input::Activity::new);
        let activity = input.clone();
        let thread = std::thread::spawn(move || {
            use windows_sys::Win32::UI::{
                Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey},
                WindowsAndMessaging::{GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, WM_HOTKEY},
            };
            let mut message: MSG = unsafe { std::mem::zeroed() };
            unsafe {
                PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
            }
            let _ =
                ready.send(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() });
            let mut active = false;
            let mut previous: Option<(u32, u32)> = None;
            let mut input_registration = None;
            while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
                if ltac::lifecycle::is_ending() {
                    break;
                }
                while let Ok((command, reply)) = receiver.try_recv() {
                    let shortcut = match command {
                        HotkeyCommand::Shortcut(shortcut) => shortcut,
                        HotkeyCommand::TrackInput(enabled) => {
                            let result = if enabled && input_registration.is_none() {
                                activity
                                    .as_ref()
                                    .ok_or_else(|| "Input tracker is unavailable".to_owned())
                                    .and_then(|activity| {
                                        activity.register().map_err(|error| format!("{error:#}"))
                                    })
                                    .map(|registration| input_registration = Some(registration))
                            } else {
                                if !enabled {
                                    input_registration.take();
                                }
                                Ok(())
                            };
                            let _ = reply.send(result);
                            continue;
                        }
                    };
                    let result = shortcut
                        .as_deref()
                        .map(parse_shortcut)
                        .transpose()
                        .and_then(|parsed| {
                            if active {
                                unsafe {
                                    UnregisterHotKey(std::ptr::null_mut(), 1);
                                }
                                active = false;
                            }
                            if let Some((modifiers, key)) = parsed {
                                if unsafe {
                                    RegisterHotKey(
                                        std::ptr::null_mut(),
                                        1,
                                        modifiers | MOD_NOREPEAT,
                                        key,
                                    )
                                } == 0
                                {
                                    if let Some((modifiers, key)) = previous {
                                        active = unsafe {
                                            RegisterHotKey(
                                                std::ptr::null_mut(),
                                                1,
                                                modifiers | MOD_NOREPEAT,
                                                key,
                                            )
                                        } != 0;
                                    }
                                    return Err("다른 앱에서 사용 중인 단축키입니다.".into());
                                }
                                active = true;
                            }
                            previous = parsed;
                            Ok(())
                        });
                    let _ = reply.send(result);
                }
                if message.message == WM_HOTKEY && active {
                    if screen {
                        let state = app.state::<Desktop>();
                        let _ = state.screen_toggle.try_send(());
                        continue;
                    }
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<Desktop>();
                        if let Err(error) = sleep_all(&state, &app).await {
                            fail(&state, &app, &format!("sleep shortcut: {error:#}"));
                        }
                    });
                }
            }
            if active {
                unsafe {
                    UnregisterHotKey(std::ptr::null_mut(), 1);
                }
            }
        });
        let thread_id = started.recv().unwrap_or(0);
        Self {
            sender,
            thread_id,
            thread: Some(thread),
            input,
        }
    }
    pub(super) fn set(&self, shortcut: Option<String>) -> std::result::Result<(), String> {
        self.request(HotkeyCommand::Shortcut(shortcut))
    }

    pub(super) fn input_idle(&self) -> Option<(u32, u32)> {
        self.input.as_ref()?.sample()
    }

    pub(super) fn set_input_enabled(&self, enabled: bool) -> std::result::Result<(), String> {
        if self
            .input
            .as_ref()
            .is_some_and(|input| input.registered() == enabled)
        {
            return Ok(());
        }
        self.request(HotkeyCommand::TrackInput(enabled))
    }

    fn request(&self, command: HotkeyCommand) -> std::result::Result<(), String> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send((command, reply))
            .map_err(|_| "단축키를 등록하지 못했어요.")?;
        if self.thread_id == 0
            || unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.thread_id,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1,
                    0,
                    0,
                )
            } == 0
        {
            return Err("단축키를 등록하지 못했어요.".into());
        }
        receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "단축키 등록 시간이 초과되었어요.")?
    }
}
impl Drop for Hotkey {
    fn drop(&mut self) {
        if self.thread_id != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.thread_id,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT,
                    0,
                    0,
                );
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub(super) fn parse_shortcut(shortcut: &str) -> std::result::Result<(u32, u32), String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
    };
    let mut modifiers = 0;
    let mut key = None;
    for part in shortcut.split('+') {
        match part {
            "Ctrl" => modifiers |= MOD_CONTROL,
            "Alt" => modifiers |= MOD_ALT,
            "Shift" => modifiers |= MOD_SHIFT,
            "Win" => modifiers |= MOD_WIN,
            value => {
                let parsed = if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() {
                    Some(value.as_bytes()[0].to_ascii_uppercase() as u32)
                } else if let Some(function) = value
                    .strip_prefix('F')
                    .and_then(|n| n.parse::<u32>().ok())
                    .filter(|n| (1..=24).contains(n))
                {
                    Some(0x70 + function - 1)
                } else if let Some(numpad) = value
                    .strip_prefix("Numpad")
                    .and_then(|n| n.parse::<u32>().ok())
                    .filter(|n| *n <= 9)
                {
                    Some(0x60 + numpad)
                } else {
                    match value {
                        "Space" => Some(0x20),
                        "Enter" | "NumpadEnter" => Some(0x0d),
                        "Tab" => Some(0x09),
                        "Escape" => Some(0x1b),
                        "Backspace" => Some(0x08),
                        "Delete" => Some(0x2e),
                        "Insert" => Some(0x2d),
                        "Home" => Some(0x24),
                        "End" => Some(0x23),
                        "PageUp" => Some(0x21),
                        "PageDown" => Some(0x22),
                        "ArrowLeft" => Some(0x25),
                        "ArrowUp" => Some(0x26),
                        "ArrowRight" => Some(0x27),
                        "ArrowDown" => Some(0x28),
                        "CapsLock" => Some(0x14),
                        "NumLock" => Some(0x90),
                        "ScrollLock" => Some(0x91),
                        "Pause" => Some(0x13),
                        "PrintScreen" => Some(0x2c),
                        "Backquote" => Some(0xc0),
                        "Minus" => Some(0xbd),
                        "Equal" => Some(0xbb),
                        "BracketLeft" => Some(0xdb),
                        "BracketRight" => Some(0xdd),
                        "Backslash" => Some(0xdc),
                        "IntlBackslash" => Some(0xe2),
                        "Semicolon" => Some(0xba),
                        "Quote" => Some(0xde),
                        "Comma" => Some(0xbc),
                        "Period" => Some(0xbe),
                        "Slash" => Some(0xbf),
                        "NumpadMultiply" => Some(0x6a),
                        "NumpadAdd" => Some(0x6b),
                        "NumpadSubtract" => Some(0x6d),
                        "NumpadDecimal" => Some(0x6e),
                        "NumpadDivide" => Some(0x6f),
                        "AudioVolumeMute" => Some(0xad),
                        "AudioVolumeDown" => Some(0xae),
                        "AudioVolumeUp" => Some(0xaf),
                        "MediaTrackNext" => Some(0xb0),
                        "MediaTrackPrevious" => Some(0xb1),
                        "MediaStop" => Some(0xb2),
                        "MediaPlayPause" => Some(0xb3),
                        _ => None,
                    }
                };
                if key.is_some() || parsed.is_none() {
                    return Err("지원하는 키 조합을 입력해주세요.".into());
                }
                key = parsed;
            }
        }
    }
    Ok((modifiers, key.ok_or("단축키를 지정해주세요.")?))
}

#[cfg(test)]
mod shortcut_tests {
    use super::parse_shortcut;

    #[test]
    fn accepts_single_keys_and_shift_only_combinations() {
        assert_eq!(parse_shortcut("K"), Ok((0, 0x4b)));
        assert_eq!(parse_shortcut("F8"), Ok((0, 0x77)));
        assert_eq!(parse_shortcut("Shift+Semicolon"), Ok((4, 0xba)));
    }

    #[test]
    fn preserves_numpad_keys_and_rejects_incomplete_combinations() {
        assert_eq!(parse_shortcut("Numpad1"), Ok((0, 0x61)));
        assert_eq!(parse_shortcut("Ctrl+NumpadAdd"), Ok((2, 0x6b)));
        assert!(parse_shortcut("Ctrl").is_err());
        assert!(parse_shortcut("K+L").is_err());
    }
}

pub(super) fn take_shortcut_resume(
    event: Event,
    pending: &std::sync::atomic::AtomicBool,
    automatic: bool,
) -> bool {
    event == Event::ResumeInteractive
        && pending.swap(false, std::sync::atomic::Ordering::AcqRel)
        && !automatic
}

async fn sleep_all(state: &Desktop, app: &tauri::AppHandle) -> Result<()> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| anyhow::anyhow!("다른 작업을 처리 중입니다."))?;
    if !read(state).map_err(anyhow::Error::msg)?.shortcut_enabled {
        return Ok(());
    }
    // Fail before touching the TV if Windows cannot grant sleep permission.
    tokio::task::spawn_blocking(|| with_sleep_privilege(|| Ok(()))).await??;
    super::screen::native_event(state, Event::Suspend);
    state
        .shortcut_sleep
        .store(true, std::sync::atomic::Ordering::Release);
    let result = sleep_with_recovery(
        async {
            if state.service_mode {
                // Propagate IPC failures, including transport failures that otherwise
                // only update the UI snapshot.
                let remote = ltac::service_ipc::request(ltac::service_ipc::Request::Sleep).await?;
                apply_service(state, app, remote);
            } else {
                stop_watcher(state).await;
                let all = configs(&state.path)?;
                let keep_tv_on = read(state).map_err(anyhow::Error::msg)?.keep_tv_on;
                let mut tasks = tokio::task::JoinSet::new();
                for config in all {
                    let mut journal = state.journal.clone();
                    tasks.spawn(async move {
                        if keep_tv_on {
                            controller::off_protected(&config, &mut journal).await
                        } else {
                            controller::off(&config, &mut journal).await
                        }
                    });
                }
                let result = tokio::time::timeout(Duration::from_secs(5), async {
                    while let Some(result) = tasks.join_next().await {
                        result??;
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .await;
                result
                    .context("TV 절전 요청 시간이 초과되었어요.")
                    .and_then(|r| r)?;
                if read(state).map_err(anyhow::Error::msg)?.automatic {
                    start_watcher(
                        state,
                        app,
                        Some(Initial {
                            target: ltac::sync::Target::Off,
                            session: None,
                        }),
                    )
                    .await?;
                }
            }
            Ok(())
        },
        async {
            let _ = state
                .journal
                .clone()
                .record("pc_suspend_requested", json!({}));
            tokio::task::spawn_blocking(suspend_pc).await?
        },
        async {
            state
                .shortcut_sleep
                .store(false, std::sync::atomic::Ordering::Release);
            restore_after_sleep(state, app).await
        },
    )
    .await;
    let _ = state.journal.clone().record(
        "pc_suspend_returned",
        json!({
            "success":result.is_ok(), "error":result.as_ref().err().map(|e|format!("{e:#}"))
        }),
    );
    result
}

async fn sleep_with_recovery(
    off: impl std::future::Future<Output = Result<()>>,
    suspend: impl std::future::Future<Output = Result<()>>,
    restore: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    let result = async {
        off.await?;
        suspend.await
    }
    .await;
    if let Err(error) = result {
        return match restore.await {
            Ok(()) => Err(error),
            Err(recovery) => Err(error.context(format!("TV 복구 실패: {recovery:#}"))),
        };
    }
    Ok(())
}

pub(super) async fn restore_after_sleep(state: &Desktop, app: &tauri::AppHandle) -> Result<()> {
    super::screen::native_event(state, Event::ResumeInteractive);
    if state.service_mode {
        let remote = ltac::service_ipc::request(ltac::service_ipc::Request::On).await?;
        apply_service(state, app, remote);
    } else {
        stop_watcher(state).await;
        let mut tasks = tokio::task::JoinSet::new();
        for config in configs(&state.path)? {
            let mut journal = state.journal.clone();
            tasks.spawn(async move {
                controller::wake_session(&config, &mut journal, Duration::from_secs(45))
                    .await
                    .map(|_| ())
            });
        }
        let result: Result<()> = async {
            while let Some(result) = tasks.join_next().await {
                result??;
            }
            Ok(())
        }
        .await;
        if read(state)
            .map_err(anyhow::Error::msg)
            .is_ok_and(|s| s.automatic || s.screen_enabled)
        {
            start_watcher(state, app, None).await?;
        }
        result?;
    }
    Ok(())
}

fn suspend_pc() -> Result<()> {
    with_sleep_privilege(|| {
        if !unsafe { windows_sys::Win32::System::Power::SetSuspendState(false, false, false) } {
            return Err(std::io::Error::last_os_error())
                .context("PC를 절전 상태로 전환하지 못했어요.");
        }
        Ok(())
    })
}

fn with_sleep_privilege(action: impl FnOnce() -> Result<()>) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, HANDLE, SetLastError},
        Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        },
        System::{
            Power::IsPwrSuspendAllowed,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };
    // SetSuspendState requires SeShutdownPrivilege, even for a user-requested sleep.
    // Restore the prior token state after the call (or any failure).
    struct Token {
        handle: HANDLE,
        previous: TOKEN_PRIVILEGES,
        restore: bool,
    }
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                if self.restore {
                    AdjustTokenPrivileges(
                        self.handle,
                        0,
                        &self.previous,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                }
                CloseHandle(self.handle);
            }
        }
    }
    unsafe {
        anyhow::ensure!(IsPwrSuspendAllowed(), "이 PC에서 절전을 사용할 수 없어요.");
        let mut handle = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut handle,
        ) == 0
        {
            return Err(std::io::Error::last_os_error()).context("절전 권한을 확인하지 못했어요.");
        }
        let mut token = Token {
            handle,
            previous: std::mem::zeroed(),
            restore: false,
        };
        let name: Vec<u16> = "SeShutdownPrivilege\0".encode_utf16().collect();
        let mut luid = std::mem::zeroed();
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) == 0 {
            return Err(std::io::Error::last_os_error()).context("절전 권한을 찾지 못했어요.");
        }
        let desired = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        SetLastError(0);
        // ReturnLength is required whenever PreviousState is supplied. A null
        // pointer fails with ERROR_NOACCESS (998) before SetSuspendState runs.
        let mut returned = 0;
        if AdjustTokenPrivileges(
            token.handle,
            0,
            &desired,
            std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
            &mut token.previous,
            &mut returned,
        ) == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("절전 권한을 활성화하지 못했어요.");
        }
        token.restore = true;
        let error = GetLastError();
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .context("현재 계정에 절전 권한이 없어요.");
        }
        action()?;
    }
    Ok(())
}

/// Called only for an actual change in the active Windows display connections.
pub(super) fn refresh_pc_inputs(path: &Path, journal: &Journal) -> Result<bool> {
    let saved = configs(path)?;
    let detection = if saved.len() == 1 {
        ltac::hdmi::capture().detect()
    } else {
        Err("multiple_tvs_select_hdmi_manually")
    };
    let mut changed = false;
    for mut config in saved {
        if config.hdmi_selection_pinned() {
            continue;
        }
        let before = config.pc_hdmi;
        config.apply_hdmi_detection(detection.as_ref().ok().copied());
        let _ = journal.clone().record(
            "pc_hdmi_detection",
            json!({
                "ip":config.ip,"port":config.pc_hdmi,"reason":detection.as_ref().err()
            }),
        );
        let device_path = config
            .source_path
            .as_deref()
            .context("Missing device path")?;
        config.save(device_path)?;
        if Config::load(path).is_ok_and(|active| active.ip == config.ip) {
            config.save(path)?;
        }
        changed |= before != config.pc_hdmi;
    }
    Ok(changed)
}

#[tauri::command]
pub(super) async fn set_keep_tv_on(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
    enabled: bool,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.")?;
    let previous = read(&state)?;
    if state.service_mode {
        let remote = ltac::service_ipc::request(ltac::service_ipc::Request::KeepTvOn { enabled })
            .await
            .map_err(|e| e.to_string())?;
        apply_service(&state, &app, remote);
    } else {
        stop_watcher(&state).await;
    }
    let prefs = Preferences {
        automatic: previous.automatic,
        keep_tv_on: enabled,
        shortcut_enabled: previous.shortcut_enabled,
        shortcut: previous.shortcut.clone(),
        ..Preferences::from(&previous)
    };
    if let Err(error) = write_preferences(&state.path, &prefs) {
        if state.service_mode {
            let _ = service_request(
                &state,
                &app,
                ltac::service_ipc::Request::KeepTvOn {
                    enabled: previous.keep_tv_on,
                },
            )
            .await;
        } else if previous.automatic && previous.configured {
            resume_automatic(&state, &app, true).await;
        }
        return Err(error.to_string());
    }
    update(&state.view, &app, |s| {
        s.keep_tv_on = enabled;
        s.error = None;
    });
    if let Some(source) = &state._source {
        source.set_shutdown_relay(state.service_mode && enabled);
    }
    if enabled {
        let _ = refresh_devices(&state, &app);
        if state.service_mode {
            configure_service(&state, previous.automatic)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    if !state.service_mode && (previous.automatic || previous.screen_enabled) && previous.configured
    {
        resume_automatic(&state, &app, previous.automatic).await;
    }
    read(&state)
}

#[tauri::command]
pub(super) async fn set_shortcut(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
    enabled: bool,
    shortcut: Option<String>,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.")?;
    let previous = read(&state)?;
    if let (Some(a), Some(b)) = (&shortcut, &previous.screen_shortcut)
        && parse_shortcut(a)? == parse_shortcut(b)?
    {
        return Err("이미 다른 LTAC 단축키에 지정된 조합입니다.".into());
    }
    let active = if enabled { shortcut.clone() } else { None };
    state.hotkey.set(active)?;
    let result: Result<()> = (|| {
        let path = preferences_path(&state.path);
        let prefs = Preferences {
            automatic: previous.automatic,
            keep_tv_on: previous.keep_tv_on,
            shortcut_enabled: enabled,
            shortcut: shortcut.clone(),
            ..Preferences::from(&previous)
        };
        let mut file =
            tempfile::NamedTempFile::new_in(path.parent().context("Missing settings directory")?)?;
        file.write_all(&serde_json::to_vec_pretty(&prefs)?)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = state.hotkey.set(if previous.shortcut_enabled {
            previous.shortcut
        } else {
            None
        });
        return Err("설정을 저장하지 못했어요.".into());
    }
    update(&state.view, &app, |s| {
        s.shortcut_enabled = enabled;
        s.shortcut = shortcut;
        s.error = None;
    });
    read(&state)
}

#[cfg(test)]
mod device_tests {
    use super::*;
    fn config(last: u8) -> Config {
        Config {
            ip: std::net::Ipv4Addr::new(192, 0, 2, last),
            mac: None,
            device: None,
            wake_verified: false,
            pc_hdmi: None,
            pc_hdmi_source: Default::default(),
            source_path: None,
            protected_key: None,
            certificate_sha256: None,
            broadcast: "192.0.2.255".parse().unwrap(),
            transport: ltac::config::Transport::Auto,
        }
    }

    #[test]
    fn hdmi_selection_persists_per_tv_and_exports_to_service_without_changing_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let prefs =
            br#"{"automatic":true,"keep_tv_on":true,"shortcut_enabled":false,"shortcut":null}"#;
        fs::write(preferences_path(&path), prefs).unwrap();
        for last in 1..=2 {
            let mut device = config(last);
            device.mac = Some(format!("02:00:00:00:00:{last:02X}").parse().unwrap());
            device.set_key("hdmi-choice-key").unwrap();
            device.pc_hdmi = Some(last);
            remember(&path, &device).unwrap();
            if last == 1 {
                device.save(&path).unwrap();
            }
        }
        let saved = devices(&path).unwrap();
        let other_path = configs(&path).unwrap()[1].source_path.clone().unwrap();
        let other_before = fs::read(&other_path).unwrap();
        let old_mac = configs(&path).unwrap()[0].mac;
        save_device_hdmi(&path, &saved[0].id, 4).unwrap();
        assert_eq!(devices(&path).unwrap()[0].pc_hdmi, Some(4));
        assert_eq!(Config::load(&path).unwrap().pc_hdmi, Some(4));
        assert_eq!(fs::read(&other_path).unwrap(), other_before);
        assert_eq!(fs::read(preferences_path(&path)).unwrap(), prefs);
        let configs = configs(&path).unwrap();
        assert_eq!(configs[0].mac, old_mac);
        assert_eq!(configs[0].key().unwrap(), "hdmi-choice-key");
        let exported = configs
            .iter()
            .map(Config::for_service)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            exported
                .iter()
                .map(|config| config.pc_hdmi)
                .collect::<Vec<_>>(),
            vec![Some(4), Some(2)]
        );
        assert_eq!(exported[0].key().unwrap(), "hdmi-choice-key");
        assert_eq!(exported[0].pc_hdmi_source, ltac::hdmi::InputSource::Manual);
        // Re-pairing without a detected input must retain the manual choice.
        let mut repaired = configs[0].clone();
        repaired.pc_hdmi = None;
        repaired.pc_hdmi_source = ltac::hdmi::InputSource::Automatic;
        remember(&path, &repaired).unwrap();
        assert_eq!(devices(&path).unwrap()[0].pc_hdmi, Some(4));
        assert_eq!(
            super::configs(&path).unwrap()[0].pc_hdmi_source,
            ltac::hdmi::InputSource::Manual
        );
    }

    #[test]
    fn invalid_or_missing_hdmi_selection_does_not_modify_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        config(1).save(&path).unwrap();
        remember(&path, &config(1)).unwrap();
        let id = devices(&path).unwrap()[0].id.clone();
        let device_path = configs(&path).unwrap()[0].source_path.clone().unwrap();
        let before = fs::read(&device_path).unwrap();
        let legacy = fs::read(&path).unwrap();
        for port in [0, 5, u8::MAX] {
            assert!(save_device_hdmi(&path, &id, port).is_err());
        }
        assert!(save_device_hdmi(&path, "../../config", 2).is_err());
        assert_eq!(fs::read(device_path).unwrap(), before);
        assert_eq!(fs::read(path).unwrap(), legacy);
    }

    #[test]
    fn failed_hdmi_application_can_restore_both_device_and_legacy_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut first = config(1);
        first.pc_hdmi = Some(3);
        first.save(&path).unwrap();
        remember(&path, &first).unwrap();
        let id = devices(&path).unwrap()[0].id.clone();
        let change = save_device_hdmi(&path, &id, 1).unwrap();
        assert_eq!(devices(&path).unwrap()[0].pc_hdmi, Some(1));
        change.restore(&path).unwrap();
        assert_eq!(devices(&path).unwrap()[0].pc_hdmi, Some(3));
        assert_eq!(Config::load(&path).unwrap().pc_hdmi, Some(3));
        // The legacy CLI configuration can belong to another TV.
        config(2).save(&path).unwrap();
        let other = fs::read(&path).unwrap();
        save_device_hdmi(&path, &id, 2).unwrap();
        assert_eq!(fs::read(&path).unwrap(), other);
    }

    #[test]
    fn saved_discovery_matches_identity_after_ip_change_and_manual_ip() {
        let identity = ltac::discovery::DeviceIdentity {
            id: "uuid:TV-1".into(),
            name: "[LG] webOS TV OLED42C4ENA".into(),
            model: "LG Smart TV".into(),
        };
        let device = Device {
            identity: identity.clone(),
            ip: "192.0.2.2".parse().unwrap(),
            broadcast: "192.0.2.255".parse().unwrap(),
        };
        let mut saved = config(1);
        saved.device = Some(ltac::discovery::DeviceIdentity {
            id: "uuid:tv-1".into(),
            ..identity
        });
        assert!(already_saved(&device, &[saved.clone()]));
        saved.device = None;
        assert!(!already_saved(&device, &[saved.clone()]));
        saved.ip = device.ip;
        assert!(already_saved(&device, &[saved]));
    }

    #[test]
    fn existing_saved_names_are_shortened_without_repairing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut saved = config(1);
        saved.device = Some(ltac::discovery::DeviceIdentity {
            id: "uuid:tv-1".into(),
            name: "[LG] webOS TV OLED42C4ENA".into(),
            model: "LG Smart TV".into(),
        });
        remember(&path, &saved).unwrap();
        assert_eq!(devices(&path).unwrap()[0].name, "OLED42C4ENA");
        assert_eq!(
            configs(&path).unwrap()[0].device.as_ref().unwrap().name,
            "[LG] webOS TV OLED42C4ENA"
        );
    }
    #[test]
    fn discovery_of_a_manually_added_tv_updates_the_existing_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut manual = config(1);
        remember(&path, &manual).unwrap();
        let original = devices(&path).unwrap()[0].id.clone();
        manual.device = Some(ltac::discovery::DeviceIdentity {
            id: "uuid:tv/../../unsafe".into(),
            name: "Living room".into(),
            model: "OLED".into(),
        });
        remember(&path, &manual).unwrap();
        let saved = devices(&path).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, original);
        assert_eq!(saved[0].name, "Living room");
        assert_eq!(
            configs(&path).unwrap()[0].device.as_ref().unwrap().id,
            "uuid:tv/../../unsafe"
        );
    }
    #[test]
    fn device_limit_never_overwrites_existing_devices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        for last in 1..=16 {
            remember(&path, &config(last)).unwrap();
        }
        assert!(remember(&path, &config(17)).is_err());
        remember(&path, &config(1)).unwrap();
        assert_eq!(devices(&path).unwrap().len(), 16);
    }
}

#[cfg(test)]
mod sleep_tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn shortcut_resume_has_one_owner_and_maintenance_wake_does_not_restore() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for automatic in [false, true] {
            let pending = AtomicBool::new(true);
            assert!(!take_shortcut_resume(
                Event::ResumeAutomatic,
                &pending,
                automatic
            ));
            assert!(pending.load(Ordering::Acquire));
            assert_eq!(
                take_shortcut_resume(Event::ResumeInteractive, &pending, automatic),
                !automatic
            );
            assert!(!pending.load(Ordering::Acquire));
            assert!(!take_shortcut_resume(
                Event::ResumeInteractive,
                &pending,
                automatic
            ));
        }
    }

    #[test]
    fn first_run_and_legacy_preferences_default_to_sync_on() {
        assert!(Preferences::default().automatic);
        assert!(serde_json::from_str::<Preferences>("{}").unwrap().automatic);
        assert!(!Preferences::default().keep_tv_on);
        assert!(!Preferences::default().shortcut_enabled);
        assert!(
            !serde_json::from_str::<Preferences>(r#"{"automatic":true}"#)
                .unwrap()
                .keep_tv_on
        );
        assert!(
            !serde_json::from_str::<Preferences>(r#"{"automatic":false}"#)
                .unwrap()
                .automatic
        );
    }

    #[test]
    fn input_protection_survives_other_preference_changes_and_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let prefs = Preferences {
            keep_tv_on: true,
            shortcut_enabled: true,
            shortcut: Some("F8".into()),
            ..Default::default()
        };
        write_preferences(&path, &prefs).unwrap();
        save_preferences(&path, false).unwrap();
        let (snapshot, _) = initial_view(&path, false, false).unwrap();
        assert!(snapshot.keep_tv_on);
        assert!(!snapshot.automatic);
        assert!(snapshot.shortcut_enabled);
        assert_eq!(snapshot.shortcut.as_deref(), Some("F8"));
    }

    #[test]
    fn native_sleep_preflight_enables_privilege_without_suspending_pc() {
        // Hosted Windows VMs may not expose standby. In that environment,
        // verify preflight refuses the operation before it reaches the action.
        if !unsafe { windows_sys::Win32::System::Power::IsPwrSuspendAllowed() } {
            assert!(with_sleep_privilege(|| panic!("Unsupported sleep must be rejected")).is_err());
            return;
        }
        with_sleep_privilege(|| {
            use windows_sys::Win32::{
                Foundation::CloseHandle,
                Security::{
                    LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, PRIVILEGE_SET, PrivilegeCheck,
                    SE_PRIVILEGE_ENABLED, TOKEN_QUERY,
                },
                System::Threading::{GetCurrentProcess, OpenProcessToken},
            };
            unsafe {
                let mut token = std::ptr::null_mut();
                assert_ne!(
                    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
                    0
                );
                let mut luid = std::mem::zeroed();
                let name: Vec<u16> = "SeShutdownPrivilege\0".encode_utf16().collect();
                assert_ne!(
                    LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid),
                    0
                );
                let mut privileges = PRIVILEGE_SET {
                    PrivilegeCount: 1,
                    Control: 1, // PRIVILEGE_SET_ALL_NECESSARY
                    Privilege: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                let mut enabled = 0;
                let checked = PrivilegeCheck(token, &mut privileges, &mut enabled);
                CloseHandle(token);
                assert_ne!(checked, 0);
                assert_ne!(enabled, 0);
            }
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn rejected_pc_sleep_restores_tv_and_reports_original_error() {
        let calls = RefCell::new(Vec::new());
        let error = sleep_with_recovery(
            async {
                calls.borrow_mut().push("off");
                Ok(())
            },
            async {
                calls.borrow_mut().push("sleep");
                anyhow::bail!("sleep rejected")
            },
            async {
                calls.borrow_mut().push("restore");
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(*calls.borrow(), ["off", "sleep", "restore"]);
        assert!(error.to_string().contains("sleep rejected"));
    }

    #[tokio::test]
    async fn partial_tv_failure_restores_devices_and_does_not_suspend_pc() {
        let calls = RefCell::new(Vec::new());
        assert!(
            sleep_with_recovery(
                async {
                    calls.borrow_mut().push("off");
                    anyhow::bail!("partial OFF")
                },
                async {
                    calls.borrow_mut().push("sleep");
                    Ok(())
                },
                async {
                    calls.borrow_mut().push("restore");
                    Ok(())
                },
            )
            .await
            .is_err()
        );
        assert_eq!(*calls.borrow(), ["off", "restore"]);
    }

    #[tokio::test]
    async fn successful_sleep_waits_for_interactive_resume_to_restore_tv() {
        sleep_with_recovery(async { Ok(()) }, async { Ok(()) }, async {
            panic!("maintenance wake must not wake TV")
        })
        .await
        .unwrap();
    }
}
