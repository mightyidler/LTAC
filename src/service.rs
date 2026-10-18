//! Native, UI-independent boot/shutdown controller. SCM callbacks only publish
//! events; all TV I/O belongs to one cancellable worker on a current-thread RT.
use crate::{
    config::Config,
    controller,
    journal::Journal,
    service_ipc::{self as ipc, Request, Response, Snapshot},
    ssap::{PowerState, Session},
    sync::{Event, Notice, State, Target},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::{mpsc, oneshot, watch},
    time::{sleep, timeout},
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SECURITY_ATTRIBUTES,
    },
    System::{
        Power::{
            POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
            UnregisterPowerSettingNotification,
        },
        Services::*,
        SystemServices::GUID_CONSOLE_DISPLAY_STATE,
    },
    UI::WindowsAndMessaging::{
        DEVICE_NOTIFY_SERVICE_HANDLE, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
        PBT_POWERSETTINGCHANGE,
    },
};

static HANDLE: AtomicUsize = AtomicUsize::new(0);
static BRIDGE: OnceLock<Bridge> = OnceLock::new();
static INITIAL_DISPLAY: AtomicBool = AtomicBool::new(true);
static CONSOLE_DISPLAY: AtomicU32 = AtomicU32::new(u32::MAX);
static SNAPSHOT_CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

struct Bridge {
    events: mpsc::UnboundedSender<Notice>,
    stop: watch::Sender<u32>,
}
#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub owner_sid: String,
    pub automatic: bool,
}
struct Envelope {
    request: Request,
    reply: oneshot::Sender<Response>,
}
#[derive(Clone, Copy)]
enum Kind {
    On,
    Off,
    Refresh,
}
struct Work {
    kind: Kind,
    urgent: bool,
    delay: Duration,
    reply: Option<oneshot::Sender<Response>>,
}

// All devices share one policy and cancellation boundary, but retain separate
// authenticated sockets and OFF evidence. A slow TV never delays another TV.
struct DeviceControl {
    config: Config,
    session: Option<Session>,
    off_attempted: AtomicBool,
    io_started: bool,
}
struct Fleet(Vec<DeviceControl>);
impl Fleet {
    fn new(mut devices: Vec<Config>) -> Result<Self> {
        anyhow::ensure!(devices.len() <= 16, "At most 16 TVs are supported");
        for (index, config) in devices.iter().enumerate() {
            config.validate()?;
            config.key()?;
            anyhow::ensure!(
                !devices[..index].iter().any(|other| other.ip == config.ip
                    || (other.device.is_some()
                        && other.device.as_ref().map(|d| &d.id)
                            == config.device.as_ref().map(|d| &d.id))),
                "Duplicate TV configuration"
            );
        }
        for config in &mut devices {
            config.source_path = None;
        }
        Ok(Self(
            devices
                .into_iter()
                .map(|config| DeviceControl {
                    config,
                    session: None,
                    off_attempted: AtomicBool::new(false),
                    io_started: false,
                })
                .collect(),
        ))
    }
    fn wake_available(&self) -> bool {
        self.0.iter().all(|device| device.config.mac.is_some())
    }
    fn all_off_attempted(&self) -> bool {
        !self.0.is_empty()
            && self
                .0
                .iter()
                .all(|d| d.off_attempted.load(Ordering::Acquire))
    }
    fn clear_sessions(&mut self) {
        for device in &mut self.0 {
            device.session = None;
        }
    }
    fn cancel_sessions(&mut self) {
        for device in &mut self.0 {
            if device.io_started {
                device.session = None;
            }
        }
    }
    fn fingerprint(&self) -> Result<Vec<u8>> {
        fingerprint(&self.0.iter().map(|d| d.config.clone()).collect::<Vec<_>>())
    }
    fn save(&self) -> Result<()> {
        use std::io::Write;
        let directory = directory()?;
        let configs = self.0.iter().map(|d| &d.config).collect::<Vec<_>>();
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        file.write_all(&serde_json::to_vec(&configs)?)?;
        file.as_file().sync_all()?;
        file.persist(directory.join("devices.json"))?;
        Ok(())
    }
    async fn perform(
        &mut self,
        work: &Work,
        journal: &Journal,
        view: &RwLock<Snapshot>,
    ) -> Result<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        let initial = snapshot(view)?;
        for device in &mut self.0 {
            device.io_started = false;
        }
        let results = futures_util::future::join_all(self.0.iter_mut().map(|device| {
            let mut journal = journal.clone();
            let local_view = RwLock::new(initial.clone());
            async move {
                sleep(work.delay).await;
                device.io_started = true;
                if matches!(work.kind, Kind::On) {
                    device.off_attempted.store(false, Ordering::Release);
                }
                if matches!(work.kind, Kind::Off) && device.off_attempted.load(Ordering::Acquire) {
                    return Ok("off_requested".to_owned());
                }
                perform(
                    work,
                    &device.config,
                    &mut device.session,
                    &mut journal,
                    &device.off_attempted,
                    &local_view,
                )
                .await?;
                Ok::<_, anyhow::Error>(snapshot(&local_view)?.power)
            }
        }))
        .await;
        let mut powers = Vec::new();
        let mut errors = Vec::new();
        for result in results {
            match result {
                Ok(power) => powers.push(power),
                Err(error) => errors.push(format!("{error:#}")),
            }
        }
        anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
        edit(view, |s| {
            s.power = if powers.iter().all(|p| p == &powers[0]) {
                powers[0].clone()
            } else {
                "unknown".into()
            };
            s.checked_at = Some(timestamp());
        });
        Ok(())
    }
    async fn idle_step(&mut self) -> Result<()> {
        let waits = self
            .0
            .iter_mut()
            .filter_map(|d| d.session.as_mut())
            .map(|session| Box::pin(session.idle_step()))
            .collect::<Vec<_>>();
        if waits.is_empty() {
            return std::future::pending().await;
        }
        Ok(futures_util::future::select_all(waits).await.0?)
    }
}

