use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ltac::{
    config::{Config, Transport, default_path},
    controller,
    journal::Journal,
    ssap::Session,
    wol::MacAddress,
};
use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::Ipv4Addr,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(version, about = "LG TV Auto Control — actual power control first")]
struct Cli {
    /// Override the per-user configuration file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Export verified credentials for the protected LocalService controller.
    /// Use only from tools/Install-Service.ps1 after it restricts the directory.
    #[cfg(windows)]
    ServiceExport {
        #[arg(long)]
        output: PathBuf,
    },
    /// Find local LG webOS TVs by name/model, without network details.
    Discover,
    /// Optional fallback only when MAC detection failed during ON verification.
    WakeMac { mac: MacAddress },
    /// Discover, select, request TV approval and save protected credentials.
    Connect {
        #[arg(long)]
        select: Option<usize>,
        #[arg(long)]
        reset: bool,
        /// Fallback only when automatic discovery finds no TV.
        #[arg(long)]
        ip: Option<Ipv4Addr>,
    },
    /// Save the single TV's network details (reconfiguration clears pairing).
    Setup {
        #[arg(long)]
        ip: Ipv4Addr,
        #[arg(long)]
        mac: MacAddress,
        #[arg(long, default_value = "255.255.255.255")]
        broadcast: Ipv4Addr,
        #[arg(long, value_enum, default_value = "auto")]
        transport: Transport,
    },
    /// Pair with a TV that is already on. Approve the prompt on the TV.
    Pair {
        /// Forget the previous key and TLS identity after checking the TV address.
        #[arg(long)]
        reset: bool,
    },
    /// Read the TV's reported power state. Unreachable does not mean OFF.
    Status,
    /// Send Wake-on-LAN, then reconnect and verify the reported ON state.
    On {
        #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    /// Send OFF once. Physical OFF must still be checked on the TV.
    Off,
    /// Follow native Windows display and suspend/resume events until Ctrl+C.
    Watch {
        /// Log actual Windows events without sending any TV power commands.
        #[arg(long)]
        observe_only: bool,
    },
    /// Interactive OFF/ON cycles with mandatory physical observations.
    Verify {
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=20))]
        cycles: u32,
        #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(3..=120))]
        standby_seconds: u64,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ERROR: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    #[cfg(windows)]
    if matches!(
        cli.command,
        Command::Status | Command::On { .. } | Command::Off
    ) && ltac::service_ipc::installed()?
    {
        let request = match cli.command {
            Command::Status => ltac::service_ipc::Request::Refresh,
            Command::On { .. } => ltac::service_ipc::Request::On,
            Command::Off => ltac::service_ipc::Request::Off,
            _ => unreachable!(),
        };
        let snapshot = ltac::service_ipc::request(request).await?;
        println!(
            "Service state: {} (physical panel state not observed)",
            snapshot.power
        );
        return Ok(());
    }
    let path = match cli.config {
        Some(p) => std::path::absolute(p)?,
        None => default_path()?,
    };
    let dir = path.parent().context("Invalid configuration path")?;
    fs::create_dir_all(dir)?;
    // The lock is released by the OS on exit, including Ctrl+C. No resident process.
    let _lock = acquire_lock(dir.join("command.lock"))?;
    let mut journal = Journal::open(&dir.join("events.jsonl"))?;
    let name = match &cli.command {
        #[cfg(windows)]
        Command::ServiceExport { .. } => "service_export",
        Command::Discover => "discover",
        Command::WakeMac { .. } => "wake_mac",
        Command::Connect { .. } => "connect",
        Command::Setup { .. } => "setup",
        Command::Pair { .. } => "pair",
        Command::Status => "status",
        Command::On { .. } => "on",
        Command::Off => "off",
        Command::Watch { .. } => "watch",
        Command::Verify { .. } => "verify",
    };
    let start = Instant::now();
    journal.record("command_started", json!({"command":name}))?;
    println!("Log: {}", dir.join("events.jsonl").display());
    let operation = execute(cli.command, &path, &mut journal);
    let result = tokio::select! {
        result = operation => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            Err(anyhow::anyhow!("Interrupted. TV may remain in its last commanded state; use status or the physical remote."))
        }
    };
    journal.record("command_finished", json!({"command":name, "success":result.is_ok(), "elapsed_ms":start.elapsed().as_millis(), "error":result.as_ref().err().map(|e|format!("{e:#}"))}))?;
    result
}

fn acquire_lock(path: PathBuf) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock()
        .context("Another LTAC command is running. Wait for it to finish.")?;
    Ok(file)
}

