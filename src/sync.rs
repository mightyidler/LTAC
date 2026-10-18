use crate::{config::Config, controller, journal::Journal};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    time::{sleep, timeout},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    NetworkChanged,
    SessionLocked,
    SessionUnlocked,
    DisplayOn,
    DisplayOff,
    DisplayDimmed,
    Suspend,
    ResumeAutomatic,
    ResumeInteractive,
    SessionEnding,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    On,
    Off,
}

#[derive(Default)]
pub struct State {
    suspended: bool,
    ending: bool,
    desired: Option<Target>,
}
impl State {
    pub fn desired_target(&self) -> Option<Target> {
        self.desired
    }
    pub fn set_desired(&mut self, target: Target) {
        self.desired = Some(target);
    }
    pub fn apply(&mut self, event: Event) -> Option<Target> {
        if self.ending {
            return None;
        }
        let desired = match event {
            Event::SessionEnding => {
                self.ending = true;
                Some(Target::Off)
            }
            Event::Suspend => {
                self.suspended = true;
                Some(Target::Off)
            }
            Event::ResumeAutomatic => {
                self.suspended = false;
                // Maintenance wakes must not turn on a TV with nobody present.
                None
            }
            Event::ResumeInteractive => {
                self.suspended = false;
                Some(Target::On)
            }
            Event::DisplayOn if !self.suspended => Some(Target::On),
            Event::DisplayOff => Some(Target::Off),
            Event::DisplayOn
            | Event::DisplayDimmed
            | Event::NetworkChanged
            | Event::SessionLocked
            | Event::SessionUnlocked => None,
        };
        if let Some(desired) = desired
            && self.desired != Some(desired)
        {
            self.desired = Some(desired);
            return Some(desired);
        }
        None
    }
}

pub struct Notice {
    pub event: Event,
    // The native suspend callback can wait briefly for an OFF attempt before
    // returning. This is never used for ordinary display state changes.
    pub suspend_ack: Option<std::sync::mpsc::SyncSender<()>>,
}
#[derive(Clone)]
struct Action {
    target: Target,
    urgent: bool,
    ack: Option<std::sync::mpsc::SyncSender<()>>,
}
impl Action {
    fn acknowledge(&self) {
        if let Some(ack) = &self.ack {
            let _ = ack.try_send(());
        }
    }
}

pub struct Initial {
    pub target: Target,
    pub session: Option<crate::ssap::Session>,
}

pub async fn watch(config: &Config, journal: &mut Journal, observe_only: bool) -> Result<()> {
    watch_with_initial(config, journal, observe_only, None).await
}

pub async fn watch_with_initial(
    config: &Config,
    journal: &mut Journal,
    observe_only: bool,
    initial: Option<Initial>,
) -> Result<()> {
    watch_with_ready(config, journal, observe_only, initial, None).await
}

pub async fn watch_with_ready(
    config: &Config,
    journal: &mut Journal,
    observe_only: bool,
    initial: Option<Initial>,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (config, journal, observe_only, initial, ready);
        bail!("Windows events require Windows");
    }
    #[cfg(windows)]
    {
        if !observe_only {
            config.key()?;
            anyhow::ensure!(
                config.mac.is_some(),
                "TV network address unavailable; verify ON support before enabling sync"
            );
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        let _source = crate::windows_events::Source::start(sender, !observe_only).await?;
        journal.record("watch_started", json!({"observe_only":observe_only}))?;
        if let Some(ready) = ready {
            let _ = ready.send(());
        }
        println!(
            "Windows power events active (observe_only={observe_only}). Ctrl+C stops watching."
        );
        run_with_initial(config, journal, receiver, observe_only, initial).await
    }
}

#[cfg(test)]
async fn run(
    config: &Config,
    journal: &mut Journal,
    receiver: mpsc::UnboundedReceiver<Notice>,
    observe_only: bool,
) -> Result<()> {
    run_with_initial(config, journal, receiver, observe_only, None).await
}

