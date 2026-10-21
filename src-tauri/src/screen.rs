//! Uses the existing tray app's interactive session, never service Session 0.
use super::*;
use ltac::screen_protection::{InputSample, Policy, Sampler, Signal};
use std::sync::atomic::Ordering;

fn failure_message(error: &anyhow::Error) -> &'static str {
    let detail = format!("{error:#}").to_ascii_lowercase();
    if detail.contains("insufficient permissions") || detail.contains("requires pairing") {
        "화면 보호 권한이 없습니다. TV를 다시 연결해 권한을 승인해주세요."
    } else {
        "화면 보호 요청을 완료하지 못했어요. TV와 연결 상태를 확인해주세요."
    }
}

pub(super) fn native_event(state: &Desktop, event: Event) {
    let Ok(mut session) = state.screen_session.lock() else {
        return;
    };
    if !session.apply(event) {
        return;
    }
    state
        .screen_allowed
        .store(session.allowed(), Ordering::Release);
    drop(session);
    state.screen_epoch.fetch_add(1, Ordering::AcqRel);
    state.screen_changed.notify_one();
}

fn input() -> Option<(u32, u32, bool)> {
    use windows_sys::Win32::{
        System::{
            Power::{CallNtPowerInformation, ES_DISPLAY_REQUIRED, SystemExecutionState},
            SystemInformation::GetTickCount,
        },
        UI::{
            Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
            Shell::{QUNS_ACCEPTS_NOTIFICATIONS, QUNS_QUIET_TIME, SHQueryUserNotificationState},
        },
    };
    unsafe {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut info) == 0 {
            return None;
        }
        let mut notification = 0;
        let mut execution = 0u32;
        let available = SHQueryUserNotificationState(&mut notification) >= 0
            && matches!(notification, QUNS_ACCEPTS_NOTIFICATIONS | QUNS_QUIET_TIME)
            && CallNtPowerInformation(
                SystemExecutionState,
                std::ptr::null(),
                0,
                (&mut execution as *mut u32).cast(),
                std::mem::size_of::<u32>() as u32,
            ) == 0
            && execution & ES_DISPLAY_REQUIRED == 0;
        Some((
            info.dwTime,
            GetTickCount().wrapping_sub(info.dwTime),
            available,
        ))
    }
}

async fn send(state: &Desktop, signal: Signal) -> Result<()> {
    if signal != Signal::Disabled
        && (!state.screen_allowed.load(Ordering::Acquire) || ltac::lifecycle::is_ending())
    {
        return Ok(());
    }
    if state.service_mode {
        let sequence = state.screen_sequence.fetch_add(1, Ordering::AcqRel);
        ltac::service_ipc::request(ltac::service_ipc::Request::ScreenProtection {
            signal,
            sequence,
            source: state.screen_source,
        })
        .await?;
    } else if let Ok(route) = state.events.read()
        && let Some(sender) = &route.watcher
    {
        let _ = sender.send(Notice {
            preceding: None,
            event: Event::ScreenProtection(signal),
            suspend_ack: None,
        });
    }
    Ok(())
}

pub(super) fn start(app: tauri::AppHandle, mut toggles: tokio::sync::mpsc::Receiver<()>) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Desktop>();
        let mut sampler = Sampler::default();
        let clock = std::time::Instant::now();
        let mut observed_epoch = u64::MAX;
        loop {
            let snapshot = match read(&state) {
                Ok(s) => s,
                Err(_) => return,
            };
            let enabled = snapshot.screen_enabled
                && snapshot.configured
                && state.screen_allowed.load(Ordering::Acquire);
            let toggled = tokio::select! {
                biased;
                value = toggles.recv() => { if value.is_none() { return; } true },
                _ = state.screen_changed.notified() => false,
                _ = async { if enabled { tokio::time::sleep(Duration::from_millis(500)).await; } else { std::future::pending::<()>().await; } } => false,
            };
            let snapshot = match read(&state) {
                Ok(s) => s,
                Err(_) => return,
            };
            let epoch = state.screen_epoch.load(Ordering::Acquire);
            if observed_epoch != epoch {
                sampler.reset();
                observed_epoch = epoch;
            }
            let policy = Policy {
                enabled: snapshot.screen_enabled && snapshot.configured,
                allowed: state.screen_allowed.load(Ordering::Acquire)
                    && !ltac::lifecycle::is_ending(),
                shortcut_enabled: snapshot.screen_shortcut_enabled,
                timeout_seconds: snapshot.screen_timeout_seconds,
            };
            let sample = input().map(|(token, idle_ms, allow_auto)| InputSample {
                token,
                idle_ms,
                allow_auto,
            });
            let Some(signal) =
                sampler.update(policy, sample, toggled, clock.elapsed().as_millis() as u64)
            else {
                continue;
            };
            let result = {
                let operation = send(&state, signal);
                tokio::pin!(operation);
                tokio::select! {
                    biased;
                    _ = state.screen_changed.notified() => { sampler.reset(); continue; },
                    result = &mut operation => result,
                }
            };
            if let Err(error) = result {
                let _ = state
                    .journal
                    .clone()
                    .record("panel_ipc_failed", json!({"error":error.to_string()}));
                // Panel failures must reach the same error surface as user
                // commands; logging alone made a rejected feature look inert.
                update(&state.view, &app, |s| {
                    s.error = Some(failure_message(&error).into());
                });
            }
        }
    });
}

