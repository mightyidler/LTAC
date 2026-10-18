use super::*;
use std::sync::mpsc;

#[derive(Clone, Serialize)]
pub(super) struct SavedDevice {
    id: String,
    name: String,
    ip: String,
    mac: Option<String>,
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

pub(super) fn remember(path: &Path, config: &Config) -> Result<()> {
    let existing = configs(path)?;
    if let Some(previous) = existing.iter().find(|previous| {
        previous.ip == config.ip
            || (previous.device.is_some()
                && previous.device.as_ref().map(|d| &d.id) == config.device.as_ref().map(|d| &d.id))
    }) {
        return config.save(
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
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| config.ip.to_string()),
                ip: config.ip.to_string(),
                mac: config.mac.map(|m| m.to_string()),
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

type HotkeyRequest = (
    Option<String>,
    mpsc::SyncSender<std::result::Result<(), String>>,
);
pub(super) struct Hotkey {
    sender: mpsc::Sender<HotkeyRequest>,
}
impl Hotkey {
    pub(super) fn start(app: tauri::AppHandle) -> Self {
        let (sender, receiver) = mpsc::channel::<HotkeyRequest>();
        std::thread::spawn(move || {
            use windows_sys::Win32::UI::{
                Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey},
                WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, WM_HOTKEY},
            };
            let mut active = false;
            let mut previous: Option<(u32, u32)> = None;
            while !ltac::lifecycle::is_ending() {
                while let Ok((shortcut, reply)) = receiver.try_recv() {
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
                let mut message: MSG = unsafe { std::mem::zeroed() };
                while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) }
                    != 0
                {
                    if message.message == WM_HOTKEY {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app.state::<Desktop>();
                            if let Err(error) = sleep_all(&state, &app).await {
                                fail(&state, &app, &format!("sleep shortcut: {error:#}"));
                            }
                        });
                    }
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            if active {
                unsafe {
                    UnregisterHotKey(std::ptr::null_mut(), 1);
                }
            }
        });
        Self { sender }
    }
    pub(super) fn set(&self, shortcut: Option<String>) -> std::result::Result<(), String> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .send((shortcut, reply))
            .map_err(|_| "단축키를 등록하지 못했어요.")?;
        receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "단축키 등록 시간이 초과되었어요.")?
    }
}
fn parse_shortcut(shortcut: &str) -> std::result::Result<(u32, u32), String> {
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

async fn sleep_all(state: &Desktop, app: &tauri::AppHandle) -> Result<()> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| anyhow::anyhow!("다른 작업을 처리 중입니다."))?;
    if !read(state).map_err(anyhow::Error::msg)?.shortcut_enabled {
        return Ok(());
    }
    if state.service_mode {
        let remote = service_request(state, app, ltac::service_ipc::Request::Off)
            .await
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(remote.error.is_none(), "TV 절전 요청을 완료하지 못했어요.");
    } else {
        stop_watcher(state).await;
        let all = configs(&state.path)?;
        let mut tasks = tokio::task::JoinSet::new();
        for config in all {
            let mut journal = state.journal.clone();
            tasks.spawn(async move { controller::off(&config, &mut journal).await });
        }
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(result) = tasks.join_next().await {
                result??;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if let Err(error) = result
            .context("TV 절전 요청 시간이 초과되었어요.")
            .and_then(|r| r)
        {
            if read(state).map_err(anyhow::Error::msg)?.automatic {
                start_watcher(state, app, None).await?;
            }
            return Err(error);
        }
    }
    let suspended = tokio::task::spawn_blocking(suspend_pc).await?;
    if !state.service_mode && read(state).map_err(anyhow::Error::msg)?.automatic {
        start_watcher(state, app, None).await?;
    }
    suspended?;
    Ok(())
}

fn suspend_pc() -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, HANDLE, SetLastError},
        Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        },
        System::{
            Power::SetSuspendState,
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
        if AdjustTokenPrivileges(
            token.handle,
            0,
            &desired,
            std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
            &mut token.previous,
            std::ptr::null_mut(),
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
        if !SetSuspendState(false, false, false) {
            return Err(std::io::Error::last_os_error())
                .context("PC를 절전 상태로 전환하지 못했어요.");
        }
    }
    Ok(())
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
    let active = if enabled { shortcut.clone() } else { None };
    state.hotkey.set(active)?;
    let result: Result<()> = (|| {
        let path = preferences_path(&state.path);
        let prefs = Preferences {
            automatic: previous.automatic,
            shortcut_enabled: enabled,
            shortcut: shortcut.clone(),
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
            source_path: None,
            protected_key: None,
            certificate_sha256: None,
            broadcast: "192.0.2.255".parse().unwrap(),
            transport: ltac::config::Transport::Auto,
        }
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
