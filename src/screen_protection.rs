//! Panel-only control. Ownership is volatile and belongs to one authenticated
//! connection; neither reconnect nor process restart grants permission to wake.
use crate::{
    config::Config,
    controller,
    journal::Journal,
    ssap::{PowerState, Session},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

pub const DEFAULT_SECONDS: u32 = 90;
pub fn valid_seconds(value: u32) -> bool {
    (10..=1800).contains(&value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Idle,
    Active,
    Toggle,
    Disabled,
    Inhibited,
}

#[derive(Clone, Copy)]
pub struct Policy {
    pub enabled: bool,
    pub allowed: bool,
    pub shortcut_enabled: bool,
    pub timeout_seconds: u32,
}
#[derive(Clone, Copy)]
pub struct InputSample {
    pub token: u32,
    pub idle_ms: u32,
    pub allow_auto: bool,
}
/// Independent reasons to suppress panel control. A display notification must
/// never clear a session lock, suspension, or maintenance-only resume.
pub struct SessionPolicy {
    locked: bool,
    suspended: bool,
    display_active: bool,
    interactive: bool,
    ending: bool,
}
impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            locked: false,
            suspended: false,
            display_active: true,
            interactive: true,
            ending: false,
        }
    }
}
impl SessionPolicy {
    pub fn allowed(&self) -> bool {
        !self.locked && !self.suspended && self.display_active && self.interactive && !self.ending
    }
    pub fn apply(&mut self, event: crate::sync::Event) -> bool {
        use crate::sync::Event;
        match event {
            Event::SessionLocked => self.locked = true,
            Event::SessionUnlocked => {
                self.locked = false;
                self.interactive = true;
            }
            Event::DisplayOff | Event::DisplayDimmed => self.display_active = false,
            Event::DisplayOn => self.display_active = true,
            Event::Suspend => self.suspended = true,
            Event::ResumeAutomatic => {
                self.suspended = false;
                self.interactive = false;
            }
            Event::ResumeInteractive => {
                self.suspended = false;
                self.interactive = true;
                self.display_active = true;
            }
            Event::SessionEnding => self.ending = true,
            _ => return false,
        }
        true
    }
}
/// Edge detection only. It has no OS, wall-clock or network dependencies.
#[derive(Default)]
pub struct Sampler {
    last: Option<Signal>,
    suppress_input: Option<u32>,
    pending_active: Option<u64>,
    hotkey_until: Option<u64>,
}
/// One source is the existing exclusive interactive app. A new source drops
/// volatile ownership; duplicate/reordered IPC never toggles a panel twice.
#[derive(Default)]
pub struct Sequence {
    source: Option<u64>,
    latest: u64,
    retired: std::collections::VecDeque<u64>,
}
impl Sequence {
    pub fn accept(&mut self, source: u64, sequence: u64) -> Option<bool> {
        let new_owner = self.source != Some(source);
        if new_owner && self.retired.contains(&source) {
            return None;
        }
        if !new_owner && sequence <= self.latest {
            return None;
        }
        if new_owner && let Some(previous) = self.source {
            // More than the bounded IPC mailbox can have outstanding owners.
            if self.retired.len() == 32 {
                self.retired.pop_front();
            }
            self.retired.push_back(previous);
        }
        self.source = Some(source);
        self.latest = sequence;
        Some(new_owner)
    }
}
impl Sampler {
    pub fn reset(&mut self) {
        self.last = None;
        self.pending_active = None;
        self.hotkey_until = None;
    }
    pub fn update(
        &mut self,
        policy: Policy,
        input: Option<InputSample>,
        hotkey: bool,
        now_ms: u64,
    ) -> Option<Signal> {
        let signal = if !policy.enabled || !policy.allowed {
            Signal::Disabled
        } else if hotkey && policy.shortcut_enabled {
            self.suppress_input = input.map(|i| i.token);
            self.pending_active = None;
            self.hotkey_until = Some(now_ms.saturating_add(250));
            Signal::Toggle
        } else if let Some(input) = input {
            if input.idle_ms >= policy.timeout_seconds.saturating_mul(1000) {
                if input.allow_auto {
                    Signal::Idle
                } else {
                    Signal::Inhibited
                }
            } else if self.suppress_input == Some(input.token)
                || self
                    .hotkey_until
                    .is_some_and(|until| now_ms.saturating_sub(u64::from(input.idle_ms)) <= until)
            {
                // Key-up/modifier release from the entry shortcut is still
                // part of that gesture. A later mouse/key event restores.
                self.suppress_input = Some(input.token);
                return None;
            } else {
                let began = *self.pending_active.get_or_insert(now_ms);
                if input.idle_ms < 250 && now_ms.saturating_sub(began) < 250 {
                    return None;
                }
                self.pending_active = None;
                self.suppress_input = None;
                self.hotkey_until = None;
                Signal::Active
            }
        } else {
            Signal::Disabled
        };
        if self.last == Some(signal) && signal != Signal::Toggle {
            return None;
        }
        self.last = Some(signal);
        Some(signal)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Idle,
    AutoScreenOff,
    ManualScreenOff,
    Uncertain,
    SuspendedOrEnding,
}

#[derive(Default)]
pub struct Panel {
    pub mode: Mode,
    idle_attempted: bool,
}
impl Panel {
    pub fn invalidate(&mut self) {
        self.mode = Mode::SuspendedOrEnding;
        self.idle_attempted = true;
    }
    pub fn disconnected(&mut self) {
        self.mode = Mode::Uncertain;
    }
    /// Consume an idle episode even when its input check fails: no TV polling.
    fn intention(&mut self, signal: Signal) -> Option<(bool, Mode)> {
        match signal {
            Signal::Inhibited => {
                self.idle_attempted = false;
                None
            }
            Signal::Disabled => {
                self.invalidate();
                None
            }
            Signal::Active => {
                self.idle_attempted = false;
                if matches!(self.mode, Mode::AutoScreenOff | Mode::ManualScreenOff) {
                    Some((true, Mode::Idle))
                } else {
                    if matches!(self.mode, Mode::SuspendedOrEnding | Mode::Uncertain) {
                        self.mode = Mode::Idle;
                    }
                    None
                }
            }
            Signal::Idle => {
                if self.idle_attempted {
                    return None;
                }
                self.idle_attempted = true;
                (self.mode == Mode::Idle).then_some((false, Mode::AutoScreenOff))
            }
            Signal::Toggle => match self.mode {
                Mode::AutoScreenOff | Mode::ManualScreenOff => Some((true, Mode::Idle)),
                Mode::Idle => Some((false, Mode::ManualScreenOff)),
                Mode::Uncertain | Mode::SuspendedOrEnding => None,
            },
        }
    }
    pub async fn apply(
        &mut self,
        signal: Signal,
        config: &Config,
        session: &mut Option<Session>,
        journal: &mut Journal,
    ) -> anyhow::Result<()> {
        let Some((on, confirmed)) = self.intention(signal) else {
            return Ok(());
        };
        // Mark BEFORE the first await. Cancellation or lost ACK cannot establish
        // ownership and cannot cause a blind retry or subsequent ON.
        self.mode = Mode::Uncertain;
        let exchange = async {
            if session.is_none() {
                if on {
                    return Ok(false);
                } // Ownership never crosses a connection.
                *session = Some(controller::paired_session(config).await?);
            }
            let socket = session.as_mut().expect("Authenticated panel connection");
            let payload = socket.request(crate::ssap::POWER_STATE).await?;
            let actual = PowerState::from_payload(&payload);
            if (on && actual != PowerState::ScreenOff) || (!on && payload["state"] != "Active") {
                return Ok(false);
            }
            // Also protect against external input changes while the panel is off.
            if !controller::pc_input_active(socket, config.pc_hdmi, journal).await {
                return Ok(false);
            }
            if crate::lifecycle::is_ending() {
                return Ok(false);
            }
            socket.set_screen(on).await?;
            Ok::<_, anyhow::Error>(true)
        };
        let result = tokio::time::timeout(Duration::from_secs(3), exchange).await;
        let acknowledged = matches!(result, Ok(Ok(true)));
        let _ = journal.record("panel_request", json!({"ip":config.ip,"on":on,"signal":signal,"acknowledged":acknowledged,"physical_state":"not_observed"}));
        match result {
            Ok(Ok(true)) => {
                self.mode = confirmed;
                Ok(())
            }
            Ok(Ok(false)) => {
                self.mode = Mode::Idle;
                Ok(())
            }
            Ok(Err(error)) => {
                *session = None;
                Err(error)
            }
            Err(error) => {
                *session = None;
                Err(error.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    #[test]
    fn display_notifications_do_not_override_lock_suspend_or_maintenance_resume() {
        use crate::sync::Event;
        let mut session = SessionPolicy::default();
        assert!(session.allowed());
        session.apply(Event::SessionLocked);
        session.apply(Event::DisplayOn);
        assert!(!session.allowed());
        session.apply(Event::ResumeInteractive);
        assert!(
            !session.allowed(),
            "Interactive resume cannot unlock the session"
        );
        session.apply(Event::SessionUnlocked);
        assert!(session.allowed());
        session.apply(Event::Suspend);
        session.apply(Event::DisplayOn);
        assert!(!session.allowed());
        session.apply(Event::ResumeAutomatic);
        session.apply(Event::DisplayOn);
        assert!(
            !session.allowed(),
            "Maintenance wakes cannot arm panel control"
        );
        session.apply(Event::ResumeInteractive);
        assert!(session.allowed());
        session.apply(Event::DisplayDimmed);
        assert!(!session.allowed());
        session.apply(Event::DisplayOn);
        assert!(session.allowed());
        session.apply(Event::SessionEnding);
        session.apply(Event::ResumeInteractive);
        session.apply(Event::SessionUnlocked);
        assert!(!session.allowed(), "Termination is irreversible");
    }
    #[test]
    fn fullscreen_inhibits_blank_but_does_not_hide_resumed_input() {
        let mut sampler = Sampler::default();
        let policy = Policy {
            enabled: true,
            allowed: true,
            shortcut_enabled: true,
            timeout_seconds: 90,
        };
        let input = |token, idle_ms, allow_auto| {
            Some(InputSample {
                token,
                idle_ms,
                allow_auto,
            })
        };
        assert_eq!(
            sampler.update(policy, input(1, 90_000, true), false, 0),
            Some(Signal::Idle)
        );
        assert_eq!(sampler.update(policy, input(2, 0, false), false, 500), None);
        assert_eq!(
            sampler.update(policy, input(3, 0, false), false, 1000),
            Some(Signal::Active)
        );
        assert_eq!(
            sampler.update(policy, input(3, 90_000, false), false, 91_000),
            Some(Signal::Inhibited)
        );
        assert_eq!(
            sampler.update(policy, input(3, 90_500, false), false, 91_500),
            None
        );
    }
    #[test]
    fn duplicate_or_reordered_ipc_is_ignored_and_restart_discards_ownership() {
        let mut sequence = Sequence::default();
        assert_eq!(sequence.accept(10, 1), Some(true));
        assert_eq!(sequence.accept(10, 1), None);
        assert_eq!(sequence.accept(10, 3), Some(false));
        assert_eq!(sequence.accept(10, 2), None);
        assert_eq!(sequence.accept(11, 1), Some(true));
        assert_eq!(
            sequence.accept(10, 4),
            None,
            "A delayed request from the previous app cannot reclaim ownership"
        );
        assert_eq!(sequence.accept(11, 2), Some(false));
        let mut panel = Panel::default();
        assert_eq!(panel.intention(Signal::Active), None);
    }
    #[test]
    fn ten_second_delay_fires_at_threshold_and_restores_on_input() {
        let mut sampler = Sampler::default();
        let policy = Policy {
            enabled: true,
            allowed: true,
            shortcut_enabled: false,
            timeout_seconds: 10,
        };
        let input = |token, idle_ms| {
            Some(InputSample {
                token,
                idle_ms,
                allow_auto: true,
            })
        };
        assert_eq!(
            sampler.update(policy, input(1, 9_999), false, 9_999),
            Some(Signal::Active)
        );
        assert_eq!(
            sampler.update(policy, input(1, 10_000), false, 10_000),
            Some(Signal::Idle)
        );
        assert_eq!(
            sampler.update(policy, input(1, 10_001), false, 10_001),
            None
        );
        assert_eq!(sampler.update(policy, input(2, 0), false, 10_100), None);
        assert_eq!(
            sampler.update(policy, input(2, 250), false, 10_350),
            Some(Signal::Active)
        );
    }
    #[test]
    fn fake_input_clock_threshold_changes_inhibition_and_hotkey_precedence() {
        let mut s = Sampler::default();
        let mut p = Policy {
            enabled: true,
            allowed: true,
            shortcut_enabled: true,
            timeout_seconds: 90,
        };
        let input = |token, idle_ms, allow_auto| {
            Some(InputSample {
                token,
                idle_ms,
                allow_auto,
            })
        };
        assert_eq!(
            s.update(p, input(1, 89_999, true), false, 0),
            Some(Signal::Active)
        );
        assert_eq!(
            s.update(p, input(1, 90_000, true), false, 500),
            Some(Signal::Idle)
        );
        assert_eq!(s.update(p, input(1, 100_000, true), false, 1000), None);
        assert_eq!(s.update(p, input(2, 0, true), false, 1500), None);
        assert_eq!(
            s.update(p, input(2, 10, true), true, 1510),
            Some(Signal::Toggle)
        );
        assert_eq!(s.update(p, input(2, 1000, true), false, 2500), None);
        assert_eq!(s.update(p, input(3, 0, true), false, 2600), None);
        assert_eq!(
            s.update(p, input(4, 0, true), false, 3100),
            Some(Signal::Active),
            "Continuous mouse input must still wake promptly"
        );
        assert_eq!(
            s.update(p, input(4, 100_000, false), false, 4000),
            Some(Signal::Inhibited)
        );
        assert_eq!(s.update(p, input(4, 101_000, false), false, 4500), None);
        p.timeout_seconds = 180;
        assert_eq!(
            s.update(p, input(4, 101_000, true), false, 5000),
            Some(Signal::Active)
        );
        p.timeout_seconds = 30;
        assert_eq!(
            s.update(p, input(4, 101_000, true), false, 5500),
            Some(Signal::Idle)
        );
        p.allowed = false;
        assert_eq!(
            s.update(p, input(5, 0, true), true, 6000),
            Some(Signal::Disabled)
        );
        p.enabled = false;
        p.allowed = true;
        assert_eq!(s.update(p, input(5, 0, true), true, 6500), None);
    }

    fn config(port: Option<u8>) -> Config {
        let mut config: Config = serde_json::from_value(
            json!({"ip":"127.0.0.1","broadcast":"127.0.0.1","transport":"ws"}),
        )
        .unwrap();
        config.pc_hdmi = port;
        config
    }
    async fn exchange(
        peer: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        uri: &str,
        payload: serde_json::Value,
    ) {
        let frame = peer.next().await.unwrap().unwrap();
        let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(frame["uri"], uri);
        assert_eq!(frame["payload"], json!({}));
        peer.send(Message::Text(
            json!({"id":frame["id"],"type":"response","payload":payload})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn mocked_auto_and_manual_panel_control_reuses_connection_without_power_off_or_wol() {
        for initial in [Signal::Idle, Signal::Toggle] {
            let (session, mut peer) = crate::ssap::tests::peer().await;
            let remote = tokio::spawn(async move {
                for on in [false, true] {
                    exchange(
                        &mut peer,
                        crate::ssap::POWER_STATE,
                        json!({"state":if on {"Screen Off"} else {"Active"}}),
                    )
                    .await;
                    exchange(
                        &mut peer,
                        crate::ssap::FOREGROUND_APP,
                        json!({"returnValue":true,"appId":"com.webos.app.hdmi2"}),
                    )
                    .await;
                    exchange(
                        &mut peer,
                        if on {
                            crate::ssap::SCREEN_ON
                        } else {
                            crate::ssap::SCREEN_OFF
                        },
                        json!({"returnValue":true}),
                    )
                    .await;
                }
                while let Some(Ok(frame)) = peer.next().await {
                    assert!(!frame.is_text(), "Duplicate panel request");
                }
            });
            let dir = tempfile::tempdir().unwrap();
            let mut journal = Journal::open(&dir.path().join("panel.jsonl")).unwrap();
            let mut socket = Some(session);
            let mut panel = Panel::default();
            panel
                .apply(initial, &config(Some(2)), &mut socket, &mut journal)
                .await
                .unwrap();
            panel
                .apply(Signal::Idle, &config(Some(2)), &mut socket, &mut journal)
                .await
                .unwrap();
            panel
                .apply(Signal::Active, &config(Some(2)), &mut socket, &mut journal)
                .await
                .unwrap();
            panel
                .apply(Signal::Active, &config(Some(2)), &mut socket, &mut journal)
                .await
                .unwrap();
            drop(socket);
            remote.await.unwrap();
        }
    }
    #[tokio::test]
    async fn other_inputs_external_screen_off_and_unknown_state_never_receive_panel_commands() {
        for (power, app, port) in [
            ("Active", "netflix", Some(2)),
            ("Active", "com.webos.app.hdmi1", Some(2)),
            ("Active", "", None),
            ("Screen Off", "", Some(2)),
            ("Screen Saver", "", Some(2)),
            ("Unknown", "", Some(2)),
            ("Power Off", "", Some(2)),
        ] {
            let (session, mut peer) = crate::ssap::tests::peer().await;
            let remote = tokio::spawn(async move {
                exchange(&mut peer, crate::ssap::POWER_STATE, json!({"state":power})).await;
                if power == "Active" && port.is_some() {
                    exchange(
                        &mut peer,
                        crate::ssap::FOREGROUND_APP,
                        json!({"returnValue":true,"appId":app}),
                    )
                    .await;
                }
                while let Some(Ok(frame)) = peer.next().await {
                    assert!(!frame.is_text(), "Unsafe panel command or polling");
                }
            });
            let dir = tempfile::tempdir().unwrap();
            let mut journal = Journal::open(&dir.path().join("panel.jsonl")).unwrap();
            let mut socket = Some(session);
            let mut panel = Panel::default();
            for signal in [Signal::Idle, Signal::Idle, Signal::Active, Signal::Active] {
                panel
                    .apply(signal, &config(port), &mut socket, &mut journal)
                    .await
                    .unwrap();
            }
            drop(socket);
            remote.await.unwrap();
        }
    }
    #[tokio::test]
    async fn panel_permission_rejection_reports_failure_without_ownership_or_retry() {
        let (session, mut peer) = crate::ssap::tests::peer().await;
        let remote = tokio::spawn(async move {
            exchange(
                &mut peer,
                crate::ssap::POWER_STATE,
                json!({"state":"Active"}),
            )
            .await;
            exchange(
                &mut peer,
                crate::ssap::FOREGROUND_APP,
                json!({"returnValue":true,"appId":"com.webos.app.hdmi2"}),
            )
            .await;
            let frame = peer.next().await.unwrap().unwrap();
            let request: serde_json::Value =
                serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(request["uri"], crate::ssap::SCREEN_OFF);
            peer.send(Message::Text(
                json!({"id":request["id"],"type":"error","error":"401 insufficient permissions"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            while let Some(Ok(frame)) = peer.next().await {
                assert!(
                    !frame.is_text(),
                    "Rejected panel OFF must not be retried or restored"
                );
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(&dir.path().join("panel.jsonl")).unwrap();
        let mut socket = Some(session);
        let mut panel = Panel::default();
        let error = panel
            .apply(Signal::Idle, &config(Some(2)), &mut socket, &mut journal)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("401 insufficient permissions"));
        assert_eq!(panel.mode, Mode::Uncertain);
        assert!(socket.is_none());
        for signal in [
            Signal::Idle,
            Signal::Toggle,
            Signal::Disabled,
            Signal::Active,
        ] {
            panel
                .apply(signal, &config(Some(2)), &mut socket, &mut journal)
                .await
                .unwrap();
        }
        assert_eq!(panel.mode, Mode::Idle);
        assert!(socket.is_none());
        remote.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn missing_ack_and_cancellation_never_create_wake_ownership() {
        for cancel in [false, true] {
            let (session, mut peer) = crate::ssap::tests::peer().await;
            let (sent, received) = tokio::sync::oneshot::channel();
            let remote = tokio::spawn(async move {
                exchange(
                    &mut peer,
                    crate::ssap::POWER_STATE,
                    json!({"state":"Active"}),
                )
                .await;
                exchange(
                    &mut peer,
                    crate::ssap::FOREGROUND_APP,
                    json!({"returnValue":true,"appId":"com.webos.app.hdmi2"}),
                )
                .await;
                let frame = peer.next().await.unwrap().unwrap();
                assert!(frame.to_text().unwrap().contains(crate::ssap::SCREEN_OFF));
                sent.send(()).unwrap();
                while let Some(Ok(frame)) = peer.next().await {
                    assert!(
                        !frame.is_text(),
                        "Uncertain OFF must not be retried or restored"
                    );
                }
            });
            let dir = tempfile::tempdir().unwrap();
            let mut journal = Journal::open(&dir.path().join("panel.jsonl")).unwrap();
            let mut socket = Some(session);
            let mut panel = Panel::default();
            if cancel {
                let config = config(Some(2));
                let operation = panel.apply(Signal::Idle, &config, &mut socket, &mut journal);
                tokio::pin!(operation);
                tokio::select! { _ = received => {}, result = &mut operation => panic!("Unexpected completion: {result:?}") }
            } else {
                assert!(
                    panel
                        .apply(Signal::Idle, &config(Some(2)), &mut socket, &mut journal)
                        .await
                        .is_err()
                );
                received.await.unwrap();
            }
            assert_eq!(panel.mode, Mode::Uncertain);
            panel
                .apply(Signal::Idle, &config(Some(2)), &mut socket, &mut journal)
                .await
                .unwrap();
            panel
                .apply(Signal::Toggle, &config(Some(2)), &mut socket, &mut journal)
                .await
                .unwrap();
            panel.invalidate();
            panel
                .apply(Signal::Active, &config(Some(2)), &mut socket, &mut journal)
                .await
                .unwrap();
            drop(socket);
            remote.await.unwrap();
        }
    }
    #[test]
    fn transitions_are_single_shot_and_input_restores_both_entry_modes() {
        let mut p = Panel::default();
        assert_eq!(
            p.intention(Signal::Idle),
            Some((false, Mode::AutoScreenOff))
        );
        assert_eq!(p.intention(Signal::Idle), None);
        p.mode = Mode::AutoScreenOff;
        assert_eq!(p.intention(Signal::Toggle), Some((true, Mode::Idle)));
        p.mode = Mode::ManualScreenOff;
        assert_eq!(p.intention(Signal::Active), Some((true, Mode::Idle)));
        assert_eq!(p.intention(Signal::Toggle), Some((true, Mode::Idle)));
        p.invalidate();
        assert_eq!(p.intention(Signal::Toggle), None);
        assert_eq!(p.intention(Signal::Idle), None);
        assert_eq!(p.intention(Signal::Active), None);
        assert_eq!(p.mode, Mode::Idle);
    }

    #[test]
    fn entry_shortcut_release_is_ignored_but_later_mouse_or_key_restores() {
        let p = Policy {
            enabled: true,
            allowed: true,
            shortcut_enabled: true,
            timeout_seconds: 30,
        };
        let sample = |token, idle_ms| {
            Some(InputSample {
                token,
                idle_ms,
                allow_auto: false,
            })
        };
        for later_token in [3, 4] {
            let mut sampler = Sampler::default();
            assert_eq!(
                sampler.update(p, sample(1, 0), true, 1000),
                Some(Signal::Toggle)
            );
            // A key-up 40ms after entry is first sampled at 1500ms.
            assert_eq!(sampler.update(p, sample(2, 460), false, 1500), None);
            assert_eq!(sampler.update(p, sample(2, 960), false, 2000), None);
            assert_eq!(sampler.update(p, sample(later_token, 0), false, 2500), None);
            assert_eq!(
                sampler.update(p, sample(later_token, 500), false, 3000),
                Some(Signal::Active)
            );
            assert_eq!(
                sampler.update(p, sample(later_token, 1000), false, 3500),
                None
            );
        }
    }
    #[test]
    fn restart_disconnect_disable_and_boundaries() {
        let mut p = Panel::default();
        assert_eq!(p.intention(Signal::Active), None);
        p.mode = Mode::AutoScreenOff;
        p.disconnected();
        assert_eq!(p.intention(Signal::Toggle), None);
        assert_eq!(p.intention(Signal::Active), None);
        p.mode = Mode::AutoScreenOff;
        assert_eq!(p.intention(Signal::Disabled), None);
        assert_eq!(p.intention(Signal::Toggle), None);
        assert!(valid_seconds(10) && valid_seconds(30) && valid_seconds(90) && valid_seconds(1800));
        assert!(!valid_seconds(9) && !valid_seconds(1801));
    }
}