async fn execute(command: Command, path: &std::path::Path, journal: &mut Journal) -> Result<()> {
    #[cfg(windows)]
    {
        if let Command::ServiceExport { output } = &command {
            let config = Config::load(path)?.for_service()?;
            config.save(output)?;
            journal.record("service_config_exported", json!({"machine_dpapi":true}))?;
            println!(
                "Exported paired configuration for LocalService. Physical wake verification is recorded separately. Keep the service directory ACL protected."
            );
            return Ok(());
        }
        if ltac::service_ipc::installed()? {
            let request = match command {
                Command::Status => ltac::service_ipc::Request::Refresh,
                Command::On { .. } => ltac::service_ipc::Request::On,
                Command::Off => ltac::service_ipc::Request::Off,
                _ => bail!(
                    "LTAC service owns power control. Stop/uninstall it before direct discovery, pairing, watch or verification."
                ),
            };
            let snapshot = ltac::service_ipc::request(request).await?;
            println!(
                "Service state: {} (physical panel state not observed)",
                snapshot.power
            );
            return Ok(());
        }
    }
    if matches!(command, Command::Discover | Command::Connect { .. }) {
        println!("같은 네트워크에서 LG TV를 찾고 있습니다...");
        let devices = ltac::discovery::discover().await?;
        journal.record("discovery_finished", json!({"devices":devices}))?;
        for (index, device) in devices.iter().enumerate() {
            println!(
                "{}. {} · {}",
                index + 1,
                device.identity.name,
                device.identity.model
            );
        }
        if let Command::Connect { select, reset, ip } = command {
            let config = if let Some(ip) = ip {
                anyhow::ensure!(
                    devices.is_empty(),
                    "자동 검색된 TV를 선택해 주세요. 수동 IP는 검색 실패 시에만 사용할 수 있습니다."
                );
                println!("TV에서 연결을 승인해 주세요. (최대 90초)");
                ltac::connection::pair_manual(ip, path, journal).await?
            } else {
                anyhow::ensure!(
                    !devices.is_empty(),
                    "TV를 찾지 못했어요. TV 전원과 PC 네트워크를 확인하고 다시 검색해 주세요."
                );
                let selected = match select {
                    Some(index) => index,
                    None => {
                        print!("연결할 TV 번호: ");
                        io::stdout().flush()?;
                        let input = read_line().await?;
                        input
                            .trim()
                            .parse::<usize>()
                            .context("TV 번호를 입력해 주세요.")?
                    }
                };
                let device = selected
                    .checked_sub(1)
                    .and_then(|i| devices.get(i))
                    .context("목록에 있는 TV 번호를 선택해 주세요.")?;
                println!(
                    "{} — TV에서 연결을 승인해 주세요. (최대 90초)",
                    device.identity.name
                );
                ltac::connection::pair_device(device, path, reset, journal).await?
            };
            println!("연결 정보를 저장하고 TV 상태 응답을 확인했습니다.");
            if config.wake_verified {
                println!("이 TV의 실제 OFF/ON 검증 기록이 유지되었습니다.");
                return Ok(());
            }
            if config.mac.is_some() {
                println!(
                    "TV 네트워크 주소 자동 확인 완료. `ltac verify --cycles 1`로 실제 OFF/ON을 검증하세요."
                );
            } else {
                println!(
                    "전원 끄기는 사용 가능합니다. 자동 켜기에 필요한 네트워크 주소를 확인하지 못했습니다. ON 검증 단계에서 TV 네트워크 켜기 설정과 MAC을 확인해 주세요."
                );
            }
        } else if devices.is_empty() {
            println!(
                "TV를 찾지 못했어요. TV 전원과 PC의 네트워크 연결을 확인하고 다시 검색해 주세요."
            );
        }
        return Ok(());
    }
    if let Command::Setup {
        ip,
        mac,
        broadcast,
        transport,
    } = command
    {
        Config {
            ip,
            mac: Some(mac),
            broadcast,
            transport,
            device: None,
            source_path: None,
            wake_verified: false,
            protected_key: None,
            certificate_sha256: None,
        }
        .save(path)?;
        println!("Saved TV {ip} / {mac}. Turn the TV on manually, then run `ltac pair`.");
        return Ok(());
    }
    let mut config = Config::load(path)?;
    match command {
        #[cfg(windows)]
        Command::ServiceExport { .. } => unreachable!(),
        Command::WakeMac { mac } => {
            config.mac = Some(mac);
            config.wake_verified = false;
            config.save(path)?;
            journal.record(
                "wake_mac_saved",
                json!({"source":"user", "mac":mac, "wake_verified":false}),
            )?;
            println!(
                "Saved fallback wake address. Run `ltac verify --cycles 1`; ON support is not yet verified."
            );
        }
        Command::Pair { reset } => {
            // Reset is committed only after a new registration succeeds.
            let key = if reset || config.protected_key.is_none() {
                None
            } else {
                Some(config.key()?)
            };
            let pin = if reset {
                None
            } else {
                config.certificate_sha256.as_deref()
            };
            println!(
                "Connecting to {}. Approve LTAC on the TV if prompted (90 seconds).",
                config.ip
            );
            let mut session = Session::connect(config.ip, config.transport, pin).await?;
            let key = session.register(key.as_deref(), true).await?;
            config.set_key(&key)?;
            config.certificate_sha256 = session.certificate_sha256.clone();
            config.save(path)?;
            journal.record("paired", json!({"ip":config.ip, "endpoint":session.endpoint, "tls_identity_pinned":config.certificate_sha256.is_some()}))?;
            println!(
                "Paired. Credential protected with Windows DPAPI. Endpoint: {}",
                session.endpoint
            );
        }
        Command::Status => {
            let (endpoint, state) = controller::status(&config).await?;
            journal.record("status", json!({"endpoint":endpoint, "state":state}))?;
            println!("TV reported: {state:?} ({endpoint}). Physical panel state is not observed.");
        }
        Command::On { timeout } => {
            controller::on(&config, journal, Duration::from_secs(timeout)).await?;
            println!("ON confirmed by authenticated reconnect and TV power-state response.");
        }
        Command::Off => {
            let evidence = controller::off(&config, journal).await?;
            println!(
                "OFF evidence: {evidence:?}. Check the physical TV; OFF is not yet physically confirmed."
            );
        }
        Command::Watch { observe_only } => {
            ltac::sync::watch(&config, journal, observe_only).await?;
        }
        Command::Verify {
            cycles,
            timeout,
            standby_seconds,
        } => {
            let (_, initial) = controller::status(&config)
                .await
                .context("Start verification with the TV on and paired")?;
            if initial != ltac::ssap::PowerState::On {
                bail!("Start verification with TV reporting ON; got {initial:?}");
            }
            for cycle in 1..=cycles {
                println!("Cycle {cycle}/{cycles}: sending OFF.");
                controller::off(&config, journal).await?;
                observe(journal, cycle, "off", "Is the physical TV OFF? [y/N] ").await?;
                tokio::time::sleep(Duration::from_secs(standby_seconds)).await;
                println!("Sending WoL and verifying reconnect...");
                controller::on(&config, journal, Duration::from_secs(timeout)).await?;
                observe(journal, cycle, "on", "Is the physical TV ON? [y/N] ").await?;
                journal.record(
                    "cycle_passed",
                    json!({"cycle":cycle, "physical_off":true, "physical_on":true}),
                )?;
            }
            journal.record(
                "verification_passed",
                json!({"cycles":cycles, "physical_observations":true}),
            )?;
            let mut saved = Config::load(path)?;
            saved.wake_verified = true;
            saved.save(path)?;
            println!("PASS: {cycles} physical OFF/ON cycles confirmed. TV left ON.");
        }
        Command::Setup { .. } | Command::Discover | Command::Connect { .. } => unreachable!(),
    }
    Ok(())
}

async fn read_line() -> Result<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = sender.send(io::stdin().read_line(&mut line).map(|_| line));
    });
    Ok(receiver.await??)
}

async fn observe(
    journal: &mut Journal,
    cycle: u32,
    state: &str,
    prompt: &'static str,
) -> Result<()> {
    print!("{prompt}");
    io::stdout().flush()?;
    // A plain thread is intentional: Tokio waits for spawn_blocking stdin at
    // shutdown, preventing Ctrl+C from exiting while a console read is pending.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = sender.send(io::stdin().read_line(&mut line).map(|_| line));
    });
    let input = receiver.await??;
    let confirmed = matches!(input.trim().to_lowercase().as_str(), "y" | "yes");
    journal.record(
        "physical_observation",
        json!({"cycle":cycle, "state":state, "confirmed":confirmed, "source":"human"}),
    )?;
    if !confirmed {
        bail!(
            "Physical {state} was not confirmed. Verification stopped; TV left in its last commanded state."
        );
    }
    Ok(())
}