fn fingerprint(devices: &[Config]) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for device in devices {
        let mut public = device.clone();
        public.protected_key = None;
        let bytes = serde_json::to_vec(&public)?;
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
        let key = device.key()?;
        digest.update((key.len() as u64).to_le_bytes());
        digest.update(key.as_bytes());
    }
    Ok(digest.finalize().to_vec())
}
impl Work {
    fn target(target: Target, urgent: bool) -> Self {
        Self {
            kind: if target == Target::On {
                Kind::On
            } else {
                Kind::Off
            },
            urgent,
            delay: if urgent || target == Target::On {
                Duration::ZERO
            } else {
                Duration::from_millis(1200)
            },
            reply: None,
        }
    }
}

pub fn directory() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("ProgramData").context("ProgramData unavailable")?)
            .join("LTAC")
            .join("Service"),
    )
}

pub fn dispatch() -> Result<()> {
    let name = ipc::wide(ipc::NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_ptr().cast_mut(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn status(state: u32, error: u32) {
    let handle = HANDLE.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    if !handle.is_null() {
        let value = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: if state == SERVICE_RUNNING {
                SERVICE_ACCEPT_STOP
                    | SERVICE_ACCEPT_SHUTDOWN
                    | SERVICE_ACCEPT_PRESHUTDOWN
                    | SERVICE_ACCEPT_POWEREVENT
            } else {
                0
            },
            dwWin32ExitCode: error,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: if state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING {
                1
            } else {
                0
            },
            dwWaitHint: if state == SERVICE_STOP_PENDING {
                5000
            } else if state == SERVICE_START_PENDING {
                10000
            } else {
                0
            },
        };
        unsafe {
            SetServiceStatus(handle, &value);
        }
    }
}

unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(ipc::wide(ipc::NAME).as_ptr(), Some(control), null())
    };
    if handle.is_null() {
        return;
    }
    HANDLE.store(handle as usize, Ordering::Release);
    status(SERVICE_START_PENDING, 0);
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Create service runtime")
        .and_then(|rt| rt.block_on(run()));
    if let Err(error) = &result
        && let Ok(directory) = directory()
        && let Ok(mut journal) = Journal::open(&directory.join("events.jsonl"))
    {
        let _ = journal.record("service_failed", json!({"error":format!("{error:#}")}));
    }
    status(SERVICE_STOPPED, if result.is_ok() { 0 } else { 1064 });
    // Nonzero process exit also permits SCM failure recovery after initialization.
    if result.is_err() {
        std::process::exit(1);
    }
}