pub async fn run_with_initial(
    config: &Config,
    journal: &mut Journal,
    mut receiver: mpsc::UnboundedReceiver<Notice>,
    observe_only: bool,
    initial: Option<Initial>,
) -> Result<()> {
    let (sender, mut targets) = watch::channel::<Option<Action>>(None);
    let (network_sender, mut networks) = watch::channel(0u64);
    let mut event_log = journal.clone();
    let mut suppress_initial_display = initial.is_some();
    let mut applied = initial.as_ref().map(|i| i.target);
    let mut prepared = initial.and_then(|i| i.session);
    let collector = tokio::spawn(async move {
        let mut state = State::default();
        while let Some(notice) = receiver.recv().await {
            // Logging failure must not disable power handling during shutdown.
            let _ = event_log.record("windows_event", json!({"event":notice.event}));
            if notice.event == Event::NetworkChanged && !observe_only {
                network_sender.send_modify(|generation| *generation = generation.wrapping_add(1));
            }
            println!("Windows: {:?}", notice.event);
            let desired = state.apply(notice.event);
            if suppress_initial_display
                && matches!(
                    notice.event,
                    Event::DisplayOn | Event::DisplayOff | Event::DisplayDimmed
                )
            {
                // Reopening the event source after a manual command emits the
                // current display state once. It must not undo manual OFF.
                suppress_initial_display = false;
                event_log.record("initial_display_reconciled", json!({"event":notice.event}))?;
                continue;
            }
            if observe_only {
                if let Some(ack) = notice.suspend_ack {
                    let _ = ack.try_send(());
                }
            } else if let Some(target) = desired.or_else(|| {
                matches!(notice.event, Event::Suspend | Event::SessionEnding).then_some(Target::Off)
            }) {
                sender.send_replace(Some(Action {
                    target,
                    urgent: matches!(notice.event, Event::Suspend | Event::SessionEnding),
                    ack: notice.suspend_ack,
                }));
            } else if let Some(ack) = notice.suspend_ack {
                let _ = ack.try_send(());
            }
        }
        Ok::<_, anyhow::Error>(())
    });
    // Abort the collector on Ctrl+C/cancellation as well as on ordinary errors.
    struct AbortOnDrop(tokio::task::AbortHandle);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _collector_guard = AbortOnDrop(collector.abort_handle());
    if observe_only {
        return collector.await.context("Event collector task")?;
    }

    loop {
        tokio::select! {
            changed = targets.changed() => { if changed.is_err() { break; } }
            changed = networks.changed() => {
                if changed.is_err() { break; }
                if config.device.is_none() { continue; }
                // Coalesce NIC notifications, then search once. A power event
                // cancels this read-only recovery so suspend OFF is never delayed.
                let recovery = async { sleep(Duration::from_secs(2)).await; crate::connection::recover(config).await };
                tokio::select! {
                    changed = targets.changed() => { changed.context("Windows event source ended")?; },
                    result = timeout(Duration::from_secs(15), recovery) => {
                        match result {
                            Ok(Ok((_, session))) => { prepared = Some(session); journal.record("network_recovered", json!({"identity_matched":true}))?; },
                            Ok(Err(error)) => journal.record("network_recovery_unavailable", json!({"error":format!("{error:#}")}))?,
                            Err(error) => journal.record("network_recovery_unavailable", json!({"error":error.to_string()}))?,
                        }
                        continue;
                    }
                }
            }
            result = async {
                match &mut prepared {
                    Some(session) => session.idle_step().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Err(e) = result {
                    prepared = None;
                    journal.record("idle_connection_closed", json!({"error":e.to_string()}))?;
                }
                continue;
            }
        }
        let Some(mut action) = targets.borrow_and_update().clone() else {
            continue;
        };
        'latest: loop {
            // Coalesce short display changes. Suspend gets its own strict budget.
            let delay = match (action.target, action.urgent) {
                (_, true) => Duration::ZERO,
                (Target::Off, false) => Duration::from_millis(1200),
                (Target::On, false) => Duration::from_millis(200),
            };
            tokio::select! {
                _ = sleep(delay) => {},
                changed = targets.changed() => {
                    action.acknowledge();
                    changed.context("Windows event source ended")?;
                    action = targets.borrow_and_update().clone().context("Missing target")?;
                    continue 'latest;
                }
            }
            if applied == Some(action.target) {
                action.acknowledge();
                journal.record("sync_duplicate_skipped", json!({"target":action.target}))?;
                break;
            }
            journal.record(
                "sync_action_started",
                json!({"target":action.target, "suspend":action.urgent}),
            )?;
            let target = action.target;
            let urgent = action.urgent;
            let mut action_log = journal.clone();
            let mut cached = prepared.take();
            let latest_config = config
                .source_path
                .as_deref()
                .and_then(|path| Config::load(path).ok())
                .unwrap_or_else(|| config.clone());
            let config = &latest_config;
            let operation = async {
                if target == Target::On {
                    controller::wake_session(config, &mut action_log, Duration::from_secs(45))
                        .await
                        .map(Some)
                } else {
                    let limit = if urgent {
                        Duration::from_millis(1400)
                    } else {
                        Duration::from_secs(15)
                    };
                    let turn_off = async {
                        if let Some(session) = &mut cached {
                            controller::off_cached(session, &mut action_log).await
                        } else {
                            controller::off(config, &mut action_log).await
                        }
                    };
                    timeout(limit, turn_off)
                        .await
                        .context("OFF budget expired; physical OFF remains unconfirmed")??;
                    Ok(None)
                }
            };
            tokio::pin!(operation);
            tokio::select! {
                biased;
                changed = targets.changed() => {
                    // A terminal or opposite event takes precedence over a
                    // simultaneously completed ON future.
                    applied = None;
                    action.acknowledge();
                    let _ = journal.record("sync_action_superseded", json!({"target":target}));
                    changed.context("Windows event source ended")?;
                    action = targets.borrow_and_update().clone().context("Missing target")?;
                }
                result = &mut operation => {
                    action.acknowledge();
                    journal.record("sync_action_finished", json!({"target":target, "success":result.is_ok(), "error":result.as_ref().err().map(|e|format!("{e:#}"))}))?;
                    match result {
                        Ok(session) => { prepared = session; applied = Some(target); },
                        Err(e) => { applied = None; eprintln!("Sync {target:?} failed: {e:#}"); }
                    }
                    break;
                }
            }
        }
    }
    collector.await.context("Event collector task")??;
    bail!("Windows event source stopped unexpectedly")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initial_display_notification_cannot_undo_manual_off() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.jsonl");
        let mut journal = Journal::open(&path).unwrap();
        let config = Config {
            ip: "127.0.0.1".parse().unwrap(),
            mac: None,
            device: None,
            source_path: None,
            wake_verified: false,
            broadcast: "127.0.0.1".parse().unwrap(),
            transport: crate::config::Transport::Auto,
            protected_key: None,
            certificate_sha256: None,
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            run_with_initial(
                &config,
                &mut journal,
                receiver,
                false,
                Some(Initial {
                    target: Target::Off,
                    session: None,
                }),
            )
            .await
        });
        sender
            .send(Notice {
                event: Event::DisplayOn,
                suspend_ack: None,
            })
            .unwrap();
        sleep(Duration::from_millis(400)).await;
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains("initial_display_reconciled"));
        assert!(!log.contains("sync_action_started"));
        sender
            .send(Notice {
                event: Event::DisplayOff,
                suspend_ack: None,
            })
            .unwrap();
        sender
            .send(Notice {
                event: Event::DisplayOn,
                suspend_ack: None,
            })
            .unwrap();
        sleep(Duration::from_millis(400)).await;
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("sync_action_started")
        );
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn brief_off_is_cancelled_and_duplicate_on_is_not_reissued() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.jsonl");
        let mut journal = Journal::open(&path).unwrap();
        // No credential: a scheduled action fails before any TV/WoL traffic.
        // The actual scheduler still runs, including its event collector,
        // debounce, action completion and duplicate suppression paths.
        let config = Config {
            ip: "127.0.0.1".parse().unwrap(),
            mac: Some("44:27:45:51:7E:9A".parse().unwrap()),
            broadcast: "127.0.0.1".parse().unwrap(),
            transport: crate::config::Transport::Auto,
            device: None,
            source_path: None,
            wake_verified: false,
            protected_key: None,
            certificate_sha256: None,
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move { run(&config, &mut journal, receiver, false).await });
        sender
            .send(Notice {
                event: Event::DisplayOff,
                suspend_ack: None,
            })
            .unwrap();
        sleep(Duration::from_millis(100)).await;
        sender
            .send(Notice {
                event: Event::DisplayOn,
                suspend_ack: None,
            })
            .unwrap();
        sleep(Duration::from_millis(400)).await;
        sender
            .send(Notice {
                event: Event::DisplayOn,
                suspend_ack: None,
            })
            .unwrap();
        sleep(Duration::from_millis(1300)).await;
        task.abort();
        let _ = task.await;
        let actions: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|entry| entry["event"] == "sync_action_started")
            .collect();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["details"]["target"], "on");
    }
    #[test]
    fn repeated_events_produce_one_target_change() {
        let mut state = State::default();
        assert_eq!(state.apply(Event::DisplayOn), Some(Target::On));
        assert_eq!(state.apply(Event::DisplayOn), None);
        assert_eq!(state.apply(Event::DisplayDimmed), None);
        assert_eq!(state.apply(Event::DisplayOff), Some(Target::Off));
        assert_eq!(state.apply(Event::DisplayOff), None);
    }
    #[test]
    fn suspend_overrides_late_display_events_and_maintenance_wake_stays_off() {
        let mut state = State::default();
        state.apply(Event::DisplayOn);
        assert_eq!(state.apply(Event::Suspend), Some(Target::Off));
        assert_eq!(state.apply(Event::DisplayOn), None);
        assert_eq!(state.apply(Event::ResumeAutomatic), None);
        assert_eq!(state.apply(Event::ResumeInteractive), Some(Target::On));
        assert_eq!(state.apply(Event::DisplayOn), None);
    }
}
