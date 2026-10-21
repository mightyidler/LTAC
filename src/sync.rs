use crate::{config::Config, controller, journal::Journal};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::{
    sync::watch,
    time::{sleep, timeout},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    ScreenProtection(crate::screen_protection::Signal),
    DisplayTopologyChanged,
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
    pub fn is_ending(&self) -> bool {
        self.ending
    }
    pub fn apply_notice(&mut self, notice: &Notice) -> Option<Target> {
        if let Some(context) = notice.preceding {
            self.suspended = context.suspended;
            if context.ending && !self.ending {
                return self.apply(Event::SessionEnding);
            }
        }
        self.apply(notice.event)
    }
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
            | Event::DisplayTopologyChanged
            | Event::SessionLocked
            | Event::SessionUnlocked => None,
            Event::ScreenProtection(_) => None,
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

#[derive(Debug)]
pub struct Notice {
    pub preceding: Option<EventContext>,
    pub event: Event,
    // The native suspend callback can wait briefly for an OFF attempt before
    // returning. This is never used for ordinary display state changes.
    pub suspend_ack: Option<std::sync::mpsc::SyncSender<()>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EventContext {
    suspended: bool,
    ending: bool,
}

// At most one queued notice of each kind. Producers never block an OS callback;
// only still-pending duplicates are replaced, retaining their latest order.
struct Notices {
    context: State,
    queue: std::collections::VecDeque<Notice>,
    senders: usize,
    closed: bool,
}
struct NoticeMailbox {
    pending: std::sync::Mutex<Notices>,
    ready: tokio::sync::Notify,
}
pub struct NoticeSender(std::sync::Arc<NoticeMailbox>);
pub struct NoticeReceiver(std::sync::Arc<NoticeMailbox>);
#[derive(Debug)]
pub struct SendNoticeError(pub Notice);

pub fn notice_channel() -> (NoticeSender, NoticeReceiver) {
    let shared = std::sync::Arc::new(NoticeMailbox {
        pending: std::sync::Mutex::new(Notices {
            context: State::default(),
            queue: Default::default(),
            senders: 1,
            closed: false,
        }),
        ready: tokio::sync::Notify::new(),
    });
    (NoticeSender(shared.clone()), NoticeReceiver(shared))
}
impl Clone for NoticeSender {
    fn clone(&self) -> Self {
        self.0.pending.lock().unwrap().senders += 1;
        Self(self.0.clone())
    }
}
impl NoticeSender {
    pub fn send(&self, mut notice: Notice) -> std::result::Result<(), SendNoticeError> {
        let mut pending = self.0.pending.lock().unwrap();
        if pending.closed {
            return Err(SendNoticeError(notice));
        }
        // Carry suspension/termination context through each routing layer.
        // Removing a repeated maintenance wake must not make a later DisplayOn
        // look as if it had occurred while Windows was still suspended.
        let before = notice.preceding.unwrap_or(EventContext {
            suspended: pending.context.suspended,
            ending: pending.context.ending,
        });
        pending.context.apply_notice(&notice);
        notice.preceding = Some(before);
        if let Some(index) = pending
            .queue
            .iter()
            .position(|queued| queued.event == notice.event)
        {
            let previous = pending.queue.remove(index).unwrap();
            if notice.suspend_ack.is_none() {
                notice.suspend_ack = previous.suspend_ack;
            }
        }
        pending.queue.push_back(notice);
        drop(pending);
        self.0.ready.notify_one();
        Ok(())
    }
}
impl Drop for NoticeSender {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().senders -= 1;
        self.0.ready.notify_one();
    }
}
impl NoticeReceiver {
    pub async fn recv(&mut self) -> Option<Notice> {
        // A continuously replenished mailbox must still yield to power I/O.
        tokio::task::consume_budget().await;
        loop {
            let notified = self.0.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut pending = self.0.pending.lock().unwrap();
                if let Some(notice) = pending.queue.pop_front() {
                    return Some(notice);
                }
                if pending.senders == 0 {
                    return None;
                }
            }
            notified.await;
        }
    }
}
impl Drop for NoticeReceiver {
    fn drop(&mut self) {
        let mut pending = self.0.pending.lock().unwrap();
        pending.closed = true;
        pending.queue.clear();
    }
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
        let (sender, receiver) = notice_channel();
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
    receiver: NoticeReceiver,
    observe_only: bool,
) -> Result<()> {
    run_with_initial(config, journal, receiver, observe_only, None).await
}