unsafe extern "system" fn control(
    code: u32,
    event_type: u32,
    data: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    let Some(bridge) = BRIDGE.get() else {
        return 0;
    };
    match code {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN | SERVICE_CONTROL_PRESHUTDOWN => {
            crate::lifecycle::begin_ending();
            status(SERVICE_STOP_PENDING, 0);
            // First terminal reason wins; later controls cannot turn a shutdown
            // OFF into an ordinary service stop that would skip the command.
            bridge.stop.send_if_modified(|reason| {
                if *reason == 0 {
                    *reason = code;
                    true
                } else {
                    false
                }
            });
        }
        SERVICE_CONTROL_POWEREVENT => {
            let event = match event_type {
                PBT_APMSUSPEND => Some(Event::Suspend),
                PBT_APMRESUMEAUTOMATIC => Some(Event::ResumeAutomatic),
                PBT_APMRESUMESUSPEND => Some(Event::ResumeInteractive),
                PBT_POWERSETTINGCHANGE if !data.is_null() => {
                    let setting = unsafe { &*data.cast::<POWERBROADCAST_SETTING>() };
                    let guid = setting.PowerSetting;
                    if guid.data1 == GUID_CONSOLE_DISPLAY_STATE.data1
                        && guid.data2 == GUID_CONSOLE_DISPLAY_STATE.data2
                        && guid.data3 == GUID_CONSOLE_DISPLAY_STATE.data3
                        && guid.data4 == GUID_CONSOLE_DISPLAY_STATE.data4
                        && setting.DataLength == 4
                    {
                        let display =
                            unsafe { setting.Data.as_ptr().cast::<u32>().read_unaligned() };
                        CONSOLE_DISPLAY.store(display, Ordering::Release);
                        // Registration supplies one initial snapshot. Boot ON
                        // has already been scheduled independently of EDID.
                        if INITIAL_DISPLAY.swap(false, Ordering::AcqRel) {
                            None
                        } else {
                            match display {
                                0 => Some(Event::DisplayOff),
                                1 => Some(Event::DisplayOn),
                                2 => Some(Event::DisplayDimmed),
                                _ => None,
                            }
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(event) = event {
                let (ack, wait) = if event == Event::Suspend {
                    let (tx, rx) = std::sync::mpsc::sync_channel(1);
                    (Some(tx), Some(rx))
                } else {
                    (None, None)
                };
                if bridge
                    .events
                    .send(Notice {
                        event,
                        suspend_ack: ack,
                    })
                    .is_ok()
                    && let Some(wait) = wait
                {
                    let _ = wait.recv_timeout(Duration::from_millis(1700));
                }
            }
        }
        SERVICE_CONTROL_INTERROGATE => {}
        _ => return 120,
    }
    0
}

unsafe extern "system" fn network_changed(
    _: *const std::ffi::c_void,
    _: *const windows_sys::Win32::NetworkManagement::IpHelper::MIB_IPINTERFACE_ROW,
    _: windows_sys::Win32::NetworkManagement::IpHelper::MIB_NOTIFICATION_TYPE,
) {
    if let Some(bridge) = BRIDGE.get() {
        let _ = bridge.events.send(Notice {
            event: Event::NetworkChanged,
            suspend_ack: None,
        });
    }
}
struct Registrations {
    power: isize,
    network: usize,
}
impl Drop for Registrations {
    fn drop(&mut self) {
        unsafe {
            UnregisterPowerSettingNotification(self.power);
            windows_sys::Win32::NetworkManagement::IpHelper::CancelMibChangeNotify2(
                self.network as _,
            );
        }
    }
}

fn pipe(owner_sid: &str, first: bool) -> Result<NamedPipeServer> {
    anyhow::ensure!(
        owner_sid.starts_with("S-1-")
            && owner_sid
                .chars()
                .all(|c| c.is_ascii_digit() || c == 'S' || c == '-'),
        "Invalid service owner SID"
    );
    // LocalService owns the pipe. Only the configured user and administrators
    // may control it; remote pipe clients are rejected separately.
    let sddl = ipc::wide(&format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)(A;;GRGW;;;{owner_sid})"
    ));
    unsafe {
        let mut descriptor = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                ipc::PIPE,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            );
        LocalFree(descriptor);
        Ok(result?)
    }
}

async fn serve(
    mut server: NamedPipeServer,
    owner_sid: String,
    sender: mpsc::Sender<Envelope>,
    view: Arc<RwLock<Snapshot>>,
) -> Result<()> {
    let slots = Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        server.connect().await?;
        let connected = server;
        server = pipe(&owner_sid, false)?; // Keep an instance alive across accepts.
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let sender = sender.clone();
        let view = view.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = async {
                let mut reader = BufReader::new(connected);
                let mut line = String::new();
                timeout(Duration::from_secs(2), (&mut reader).take(ipc::MAX_FRAME).read_line(&mut line)).await??;
                anyhow::ensure!(line.ends_with('\n'), "Incomplete IPC frame");
                let request: Request = serde_json::from_str(&line)?;
                if matches!(request, Request::Subscribe) {
                    loop {
                        let notified = SNAPSHOT_CHANGED.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        let response = Response { snapshot: snapshot(&view)?, error: None };
                        let mut bytes = serde_json::to_vec(&response)?; bytes.push(b'\n');
                        timeout(Duration::from_secs(2), reader.get_mut().write_all(&bytes)).await??;
                        let mut byte = [0u8; 1];
                        tokio::select! { _ = &mut notified => {}, _ = reader.read(&mut byte) => return Ok(()) }
                    }
                }
                let response = if matches!(request, Request::Snapshot) {
                    Response { snapshot: snapshot(&view)?, error: None }
                } else {
                    let (reply, receiver) = oneshot::channel();
                    sender.try_send(Envelope { request, reply }).map_err(|_| anyhow::anyhow!("Service queue full"))?;
                    timeout(Duration::from_secs(95), receiver).await??
                };
                let mut bytes = serde_json::to_vec(&response)?; bytes.push(b'\n');
                timeout(Duration::from_secs(2), reader.get_mut().write_all(&bytes)).await??;
                Ok::<_, anyhow::Error>(())
            }.await;
        });
    }
}

