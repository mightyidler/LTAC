use crate::{
    config::Config,
    journal::Journal,
    ssap::{Error, OffEvidence, PowerState, Session},
    wol,
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::time::{sleep, timeout};

pub async fn paired_session(config: &Config) -> Result<Session> {
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
        Ok(session) => Ok(session),
        Err(error)
            if config.device.is_some()
                && (error.retryable()
                    || matches!(error, Error::CertificateChanged | Error::PairingRequired)) =>
        {
            crate::connection::recover(config)
                .await
                .map(|(_, session)| session)
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn status(config: &Config) -> Result<(String, PowerState)> {
    let mut session = paired_session(config).await?;
    Ok((session.endpoint.clone(), session.power_state().await?))
}

pub async fn off(config: &Config, journal: &mut Journal) -> Result<OffEvidence> {
    let start = Instant::now();
    let mut session = paired_session(config).await?;
    let evidence = if session.power_state().await? == PowerState::Standby {
        // Never resend OFF to a TV already reporting standby.
        OffEvidence::AlreadyReportedStandby
    } else {
        session.power_off().await?
    };
    journal.record("off_evidence", json!({"endpoint":session.endpoint, "response":evidence, "elapsed_ms":start.elapsed().as_millis(), "physical_state":"not_observed"}))?;
    Ok(evidence)
}

pub async fn on(config: &Config, journal: &mut Journal, duration: Duration) -> Result<()> {
    wake_session(config, journal, duration).await?;
    Ok(())
}

pub async fn off_cached(session: &mut Session, journal: &mut Journal) -> Result<OffEvidence> {
    let start = Instant::now();
    let evidence = session.power_off().await?;
    journal.record("off_evidence", json!({"endpoint":session.endpoint, "response":evidence, "elapsed_ms":start.elapsed().as_millis(), "cached_connection":true, "physical_state":"not_observed"}))?;
    Ok(evidence)
}

pub async fn wake_session(
    config: &Config,
    journal: &mut Journal,
    duration: Duration,
) -> Result<Session> {
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
) -> Result<Session> {
    let mac = config
        .mac
        .context("TV MAC unavailable. Verify network wake support before enabling ON.")?;
    let key = config.key()?; // Never send WoL before validating credentials.
    let start = Instant::now();
    let mut wake_log = journal.clone();
    // Resume can precede NIC/route readiness. Run the bounded wake schedule
    // concurrently with reconnects so a TCP timeout cannot delay a wake retry.
    let wake_bursts = async {
        for (index, seconds) in [0, 1, 2, 4, 8, 15, 25, 40, 60]
            .into_iter()
            .filter(|seconds| Duration::from_secs(*seconds) < duration)
            .enumerate()
        {
            tokio::time::sleep_until(tokio::time::Instant::from_std(
                start + Duration::from_secs(seconds),
            ))
            .await;
            let burst = index + 1;
            match wol::send(mac, config.ip, config.broadcast).await {
                Ok(source) => wake_log.record("wol_sent", json!({"mac":mac.to_string(), "source":source, "destination":config.broadcast, "packets":3, "burst":burst, "power_confirmed":false}))?,
                Err(e) => wake_log.record("wol_send_failed", json!({"burst":burst, "error":e.to_string()}))?,
            }
        }
        std::future::pending::<Result<Session>>().await
    };
    let reconnect = async {
        let mut attempt: u64 = 0;
        let mut current = config.clone();
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
                    Err(error) if error.retryable() && config.device.is_some() && attempt == 2 => {
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
                        return Ok(session);
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
                        {
                            let (candidate, mut session) =
                                crate::connection::recover(&current).await?;
                            current = candidate;
                            if session.power_state().await? == PowerState::On {
                                journal.record("reconnect", json!({"attempt":attempt,"endpoint":session.endpoint,"state":PowerState::On,"identity_recovered":true,"elapsed_ms":start.elapsed().as_millis()}))?;
                                return Ok(session);
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