pub async fn run_with_initial(
    config: &Config,
    journal: &mut Journal,
    receiver: NoticeReceiver,
    observe_only: bool,
    initial: Option<Initial>,
) -> Result<()> {
    run_with_policy(config, journal, receiver, observe_only, initial, false).await
}

pub async fn run_with_policy(
    config: &Config,
    journal: &mut Journal,
    receiver: NoticeReceiver,
    observe_only: bool,
    initial: Option<Initial>,
    keep_tv_on: bool,
) -> Result<()> {
    run_with_screen_policy(
        config,
        journal,
        receiver,
        observe_only,
        initial,
        keep_tv_on,
        true,
    )
    .await
}

pub async fn run_with_screen_policy(
    config: &Config,
    journal: &mut Journal,
    mut receiver: NoticeReceiver,
    observe_only: bool,
    initial: Option<Initial>,
    keep_tv_on: bool,
    power_automatic: bool,
) -> Result<()> {
    let journal = &mut journal.clone().best_effort();
    let (sender, mut targets) = watch::channel::<Option<Action>>(None);
    let (network_sender, mut networks) = watch::channel(0u64);
    let (screen_sender, mut screens) = watch::channel(crate::screen_protection::Signal::Disabled);
    let mut panel = crate::screen_protection::Panel::default();
    let mut event_log = journal.clone();
    let mut suppress_initial_display = initial.is_some();
    let mut applied = initial.as_ref().map(|i| i.target);
    let mut prepared = initial.and_then(|i| i.session);
    let off_attempted = AtomicBool::new(applied == Some(Target::Off));
    let collector = tokio::spawn(async move {
        let mut state = State::default();
        let mut screen_session = crate::screen_protection::SessionPolicy::default();
        while let Some(notice) = receiver.recv().await {
            if let Event::ScreenProtection(signal) = notice.event {
                if screen_session.allowed() || signal == crate::screen_protection::Signal::Disabled
                {
                    screen_sender.send_replace(signal);
                }
                continue;
            }
            if screen_session.apply(notice.event) && !screen_session.allowed() {
                screen_sender.send_replace(crate::screen_protection::Signal::Disabled);
            }
            // Logging failure must not disable power handling during shutdown.
            let _ = event_log.record("windows_event", json!({"event":notice.event}));
            if notice.event == Event::NetworkChanged && !observe_only {
                network_sender.send_modify(|generation| *generation = generation.wrapping_add(1));
            }
            println!("Windows: {:?}", notice.event);
            let desired = state.apply_notice(&notice);
            if suppress_initial_display
                && matches!(
                    notice.event,
                    Event::DisplayOn | Event::DisplayOff | Event::DisplayDimmed
                )
            {
                // Reopening the event source after a manual command emits the
                // current display state once. It must not undo manual OFF.
                suppress_initial_display = false;
                let _ =
                    event_log.record("initial_display_reconciled", json!({"event":notice.event}));
                continue;
            }
            if observe_only {
                if let Some(ack) = notice.suspend_ack {
                    let _ = ack.try_send(());
                }
            } else if let Some(target) = desired.filter(|_| power_automatic).or_else(|| {
                (power_automatic && matches!(notice.event, Event::Suspend | Event::SessionEnding))
                    .then_some(Target::Off)
            }) {
                sender.send_replace(Some(Action {
                    target,
                    urgent: state.is_ending()
                        || matches!(notice.event, Event::Suspend | Event::SessionEnding),
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
            biased;
            changed = targets.changed() => { if changed.is_err() { break; } }
            changed = screens.changed() => {
                if changed.is_err() { break; }
                let signal = *screens.borrow_and_update();
                let (cancelled, power_event) = {
                let operation = panel.apply(signal, config, &mut prepared, journal);
                tokio::pin!(operation);
                tokio::select! {
                    biased;
                    _ = targets.changed() => (true, true),
                    _ = screens.changed() => (true, false),
                    result = &mut operation => { if let Err(error) = result { eprintln!("Panel request failed: {error:#}"); } (false, false) }
                }
                };
                if cancelled { prepared = None; panel.invalidate(); }
                if cancelled && !power_event { screens.mark_changed(); }
                if !power_event { continue; }
            }
            changed = networks.changed() => {
                if changed.is_err() { break; }
                if config.device.is_none() { continue; }
                let desired = targets.borrow().as_ref().map(|action| action.target).or(applied);
                if desired != Some(Target::On) || prepared.is_some() || crate::lifecycle::is_ending() { continue; }
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
                    panel.disconnected();
                    journal.record("idle_connection_closed", json!({"error":e.to_string()}))?;
                }
                continue;
            }
        }
        let Some(mut action) = targets.borrow_and_update().clone() else {
            continue;
        };
        panel.invalidate();
        loop {
            // Only duplicate intentions are coalesced. Real transitions start
            // immediately; a possibly transmitted OFF survives cancellation.
            if applied == Some(action.target)
                || (action.target == Target::Off && off_attempted.load(Ordering::Acquire))
            {
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
                    anyhow::ensure!(!crate::lifecycle::is_ending(), "Windows session is ending");
                    off_attempted.store(false, Ordering::Release);
                    if let Some(session) = &mut cached
                        && matches!(
                            timeout(Duration::from_millis(250), session.power_state()).await,
                            Ok(Ok(crate::ssap::PowerState::On))
                        )
                    {
                        return Ok((cached.take(), true));
                    }
                    controller::wake_session(config, &mut action_log, Duration::from_secs(45))
                        .await
                        .map(|session| (Some(session), true))
                } else {
                    let limit = if urgent {
                        Duration::from_millis(1400)
                    } else {
                        Duration::from_secs(15)
                    };
                    let turn_off = async {
                        if keep_tv_on {
                            if config.pc_hdmi.is_none() {
                                let _ = action_log.record(
                                    "off_skipped_input",
                                    json!({"reason":"pc_hdmi_not_set"}),
                                );
                                return Ok(crate::ssap::OffEvidence::SkippedInput);
                            }
                            if cached.is_none() {
                                cached = controller::paired_session(config).await.ok();
                            }
                            let allowed = if let Some(session) = &mut cached {
                                controller::protected_input_active(session, config, &mut action_log)
                                    .await
                            } else {
                                let _ = action_log.record(
                                    "off_skipped_input",
                                    json!({"reason":"connection_unavailable","ip":config.ip}),
                                );
                                false
                            };
                            if !allowed {
                                return Ok(crate::ssap::OffEvidence::SkippedInput);
                            }
                        }
                        if let Some(session) = &mut cached {
                            controller::off_cached_tracked(
                                session,
                                &mut action_log,
                                Some(&off_attempted),
                            )
                            .await
                        } else {
                            controller::off_tracked(config, &mut action_log, Some(&off_attempted))
                                .await
                        }
                    };
                    let evidence = timeout(limit, turn_off)
                        .await
                        .context("OFF budget expired; physical OFF remains unconfirmed")??;
                    let applied = !matches!(evidence, crate::ssap::OffEvidence::SkippedInput);
                    Ok((None, applied))
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
                        Ok((session, performed)) => { prepared = session; applied = performed.then_some(target); if target == Target::On && performed { panel = Default::default(); } },
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

    fn notice(event: Event) -> Notice {
        Notice {
            preceding: None,
            event,
            suspend_ack: None,
        }
    }
    fn isolated_config() -> Config {
        serde_json::from_value(json!({"ip":"127.0.0.1", "broadcast":"127.0.0.1", "transport":"ws"}))
            .unwrap()
    }

    #[tokio::test]
    async fn bounded_mailbox_preserves_reordered_power_sequences_and_routing_context() {
        let events = [
            Event::Suspend,
            Event::ResumeAutomatic,
            Event::ResumeInteractive,
            Event::DisplayOn,
            Event::DisplayOff,
            Event::SessionEnding,
        ];
        // Includes automatic-wake/display/automatic-wake and repeated terminal
        // events, where keeping only the last raw event loses essential state.
        for mut sequence in 0..6usize.pow(6) {
            let (sender, mut receiver) = notice_channel();
            let mut expected = State::default();
            for _ in 0..6 {
                let event = events[sequence % events.len()];
                sequence /= events.len();
                expected.apply(event);
                sender.send(notice(event)).unwrap();
            }
            assert!(sender.0.pending.lock().unwrap().queue.len() <= events.len());
            drop(sender);
            let (route, mut routed) = notice_channel();
            while let Some(notice) = receiver.recv().await {
                route.send(notice).unwrap();
            }
            drop(route);
            let mut actual = State::default();
            while let Some(notice) = routed.recv().await {
                actual.apply_notice(&notice);
            }
            assert_eq!(actual.desired, expected.desired);
            assert_eq!(actual.ending, expected.ending);
            if !actual.ending {
                assert_eq!(actual.suspended, expected.suspended);
            }
        }
        let (sender, mut receiver) = notice_channel();
        let (ack, wait) = std::sync::mpsc::sync_channel(1);
        sender
            .send(Notice {
                suspend_ack: Some(ack),
                ..notice(Event::Suspend)
            })
            .unwrap();
        for _ in 0..100_000 {
            sender.send(notice(Event::Suspend)).unwrap();
        }
        assert_eq!(sender.0.pending.lock().unwrap().queue.len(), 1);
        receiver
            .recv()
            .await
            .unwrap()
            .suspend_ack
            .unwrap()
            .try_send(())
            .unwrap();
        assert!(wait.try_recv().is_ok());
        drop(receiver);
        assert!(sender.send(notice(Event::SessionEnding)).is_err());
    }

    #[tokio::test]
    async fn immediate_off_is_sent_once_across_display_suspend_and_shutdown_without_ack() {
        use futures_util::StreamExt;
        let (session, mut peer) = crate::ssap::tests::peer().await;
        let (seen, received) = tokio::sync::oneshot::channel();
        let remote = tokio::spawn(async move {
            let frame = peer.next().await.unwrap().unwrap();
            let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
            seen.send(()).unwrap();
            let mut commands = 1;
            while let Some(Ok(frame)) = peer.next().await {
                commands += usize::from(frame.is_text());
            }
            commands
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut journal = Journal::open(&path).unwrap();
        let (sender, receiver) = notice_channel();
        let task = tokio::spawn(async move {
            run_with_initial(
                &isolated_config(),
                &mut journal,
                receiver,
                false,
                Some(Initial {
                    target: Target::On,
                    session: Some(session),
                }),
            )
            .await
        });
        sender.send(notice(Event::DisplayOn)).unwrap();
        sender.send(notice(Event::DisplayOff)).unwrap();
        timeout(Duration::from_millis(300), received)
            .await
            .unwrap()
            .unwrap();
        for _ in 0..1000 {
            sender.send(notice(Event::Suspend)).unwrap();
        }
        sender.send(notice(Event::SessionEnding)).unwrap();
        sender.send(notice(Event::DisplayOn)).unwrap();
        sender.send(notice(Event::ResumeInteractive)).unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), remote)
                .await
                .unwrap()
                .unwrap(),
            1
        );
        task.abort();
        let _ = task.await;
        let log = std::fs::read_to_string(path).unwrap();
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("sync_action_started"))
                .count(),
            1
        );
        assert!(!log.contains("wol_sent"));
    }

    #[tokio::test]
    async fn cached_on_uses_one_state_query_without_wol_or_authentication() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let (session, mut peer) = crate::ssap::tests::peer().await;
        let (done, completed) = tokio::sync::oneshot::channel();
        let remote = tokio::spawn(async move {
            let frame = peer.next().await.unwrap().unwrap();
            let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(frame["uri"], crate::ssap::POWER_STATE);
            peer.send(Message::Text(
                json!({"id":frame["id"],"type":"response","payload":{"state":"Active"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            done.send(()).unwrap();
            let mut calls = 1;
            while let Some(Ok(frame)) = peer.next().await {
                calls += usize::from(frame.is_text());
            }
            calls
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut journal = Journal::open(&path).unwrap();
        let (sender, receiver) = notice_channel();
        let task = tokio::spawn(async move {
            run_with_initial(
                &isolated_config(),
                &mut journal,
                receiver,
                false,
                Some(Initial {
                    target: Target::Off,
                    session: Some(session),
                }),
            )
            .await
        });
        sender.send(notice(Event::DisplayOff)).unwrap();
        sender.send(notice(Event::ResumeInteractive)).unwrap();
        timeout(Duration::from_millis(300), completed)
            .await
            .unwrap()
            .unwrap();
        for _ in 0..1000 {
            sender.send(notice(Event::DisplayOn)).unwrap();
        }
        sleep(Duration::from_millis(20)).await;
        task.abort();
        let _ = task.await;
        assert_eq!(remote.await.unwrap(), 1);
        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"success\":true"));
        assert!(!log.contains("wol_sent"));
    }

    #[tokio::test]
    async fn protected_display_suspend_and_shutdown_preserve_non_pc_viewing() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        for event in [Event::DisplayOff, Event::Suspend, Event::SessionEnding] {
            let (session, mut peer) = crate::ssap::tests::peer().await;
            let remote = tokio::spawn(async move {
                let frame = peer.next().await.unwrap().unwrap();
                let frame: serde_json::Value =
                    serde_json::from_str(frame.to_text().unwrap()).unwrap();
                assert_eq!(frame["uri"], crate::ssap::FOREGROUND_APP);
                peer.send(Message::Text(json!({"id":frame["id"], "type":"response", "payload":{"appId":"netflix","returnValue":true}}).to_string().into())).await.unwrap();
                while let Some(Ok(frame)) = peer.next().await {
                    assert!(
                        !frame.is_text(),
                        "Protected OFF sent a command after Netflix was detected"
                    );
                }
            });
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("events.jsonl");
            let mut journal = Journal::open(&path).unwrap();
            let config: Config = serde_json::from_value(
                json!({"ip":"127.0.0.1", "broadcast":"127.0.0.1", "transport":"ws", "pc_hdmi":2, "pc_hdmi_source":"manual"}),
            )
            .unwrap();
            let (sender, receiver) = notice_channel();
            let task = tokio::spawn(async move {
                run_with_policy(
                    &config,
                    &mut journal,
                    receiver,
                    false,
                    Some(Initial {
                        target: Target::On,
                        session: Some(session),
                    }),
                    true,
                )
                .await
            });
            // Reconcile the initial Windows display notification first.
            sender
                .send(Notice {
                    preceding: None,
                    event: Event::DisplayOn,
                    suspend_ack: None,
                })
                .unwrap();
            sender
                .send(Notice {
                    preceding: None,
                    event,
                    suspend_ack: None,
                })
                .unwrap();
            timeout(Duration::from_secs(3), remote)
                .await
                .unwrap()
                .unwrap();
            task.abort();
            let _ = task.await;
            let log = std::fs::read_to_string(path).unwrap();
            assert!(log.contains("off_skipped_input"));
            assert!(!log.contains("off_evidence"));
        }
    }

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
            pc_hdmi: None,
            pc_hdmi_source: Default::default(),
            broadcast: "127.0.0.1".parse().unwrap(),
            transport: crate::config::Transport::Auto,
            protected_key: None,
            certificate_sha256: None,
        };
        let (sender, receiver) = notice_channel();
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
                preceding: None,
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
                preceding: None,
                event: Event::DisplayOff,
                suspend_ack: None,
            })
            .unwrap();
        sender
            .send(Notice {
                preceding: None,
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
    async fn pending_off_is_superseded_and_duplicate_on_is_not_reissued() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.jsonl");
        let mut journal = Journal::open(&path).unwrap();
        // No credential: a scheduled action fails before any TV/WoL traffic.
        // The actual scheduler still runs, including its event collector,
        // action completion and duplicate suppression paths.
        let config = Config {
            ip: "127.0.0.1".parse().unwrap(),
            mac: Some("44:27:45:51:7E:9A".parse().unwrap()),
            broadcast: "127.0.0.1".parse().unwrap(),
            transport: crate::config::Transport::Auto,
            device: None,
            source_path: None,
            wake_verified: false,
            pc_hdmi: None,
            pc_hdmi_source: Default::default(),
            protected_key: None,
            certificate_sha256: None,
        };
        let (sender, receiver) = notice_channel();
        let task = tokio::spawn(async move { run(&config, &mut journal, receiver, false).await });
        sender
            .send(Notice {
                preceding: None,
                event: Event::DisplayOff,
                suspend_ack: None,
            })
            .unwrap();
        sender
            .send(Notice {
                preceding: None,
                event: Event::DisplayOn,
                suspend_ack: None,
            })
            .unwrap();
        sleep(Duration::from_millis(400)).await;
        sender
            .send(Notice {
                preceding: None,
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