fn snapshot(view: &RwLock<Snapshot>) -> Result<Snapshot> {
    view.read()
        .map(|s| s.clone())
        .map_err(|_| anyhow::anyhow!("Service snapshot poisoned"))
}
fn edit(view: &RwLock<Snapshot>, f: impl FnOnce(&mut Snapshot)) {
    if let Ok(mut state) = view.write() {
        f(&mut state);
        drop(state);
        SNAPSHOT_CHANGED.notify_waiters();
    }
}
fn reply(work: &mut Work, view: &RwLock<Snapshot>, error: Option<String>) {
    if let Some(reply) = work.reply.take()
        && let Ok(snapshot) = snapshot(view)
    {
        let _ = reply.send(Response { snapshot, error });
    }
}
fn save_settings(settings: &Settings) -> Result<()> {
    use std::io::Write;
    let directory = directory()?;
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    file.write_all(&serde_json::to_vec_pretty(settings)?)?;
    file.as_file().sync_all()?;
    file.persist(directory.join("service.json"))?;
    Ok(())
}
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn current_target(state: &State) -> Target {
    match CONSOLE_DISPLAY.load(Ordering::Acquire) {
        0 => Target::Off,
        1 | 2 => Target::On,
        _ => state.desired_target().unwrap_or(Target::On),
    }
}

async fn perform(
    work: &Work,
    config: &Config,
    cache: &mut Option<Session>,
    journal: &mut Journal,
    attempted: &AtomicBool,
    view: &RwLock<Snapshot>,
) -> Result<()> {
    match work.kind {
        Kind::On => {
            // A brief display flicker must not send WoL to an already active TV.
            let already_on = if let Some(session) = cache.as_mut() {
                matches!(
                    timeout(Duration::from_millis(250), session.power_state()).await,
                    Ok(Ok(PowerState::On))
                )
            } else {
                false
            };
            if !already_on {
                *cache =
                    Some(controller::wake_session(config, journal, Duration::from_secs(90)).await?);
            }
            edit(view, |s| {
                s.power = "on".into();
                s.checked_at = Some(timestamp());
            });
        }
        Kind::Off => {
            let off = async {
                if cache.is_none() {
                    *cache = Some(controller::paired_session(config).await?);
                }
                let session = cache.as_mut().context("Missing OFF session")?;
                let evidence = session.power_off_tracked(Some(attempted)).await?;
                let _ = journal.record(
                    "off_evidence",
                    json!({"response":evidence, "physical_state":"not_observed"}),
                );
                Ok::<_, anyhow::Error>(())
            };
            timeout(
                if work.urgent {
                    Duration::from_millis(1400)
                } else {
                    Duration::from_secs(12)
                },
                off,
            )
            .await
            .context("OFF budget expired")??;
            *cache = None;
            edit(view, |s| {
                s.power = "off_requested".into();
                s.checked_at = Some(timestamp());
            });
        }
        Kind::Refresh => {
            let refresh = async {
                if cache.is_none() {
                    *cache = Some(controller::paired_session(config).await?);
                }
                let power = cache
                    .as_mut()
                    .context("Missing status session")?
                    .power_state()
                    .await?;
                edit(view, |s| {
                    s.power = match power {
                        PowerState::On => "on",
                        PowerState::Standby => "standby",
                        PowerState::ScreenOff => "screen_off",
                        PowerState::Unknown(_) => "unknown",
                    }
                    .into();
                    s.checked_at = Some(timestamp());
                });
                Ok::<_, anyhow::Error>(())
            };
            timeout(Duration::from_secs(12), refresh)
                .await
                .context("Status budget expired")??;
        }
    }
    Ok(())
}

fn event_work(
    notice: Notice,
    state: &mut State,
    automatic: bool,
    view: &RwLock<Snapshot>,
) -> (Option<Work>, Option<std::sync::mpsc::SyncSender<()>>) {
    let target = state.apply(notice.event);
    let work = if automatic {
        if notice.event == Event::Suspend {
            Some(Work::target(Target::Off, true))
        } else if notice.event == Event::NetworkChanged
            && snapshot(view).is_ok_and(|s| s.power == "unknown")
        {
            None
        } else {
            target.map(|target| Work::target(target, false))
        }
    } else {
        None
    };
    (work, notice.suspend_ack)
}

fn command_work(
    envelope: Envelope,
    settings: &mut Settings,
    view: &RwLock<Snapshot>,
    active: bool,
) -> Option<Work> {
    let Envelope { request, reply } = envelope;
    let kind = match request {
        Request::On => Kind::On,
        Request::Off => Kind::Off,
        Request::Refresh if !active => Kind::Refresh,
        Request::Automatic { enabled } => {
            let previous = settings.automatic;
            settings.automatic = enabled;
            let error = save_settings(settings).err().map(|e| format!("{e:#}"));
            if error.is_some() {
                settings.automatic = previous;
            }
            edit(view, |s| s.automatic = settings.automatic);
            if let Ok(snapshot) = snapshot(view) {
                let _ = reply.send(Response { snapshot, error });
            }
            // A disabled auto policy cancels its current operation below.
            return None;
        }
        _ => {
            if let Ok(snapshot) = snapshot(view) {
                let _ = reply.send(Response {
                    snapshot,
                    error: None,
                });
            }
            return None;
        }
    };
    Some(Work {
        kind,
        urgent: false,
        delay: Duration::ZERO,
        reply: Some(reply),
    })
}