#[tauri::command]
pub(super) async fn set_screen_protection(
    state: State<'_, Desktop>,
    app: tauri::AppHandle,
    enabled: bool,
    timeout_seconds: u32,
    shortcut_enabled: bool,
    shortcut: Option<String>,
) -> std::result::Result<Snapshot, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "다른 작업을 처리 중입니다.")?;
    if !ltac::screen_protection::valid_seconds(timeout_seconds) {
        return Err("화면 보호 시간은 10초에서 30분 사이로 지정해주세요.".into());
    }
    if let Some(value) = &shortcut {
        ui::parse_shortcut(value)?;
    }
    let previous = read(&state)?;
    if let (Some(a), Some(b)) = (&shortcut, &previous.shortcut)
        && ui::parse_shortcut(a)? == ui::parse_shortcut(b)?
    {
        return Err("이미 다른 LTAC 단축키에 지정된 조합입니다.".into());
    }
    let mut prefs = Preferences::from(&previous);
    prefs.screen_enabled = enabled;
    prefs.screen_timeout_seconds = timeout_seconds;
    prefs.screen_shortcut_enabled = shortcut_enabled;
    prefs.screen_shortcut = shortcut.clone();
    state.screen_hotkey.set(if enabled && shortcut_enabled {
        shortcut.clone()
    } else {
        None
    })?;
    if write_preferences(&state.path, &prefs).is_err() {
        let _ = state.screen_hotkey.set(
            if previous.screen_enabled && previous.screen_shortcut_enabled {
                previous.screen_shortcut.clone()
            } else {
                None
            },
        );
        return Err("설정을 저장하지 못했어요.".into());
    }
    update(&state.view, &app, |s| {
        s.screen_enabled = enabled;
        s.screen_timeout_seconds = timeout_seconds;
        s.screen_shortcut_enabled = shortcut_enabled;
        s.screen_shortcut = shortcut;
        s.error = None;
    });
    if !enabled {
        let _ = send(&state, Signal::Disabled).await;
    }
    if !state.service_mode && !previous.automatic && previous.screen_enabled != enabled {
        stop_watcher(&state).await;
        if (enabled || previous.automatic)
            && previous.configured
            && let Err(error) = start_watcher(&state, &app, None).await
        {
            let _ = write_preferences(&state.path, &Preferences::from(&previous));
            let _ = state.screen_hotkey.set(
                if previous.screen_enabled && previous.screen_shortcut_enabled {
                    previous.screen_shortcut.clone()
                } else {
                    None
                },
            );
            update(&state.view, &app, |s| {
                s.screen_enabled = previous.screen_enabled;
                s.screen_timeout_seconds = previous.screen_timeout_seconds;
                s.screen_shortcut_enabled = previous.screen_shortcut_enabled;
                s.screen_shortcut = previous.screen_shortcut.clone();
            });
            return Err(error.to_string());
        }
    }
    state.screen_epoch.fetch_add(1, Ordering::AcqRel);
    state.screen_changed.notify_one();
    read(&state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panel_permission_and_transport_failures_have_actionable_ui_messages() {
        let denied = anyhow::anyhow!("TV rejected request: 401 insufficient permissions");
        assert!(failure_message(&denied).contains("권한"));
        assert!(failure_message(&denied).contains("다시 연결"));
        let pairing = anyhow::Error::new(ltac::ssap::Error::PairingRequired);
        assert_eq!(failure_message(&pairing), failure_message(&denied));
        assert!(
            failure_message(&anyhow::anyhow!("Service request timed out")).contains("연결 상태")
        );
    }
    #[test]
    fn old_preferences_default_off_and_new_settings_survive_other_changes() {
        let old: Preferences =
            serde_json::from_str(r#"{"automatic":true,"shortcut":"F8"}"#).unwrap();
        assert!(!old.screen_enabled && !old.screen_shortcut_enabled);
        assert_eq!(old.screen_timeout_seconds, 90);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        write_preferences(
            &path,
            &Preferences {
                screen_enabled: true,
                screen_timeout_seconds: 91,
                screen_shortcut_enabled: true,
                screen_shortcut: Some("Ctrl+F9".into()),
                ..Default::default()
            },
        )
        .unwrap();
        save_preferences(&path, false).unwrap();
        let (view, _) = initial_view(&path, false, false).unwrap();
        assert!(view.screen_enabled && view.screen_shortcut_enabled);
        assert_eq!(view.screen_timeout_seconds, 91);
        assert_eq!(view.screen_shortcut.as_deref(), Some("Ctrl+F9"));
        assert!(!view.automatic);
    }

    #[test]
    fn ten_second_delay_survives_preference_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        write_preferences(
            &path,
            &Preferences {
                screen_enabled: true,
                screen_timeout_seconds: 10,
                ..Default::default()
            },
        )
        .unwrap();
        let (view, _) = initial_view(&path, false, false).unwrap();
        assert!(view.screen_enabled);
        assert_eq!(view.screen_timeout_seconds, 10);
    }
}
