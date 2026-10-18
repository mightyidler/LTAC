//! Read-only hardware probe: no pairing approval, WoL, or power commands.
//! Run with `cargo run --example verify_ip_recovery` while the saved TV is awake.
use anyhow::{Context, Result, ensure};
use ltac::{config::Config, controller, ssap::PowerState};
use serde_json::json;
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let path = ltac::config::default_path()?;
    let before = std::fs::read(&path)?;
    let result = probe().await;
    let unchanged = std::fs::read(&path)? == before;
    if let Err(error) = &result {
        println!(
            "{}",
            json!({"case":"probe_failed", "passed":false,
            "error":format!("{error:#}"), "production_config_unchanged":unchanged,
            "power_commands_sent":false,"dhcp_changed":false,"actual_ip_moved":false})
        );
    }
    ensure!(unchanged, "Production configuration changed");
    result
}

async fn probe() -> Result<()> {
    let path = ltac::config::default_path()?;
    let original_bytes = std::fs::read(&path)?;
    let original = Config::load(&path)?;
    ensure!(
        original.device.is_some(),
        "Saved discovery identity required"
    );
    ensure!(
        original.certificate_sha256.is_some(),
        "Saved TLS pin required"
    );
    original.key()?;
    let dir = tempfile::tempdir()?;
    let probe_path = dir.path().join("config.json");
    let mut stale = original.clone();
    stale.ip = "127.0.0.1".parse()?;
    stale.source_path = Some(probe_path.clone());
    stale.save(&probe_path)?;

    let start = Instant::now();
    let (endpoint, state) = controller::status(&stale)
        .await
        .context("Stale-address recovery")?;
    let recovered = Config::load(&probe_path)?;
    ensure!(recovered.ip != stale.ip, "Address was not replaced");
    ensure!(recovered.device == original.device, "Identity changed");
    ensure!(
        recovered.protected_key == original.protected_key,
        "Credential changed"
    );
    ensure!(
        recovered.certificate_sha256 == original.certificate_sha256,
        "TLS pin changed"
    );
    ensure!(
        !matches!(state, PowerState::Unknown(_)),
        "Unknown power state"
    );
    println!(
        "{}",
        json!({"case":"stale_ip_recovered", "passed":true,
        "endpoint":endpoint, "state":state, "elapsed_ms":start.elapsed().as_millis(),
        "identity_preserved":true,"credential_preserved":true,"tls_pin_preserved":true,
        "mac_preserved":recovered.mac == original.mac,"wake_verified":recovered.wake_verified})
    );

    let mut wrong_identity = stale.clone();
    wrong_identity.device.as_mut().unwrap().id = "uuid:ltac-verification-nonexistent-device".into();
    wrong_identity.save(&probe_path)?;
    let wrong_bytes = std::fs::read(&probe_path)?;
    ensure!(
        controller::status(&wrong_identity).await.is_err(),
        "Wrong identity accepted"
    );
    ensure!(
        std::fs::read(&probe_path)? == wrong_bytes,
        "Failed recovery changed configuration"
    );
    println!(
        "{}",
        json!({"case":"wrong_identity_rejected", "passed":true,"copy_unchanged":true})
    );

    let mut memory_only = stale.clone();
    memory_only.source_path = None;
    let (candidate, mut session) = ltac::connection::recover(&memory_only).await?;
    let service_state = session.power_state().await?;
    ensure!(
        candidate.ip != memory_only.ip,
        "In-memory address was not recovered"
    );
    ensure!(
        candidate.source_path.is_none(),
        "In-memory recovery gained a save path"
    );
    ensure!(
        std::fs::read(&probe_path)? == wrong_bytes,
        "In-memory recovery wrote to disk"
    );
    println!(
        "{}",
        json!({"case":"memory_only_recovery", "passed":true,
        "state":service_state,"recovered_ip":candidate.ip,"writes_to_disk":false})
    );

    ensure!(
        std::fs::read(&path)? == original_bytes,
        "Production configuration changed"
    );
    println!(
        "{}",
        json!({"case":"production_config_unchanged", "passed":true,
        "power_commands_sent":false,"dhcp_changed":false,"actual_ip_moved":false})
    );
    Ok(())
}