async fn run() -> Result<()> {
    let started = Instant::now();
    let directory = directory()?;
    let _lock: File = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("service.lock"))?;
    _lock
        .try_lock()
        .context("Another service owns power control")?;
    let mut journal = Journal::open(&directory.join("events.jsonl"))?.best_effort();
    let mut settings: Settings =
        serde_json::from_slice(&std::fs::read(directory.join("service.json"))?)?;
    let devices_path = directory.join("devices.json");
    let devices = if devices_path.exists() {
        serde_json::from_slice(&std::fs::read(devices_path)?)?
    } else if directory.join("config.json").exists() {
        vec![Config::load(&directory.join("config.json"))?]
    } else {
        Vec::new()
    };
    let mut fleet = Fleet::new(devices)?;
    anyhow::ensure!(
        !settings.automatic || fleet.wake_available(),
        "TV MAC address missing"
    );
    let view = Arc::new(RwLock::new(Snapshot {
        power: "unknown".into(),
        automatic: settings.automatic,
        busy: false,
        checked_at: None,
        error: None,
    }));
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (stop_tx, mut stop) = watch::channel(0);
    BRIDGE
        .set(Bridge {
            events: events_tx,
            stop: stop_tx,
        })
        .map_err(|_| anyhow::anyhow!("Service bridge already exists"))?;
    let server = pipe(&settings.owner_sid, true)?;
    let power = unsafe {
        RegisterPowerSettingNotification(
            HANDLE.load(Ordering::Acquire) as _,
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_SERVICE_HANDLE,
        )
    };
    anyhow::ensure!(
        power != 0,
        "Register console display notifications: {}",
        std::io::Error::last_os_error()
    );
    let mut network = null_mut();
    let code = unsafe {
        windows_sys::Win32::NetworkManagement::IpHelper::NotifyIpInterfaceChange(
            windows_sys::Win32::Networking::WinSock::AF_INET,
            Some(network_changed),
            null(),
            false,
            &mut network,
        )
    };
    if code != 0 {
        unsafe {
            UnregisterPowerSettingNotification(power);
        }
        bail!("Register network notifications: {code}");
    }
    let _registrations = Registrations {
        power,
        network: network as usize,
    };
    let (commands_tx, mut commands) = mpsc::channel(16);
    let mut server_task = tokio::spawn(serve(
        server,
        settings.owner_sid.clone(),
        commands_tx,
        view.clone(),
    ));
    status(SERVICE_RUNNING, 0);
    let _ = journal.record("service_control_ready", json!({"elapsed_ms":started.elapsed().as_millis(), "automatic":settings.automatic, "ui_required":false}));
    let mut state = State::default();
    state.apply(Event::ResumeInteractive);
    let skip_boot_wake = directory.join("skip-boot-wake");
    let skip_boot = skip_boot_wake.exists();
    if skip_boot {
        std::fs::remove_file(skip_boot_wake)?;
    }
    let mut work = (settings.automatic && !fleet.0.is_empty() && !skip_boot)
        .then(|| Work::target(Target::On, true));
    let mut configuration: Option<Envelope> = None;
    let mut suspend_ack: Option<std::sync::mpsc::SyncSender<()>> = None;
    let mut repair_attempts = 0u32;
    loop {
        if *stop.borrow() != 0 {
            break;
        }
        if let Some(envelope) = configuration.take()
            && let Request::Configure { devices } = envelope.request
        {
            let result = Fleet::new(devices).and_then(|next| {
                anyhow::ensure!(
                    !settings.automatic || next.wake_available(),
                    "기기 정보에서 MAC 주소를 확인해주세요."
                );
                next.save()?;
                Ok(next)
            });
            let error = match result {
                Ok(next) => {
                    if fleet.fingerprint()? == next.fingerprint()? {
                        let _ = envelope.reply.send(Response {
                            snapshot: snapshot(&view)?,
                            error: None,
                        });
                        continue;
                    }
                    fleet = next;
                    repair_attempts = 0;
                    edit(&view, |s| {
                        s.power = "unknown".into();
                        s.error = None;
                        s.busy = false;
                    });
                    let target = current_target(&state);
                    state.set_desired(target);
                    work = (settings.automatic && !fleet.0.is_empty())
                        .then(|| Work::target(target, false));
                    None
                }
                Err(error) => Some(format!("{error:#}")),
            };
            let _ = envelope.reply.send(Response {
                snapshot: snapshot(&view)?,
                error,
            });
        }
        if let Some(mut current) = work.take() {
            let current_ack = suspend_ack.take();
            if matches!(current.kind, Kind::Off)
                && fleet.all_off_attempted()
                && current.reply.is_none()
            {
                if let Some(ack) = current_ack {
                    let _ = ack.try_send(());
                }
                let _ = journal.record(
                    "service_off_duplicate_skipped",
                    json!({"previous_off_may_have_been_sent":true}),
                );
                continue;
            }
            let is_auto = current.reply.is_none();
            edit(&view, |s| {
                s.busy = true;
                s.error = None;
                s.power = match current.kind {
                    Kind::On => "waking",
                    Kind::Off => "turning_off",
                    Kind::Refresh => &s.power,
                }
                .into();
            });
            let _ = journal.record("service_action_started", json!({"kind":match current.kind { Kind::On => "on", Kind::Off => "off", Kind::Refresh => "refresh" }, "automatic":is_auto}));
            let action_log = journal.clone();
            let fleet_empty = fleet.0.is_empty();
            let wake_available = fleet.wake_available();
            let active_fingerprint = fleet.fingerprint()?;
            let completed = {
                let operation = fleet.perform(&current, &action_log, &view);
                tokio::pin!(operation);
                loop {
                    tokio::select! {
                        biased;
                        _ = stop.changed() => break None,
                        result = &mut server_task => { result??; bail!("Service IPC stopped unexpectedly"); }
                        notice = events.recv() => {
                            let notice = notice.context("Service events closed")?;
                            let _ = journal.record("windows_event", json!({"event":notice.event}));
                            let (next, ack) = event_work(notice, &mut state, settings.automatic && !fleet_empty, &view);
                            if let Some(next) = next { work = Some(next); if let Some(old) = suspend_ack.take() { let _ = old.try_send(()); } suspend_ack = ack; break None; }
                            if let Some(ack) = ack { let _ = ack.try_send(()); }
                        }
                        envelope = commands.recv() => {
                            let envelope = envelope.context("Service commands closed")?;
                            if let Request::Configure { devices } = &envelope.request {
                                match fingerprint(devices) {
                                    Ok(value) if value == active_fingerprint => {
                                        let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: None });
                                        continue;
                                    },
                                    Err(error) => {
                                        let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: Some(error.to_string()) });
                                        continue;
                                    },
                                    _ => {}
                                }
                                configuration = Some(envelope); work = None; break None;
                            }
                            if matches!(envelope.request, Request::Automatic { enabled: true }) && !wake_available {
                                let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: Some("기기 정보에서 MAC 주소를 확인해주세요.".into()) });
                                continue;
                            }
                            match envelope.request { Request::On => state.set_desired(Target::On), Request::Off => state.set_desired(Target::Off), _ => {} }
                            let was_auto = settings.automatic;
                            let next = command_work(envelope, &mut settings, &view, true);
                            if !was_auto && settings.automatic && !fleet_empty {
                                let target = current_target(&state);
                                state.set_desired(target);
                                work = Some(Work::target(target, true));
                                break None;
                            }
                            if next.is_some() || (is_auto && was_auto && !settings.automatic) { work = next; break None; }
                        }
                        result = &mut operation => break Some(result),
                    }
                }
            };
            let off_attempted = fleet.all_off_attempted();
            if let Some(ack) = current_ack {
                let _ = ack.try_send(());
            }
            edit(&view, |s| s.busy = false);
            if let Some(result) = completed {
                let error = result.err().map(|e| format!("{e:#}"));
                let _ = journal.record("service_action_finished", json!({"success":error.is_none(), "error":error, "off_may_have_been_sent":off_attempted}));
                if error.is_some() {
                    fleet.clear_sessions();
                    edit(&view, |s| {
                        s.power = "unknown".into();
                        s.error = error.clone();
                    });
                    if is_auto
                        && matches!(current.kind, Kind::Refresh)
                        && settings.automatic
                        && state.desired_target() == Some(Target::On)
                        && repair_attempts < 3
                    {
                        repair_attempts += 1;
                        work = Some(Work {
                            kind: Kind::Refresh,
                            urgent: false,
                            delay: Duration::from_secs(u64::from(repair_attempts)),
                            reply: None,
                        });
                    }
                } else if matches!(current.kind, Kind::On | Kind::Refresh) {
                    repair_attempts = 0;
                }
                reply(&mut current, &view, error);
            } else {
                edit(&view, |s| {
                    s.power = if off_attempted {
                        "off_requested"
                    } else {
                        "unknown"
                    }
                    .into()
                });
                reply(
                    &mut current,
                    &view,
                    Some("Request superseded by a newer power event".into()),
                );
                // An OFF cancelled during debounce has not touched its socket.
                // Keep that connection for the returning ON check without WoL.
                fleet.cancel_sessions();
            }
            continue;
        }
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            result = &mut server_task => { result??; bail!("Service IPC stopped unexpectedly"); }
            notice = events.recv() => {
                let notice = notice.context("Service events closed")?;
                let _ = journal.record("windows_event", json!({"event":notice.event}));
                // A late NIC becoming available re-arms a failed desired ON,
                // while an OFF policy never wakes the TV merely for discovery.
                let network_retry = notice.event == Event::NetworkChanged && settings.automatic && state.desired_target() == Some(Target::On) && snapshot(&view)?.power == "unknown";
                let (next, ack) = event_work(notice, &mut state, settings.automatic && !fleet.0.is_empty(), &view);
                work = if network_retry { Some(Work::target(Target::On, true)) } else { next };
                if work.is_some() { suspend_ack = ack; } else if let Some(ack) = ack { let _ = ack.try_send(()); }
            }
            envelope = commands.recv() => {
                let envelope = envelope.context("Service commands closed")?;
                if matches!(envelope.request, Request::Configure { .. }) {
                    configuration = Some(envelope); continue;
                }
                if matches!(envelope.request, Request::Automatic { enabled: true }) && !fleet.wake_available() {
                    let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: Some("기기 정보에서 MAC 주소를 확인해주세요.".into()) });
                    continue;
                }
                match envelope.request { Request::On => state.set_desired(Target::On), Request::Off => state.set_desired(Target::Off), _ => {} }
                let was_auto = settings.automatic;
                work = command_work(envelope, &mut settings, &view, false);
                if !was_auto && settings.automatic && !fleet.0.is_empty() {
                    let target = match CONSOLE_DISPLAY.load(Ordering::Acquire) {
                        0 => Target::Off, 1 | 2 => Target::On,
                        _ => state.desired_target().unwrap_or(Target::On),
                    };
                    state.set_desired(target);
                    work = Some(Work::target(target, true));
                }
            }
            result = fleet.idle_step() => {
                if let Err(error) = result {
                    fleet.clear_sessions(); edit(&view, |s| s.power = "unknown".into());
                    let _ = journal.record("idle_connection_closed", json!({"error":error.to_string()}));
                    if settings.automatic && state.desired_target() == Some(Target::On) {
                        repair_attempts = 1;
                        work = Some(Work { kind: Kind::Refresh, urgent: false, delay: Duration::from_millis(250), reply: None });
                    }
                }
            }
        }
    }
    server_task.abort();
    if let Some(ack) = suspend_ack {
        let _ = ack.try_send(());
    }
    let reason = *stop.borrow();
    if reason != SERVICE_CONTROL_STOP && settings.automatic && !fleet.all_off_attempted() {
        let work = Work {
            kind: Kind::Off,
            urgent: false,
            delay: Duration::ZERO,
            reply: None,
        };
        // Enclosing 3.5 s deadline includes connect/registration and response.
        // No polling or indefinite shutdown.
        let result = timeout(
            Duration::from_millis(3500),
            fleet.perform(&work, &journal, &view),
        )
        .await;
        let _ = journal.record("service_shutdown_off", json!({"success":matches!(result, Ok(Ok(()))), "off_may_have_been_sent":fleet.all_off_attempted(), "physical_state":"not_observed"}));
    } else {
        let _ = journal.record("service_shutdown_off_skipped", json!({"ordinary_stop":reason == SERVICE_CONTROL_STOP, "automatic":settings.automatic, "previous_off_may_have_been_sent":fleet.all_off_attempted()}));
    }
    let _ = journal.record("service_stopped", json!({"reason":reason}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    fn config(last: u8) -> Config {
        let mut config = Config {
            ip: std::net::Ipv4Addr::new(127, 0, 0, last),
            mac: Some("02:00:00:00:00:01".parse().unwrap()),
            broadcast: "127.0.0.1".parse().unwrap(),
            transport: crate::config::Transport::Ws,
            device: None,
            source_path: None,
            wake_verified: false,
            protected_key: None,
            certificate_sha256: None,
        };
        config.set_key("local-test-key").unwrap();
        config.for_service().unwrap()
    }
    fn view() -> RwLock<Snapshot> {
        RwLock::new(Snapshot {
            power: "unknown".into(),
            automatic: true,
            busy: false,
            checked_at: None,
            error: None,
        })
    }

    #[test]
    fn configuration_rejects_duplicates_invalid_credentials_and_excess_devices() {
        assert!(Fleet::new(vec![config(1), config(1)]).is_err());
        let mut invalid = config(1);
        invalid.protected_key.as_mut().unwrap()[0] ^= 0xff;
        assert!(Fleet::new(vec![invalid]).is_err());
        assert!(Fleet::new((1..=17).map(config).collect()).is_err());
        let mut missing_mac = config(1);
        missing_mac.mac = None;
        assert!(!Fleet::new(vec![missing_mac]).unwrap().wake_available());
        assert!(Fleet::new(Vec::new()).unwrap().wake_available());
    }

    #[test]
    fn pairing_export_does_not_claim_physical_verification() {
        let exported = config(1);
        assert!(!exported.wake_verified);
        assert_eq!(exported.key().unwrap(), "local-test-key");
        assert!(exported.source_path.is_none());
        let again = config(1);
        assert_eq!(
            fingerprint(&[exported]).unwrap(),
            fingerprint(&[again]).unwrap()
        );
    }

    #[tokio::test]
    async fn cancelled_display_off_keeps_connection_and_active_on_sends_no_wol() {
        let (session, mut peer) = crate::ssap::tests::peer().await;
        let mut fleet = Fleet::new(vec![config(1)]).unwrap();
        fleet.0[0].session = Some(session);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let journal = Journal::open(&path).unwrap();
        let view = view();
        assert!(
            timeout(
                Duration::from_millis(20),
                fleet.perform(&Work::target(Target::Off, false), &journal, &view)
            )
            .await
            .is_err()
        );
        fleet.cancel_sessions();
        assert!(fleet.0[0].session.is_some());
        assert!(!fleet.all_off_attempted());
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
        });
        fleet
            .perform(&Work::target(Target::On, true), &journal, &view)
            .await
            .unwrap();
        remote.await.unwrap();
        assert_eq!(snapshot(&view).unwrap().power, "on");
        assert!(!std::fs::read_to_string(path).unwrap().contains("wol_sent"));
    }

    #[tokio::test]
    async fn fleet_sends_off_concurrently_and_never_repeats_a_transmitted_off() {
        let (first, mut first_peer) = crate::ssap::tests::peer().await;
        let (second, mut second_peer) = crate::ssap::tests::peer().await;
        let (second_seen, wait_second) = oneshot::channel();
        let slow = tokio::spawn(async move {
            let frame = first_peer.next().await.unwrap().unwrap();
            let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
            // This response depends on the OTHER TV receiving OFF. Sequential
            // execution would deadlock until the enclosing regression deadline.
            wait_second.await.unwrap();
            first_peer
                .send(Message::Text(
                    json!({"id":frame["id"],"type":"response","payload":{"returnValue":true}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let fast = tokio::spawn(async move {
            let frame = second_peer.next().await.unwrap().unwrap();
            let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
            second_seen.send(()).unwrap();
            second_peer
                .send(Message::Text(
                    json!({"id":frame["id"],"type":"response","payload":{"returnValue":true}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let mut fleet = Fleet::new(vec![config(1), config(2)]).unwrap();
        fleet.0[0].session = Some(first);
        fleet.0[1].session = Some(second);
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("test.jsonl")).unwrap();
        let view = view();
        let off = Work::target(Target::Off, true);
        timeout(Duration::from_secs(2), fleet.perform(&off, &journal, &view))
            .await
            .unwrap()
            .unwrap();
        slow.await.unwrap();
        fast.await.unwrap();
        assert!(fleet.all_off_attempted());
        assert_eq!(snapshot(&view).unwrap().power, "off_requested");
        // Sessions were dropped and no server is available for another command.
        timeout(
            Duration::from_millis(50),
            fleet.perform(&off, &journal, &view),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn suspend_deadline_preserves_off_evidence_after_timeout() {
        let (session, mut peer) = crate::ssap::tests::peer().await;
        let remote = tokio::spawn(async move {
            let frame = peer.next().await.unwrap().unwrap();
            assert!(frame.to_text().unwrap().contains(crate::ssap::POWER_OFF));
            std::future::pending::<()>().await;
        });
        let mut fleet = Fleet::new(vec![config(1)]).unwrap();
        fleet.0[0].session = Some(session);
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("test.jsonl")).unwrap();
        let started = Instant::now();
        assert!(
            fleet
                .perform(&Work::target(Target::Off, true), &journal, &view())
                .await
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(fleet.all_off_attempted());
        remote.abort();
    }

    #[tokio::test]
    async fn idle_disconnect_from_any_tv_is_observed_without_polling() {
        let (first, _first_peer) = crate::ssap::tests::peer().await;
        let (second, second_peer) = crate::ssap::tests::peer().await;
        let mut fleet = Fleet::new(vec![config(1), config(2)]).unwrap();
        fleet.0[0].session = Some(first);
        fleet.0[1].session = Some(second);
        drop(second_peer);
        assert!(
            timeout(Duration::from_secs(1), fleet.idle_step())
                .await
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn disabled_policy_and_maintenance_resume_do_not_schedule_wake() {
        let view = view();
        let mut state = State::default();
        let (work, _) = event_work(
            Notice {
                event: Event::DisplayOn,
                suspend_ack: None,
            },
            &mut state,
            false,
            &view,
        );
        assert!(work.is_none());
        state.apply(Event::Suspend);
        let (work, _) = event_work(
            Notice {
                event: Event::ResumeAutomatic,
                suspend_ack: None,
            },
            &mut state,
            true,
            &view,
        );
        assert!(work.is_none());
        let (work, _) = event_work(
            Notice {
                event: Event::ResumeInteractive,
                suspend_ack: None,
            },
            &mut state,
            true,
            &view,
        );
        assert!(matches!(work.unwrap().kind, Kind::On));
    }
}
