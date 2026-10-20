use crate::{
    config::Config,
    journal::Journal,
    ssap::{Error, OffEvidence, PowerState, Session},
    wol,
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use tokio::time::{sleep, timeout};

pub async fn paired_session(config: &Config) -> Result<Session> {
    paired_session_resolved(config)
        .await
        .map(|(_, session)| session)
}

pub async fn paired_session_resolved(config: &Config) -> Result<(Config, Session)> {
    let key = config.key()?;
    let attempt = async {
        let mut session = Session::connect(
            config.ip,
            config.transport,
            config.certificate_sha256.as_deref(),
        )
        .await?;
        session.register(Some(&key), false).await?;
        Ok::<_, Error>(session)
    }
    .await;
    match attempt {
        Ok(session) => Ok((config.clone(), session)),
        Err(error)
            if config.device.is_some()
                && (error.retryable()
                    || matches!(error, Error::CertificateChanged | Error::PairingRequired)) =>
        {
            crate::connection::recover(config).await
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn status(config: &Config) -> Result<(String, PowerState)> {
    let mut session = paired_session(config).await?;
    Ok((session.endpoint.clone(), session.power_state().await?))
}

pub async fn off(config: &Config, journal: &mut Journal) -> Result<OffEvidence> {
    off_tracked(config, journal, None).await
}

pub async fn off_tracked(
    config: &Config,
    journal: &mut Journal,
    attempted: Option<&AtomicBool>,
) -> Result<OffEvidence> {
    let start = Instant::now();
    let mut session = paired_session(config).await?;
    let evidence = if session.power_state().await? == PowerState::Standby {
        // Never resend OFF to a TV already reporting standby.
        OffEvidence::AlreadyReportedStandby
    } else {
        session.power_off_tracked(attempted).await?
    };
    journal.record("off_evidence", json!({"endpoint":session.endpoint, "response":evidence, "elapsed_ms":start.elapsed().as_millis(), "physical_state":"not_observed"}))?;
    Ok(evidence)
}

pub async fn on(config: &Config, journal: &mut Journal, duration: Duration) -> Result<()> {
    wake_session(config, journal, duration).await?;
    Ok(())
}

pub async fn off_cached(session: &mut Session, journal: &mut Journal) -> Result<OffEvidence> {
    off_cached_tracked(session, journal, None).await
}

pub async fn off_cached_tracked(
    session: &mut Session,
    journal: &mut Journal,
    attempted: Option<&AtomicBool>,
) -> Result<OffEvidence> {
    let start = Instant::now();
    let evidence = session.power_off_tracked(attempted).await?;
    journal.record("off_evidence", json!({"endpoint":session.endpoint, "response":evidence, "elapsed_ms":start.elapsed().as_millis(), "cached_connection":true, "physical_state":"not_observed"}))?;
    Ok(evidence)
}

/// Compare only the current TV app with the saved PC HDMI for this OFF event.
pub async fn pc_input_active(
    session: &mut Session,
    pc_hdmi: Option<u8>,
    journal: &mut Journal,
) -> bool {
    let Some(port) = pc_hdmi.filter(|port| (1..=4).contains(port)) else {
        let _ = journal.record("off_skipped_input", json!({"reason":"pc_hdmi_not_set"}));
        return false;
    };
    for attempt in 1..=2 {
        let result = timeout(Duration::from_millis(400), session.foreground_app()).await;
        if let Ok(Ok(app)) = &result {
            let allowed = app == &format!("com.webos.app.hdmi{port}");
            let _ = journal.record(
                if allowed {
                    "off_input_verified"
                } else {
                    "off_skipped_input"
                },
                json!({"pc_hdmi":port,"app":app,"endpoint":session.endpoint,
                    "reason":if allowed { "pc_input" } else { "different_input" }}),
            );
            return allowed;
        }
        let reason = if result.is_err() {
            "input_timeout"
        } else {
            "input_unavailable"
        };
        let error = match result {
            Ok(Err(error)) => error.to_string(),
            _ => "Foreground app query timed out".to_owned(),
        };
        let _ = journal.record(
            if attempt == 1 {
                "off_input_retry"
            } else {
                "off_skipped_input"
            },
            json!({"pc_hdmi":port,"reason":reason,"error":error,"attempt":attempt}),
        );
        if attempt == 1 {
            sleep(Duration::from_millis(50)).await;
        }
    }
    false
}

pub async fn protected_input_active(
    session: &mut Session,
    config: &Config,
    journal: &mut Journal,
) -> bool {
    pc_input_active(session, config.pc_hdmi, journal).await
}

pub async fn off_protected(config: &Config, journal: &mut Journal) -> Result<OffEvidence> {
    if config.pc_hdmi.is_none() {
        let _ = journal.record("off_skipped_input", json!({"reason":"pc_hdmi_not_set"}));
        return Ok(OffEvidence::SkippedInput);
    }
    // A TV we cannot query must also be left alone.
    let Ok(Ok(mut session)) = timeout(Duration::from_secs(2), paired_session(config)).await else {
        let _ = journal.record(
            "off_skipped_input",
            json!({"reason":"connection_unavailable"}),
        );
        return Ok(OffEvidence::SkippedInput);
    };
    if !protected_input_active(&mut session, config, journal).await {
        return Ok(OffEvidence::SkippedInput);
    }
    off_cached(&mut session, journal).await
}

pub async fn wake_session(
    config: &Config,
    journal: &mut Journal,
    duration: Duration,
) -> Result<Session> {
    wake_session_resolved(config, journal, duration)
        .await
        .map(|(_, session)| session)
}

pub async fn wake_session_resolved(
    config: &Config,
    journal: &mut Journal,
    duration: Duration,
) -> Result<(Config, Session)> {
    anyhow::ensure!(!crate::lifecycle::is_ending(), "Windows session is ending");
    tokio::select! {
        biased;
        _ = crate::lifecycle::ending() => bail!("ON cancelled: Windows session is ending"),
        result = wake_session_inner(config, journal, duration) => result,
    }
}

async fn wake_session_inner(
    config: &Config,
    journal: &mut Journal,
    duration: Duration,
) -> Result<(Config, Session)> {
    let journal = &mut journal.clone().best_effort();
    let mac = config
        .mac
        .context("TV MAC unavailable. Verify network wake support before enabling ON.")?;
    let key = config.key()?; // Never send WoL before validating credentials.
    let start = Instant::now();
    let wake_log = journal.clone().best_effort();
    // Resume can precede NIC/route readiness. Run the bounded wake schedule
    // concurrently with reconnects so a TCP timeout cannot delay a wake retry.
    let wake_bursts = wake_schedule(duration, |burst| {
        let mut wake_log = wake_log.clone();
        async move {
            match wol::send(mac, config.ip, config.broadcast).await {
                Ok(source) => wake_log.record("wol_sent", json!({"mac":mac.to_string(), "source":source, "destination":config.broadcast, "packets":3, "burst":burst, "power_confirmed":false}))?,
                Err(e) => wake_log.record("wol_send_failed", json!({"burst":burst, "error":e.to_string()}))?,
            }
            Ok(())
        }
    });
    let reconnect = async {
        let mut attempt: u64 = 0;
        let mut current = config.clone();
        let mut recovery_attempted = false;
        loop {
            attempt += 1;
            let result = async {
                let mut session = match Session::connect(
                    current.ip,
                    current.transport,
                    current.certificate_sha256.as_deref(),
                )
                .await
                {
                    Ok(mut session) => {
                        session.register(Some(&key), false).await?;
                        session
                    }
                    Err(error)
                        if error.retryable()
                            && config.device.is_some()
                            && attempt == 2
                            && !recovery_attempted =>
                    {
                        recovery_attempted = true;
                        match crate::connection::recover(&current).await {
                            Ok((candidate, session)) => {
                                current = candidate;
                                session
                            }
                            Err(_) => return Err(error),
                        }
                    }
                    Err(error) => return Err(error),
                };
                let state = session.power_state().await?;
                Ok::<_, Error>((session, state))
            }
            .await;
            match result {
                Ok((session, state)) => {
                    journal.record("reconnect", json!({"attempt":attempt, "endpoint":session.endpoint, "state":state, "elapsed_ms":start.elapsed().as_millis()}))?;
                    if state == PowerState::On {
                        return Ok((current, session));
                    }
                    if matches!(state, PowerState::Unknown(_)) {
                        bail!(
                            "Connected but TV power state is unknown: {state:?}; ON is not confirmed"
                        );
                    }
                }
                Err(e) => {
                    journal.record("reconnect_failed", json!({"attempt":attempt, "error":e.to_string(), "elapsed_ms":start.elapsed().as_millis()}))?;
                    if !e.retryable() {
                        if current.device.is_some()
                            && matches!(e, Error::CertificateChanged | Error::PairingRequired)
                            && !recovery_attempted
                        {
                            recovery_attempted = true;
                            let (candidate, mut session) =
                                crate::connection::recover(&current).await?;
                            current = candidate;
                            if session.power_state().await? == PowerState::On {
                                journal.record("reconnect", json!({"attempt":attempt,"endpoint":session.endpoint,"state":PowerState::On,"identity_recovered":true,"elapsed_ms":start.elapsed().as_millis()}))?;
                                return Ok((current, session));
                            }
                            continue;
                        }
                        return Err(e.into());
                    }
                }
            }
            sleep(Duration::from_millis((attempt * 250).min(1500))).await;
        }
    };
    let operation = async {
        tokio::select! { result = reconnect => result, result = wake_bursts => result }
    };
    match timeout(duration, operation).await {
        Ok(result) => result,
        Err(_) => bail!(
            "ON not confirmed within {} seconds. Check TV network wake setting, standby mode, MAC/interface and UDP broadcast route. See reconnect evidence in the log.",
            duration.as_secs()
        ),
    }
}

// The schedule is also exercised with a counter and virtual time: tests never
// send a magic packet or consult the installed service or a real TV.
async fn wake_schedule<F, Fut, T>(duration: Duration, mut send: F) -> Result<T>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let start = tokio::time::Instant::now();
    for (index, seconds) in [0, 1, 2, 4, 8, 15, 25, 40, 60]
        .into_iter()
        .filter(|seconds| Duration::from_secs(*seconds) < duration)
        .enumerate()
    {
        tokio::time::sleep_until(start + Duration::from_secs(seconds)).await;
        send(index + 1).await?;
    }
    std::future::pending().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test(start_paused = true)]
    async fn wake_bursts_are_bounded_and_stop_on_reconnect_or_cancellation() {
        for (budget, stop, expected) in [
            (
                90,
                90_000,
                vec![0, 1000, 2000, 4000, 8000, 15000, 25000, 40000, 60000],
            ),
            (45, 2_500, vec![0, 1000, 2000]),
            (2, 2_000, vec![0, 1000]),
        ] {
            let start = tokio::time::Instant::now();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let counter = calls.clone();
            let schedule = wake_schedule::<_, _, ()>(Duration::from_secs(budget), move |_| {
                counter.lock().unwrap().push(start.elapsed().as_millis());
                std::future::ready(Ok(()))
            });
            assert!(
                timeout(Duration::from_millis(stop), schedule)
                    .await
                    .is_err()
            );
            assert_eq!(*calls.lock().unwrap(), expected);
            let count = calls.lock().unwrap().len();
            tokio::time::advance(Duration::from_secs(120)).await;
            assert_eq!(calls.lock().unwrap().len(), count);
        }
    }
}
