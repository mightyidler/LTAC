//! Native, UI-independent boot/shutdown controller. SCM callbacks only publish
//! events; all TV I/O belongs to one cancellable worker on a current-thread RT.
use crate::{
    config::Config,
    controller,
    journal::Journal,
    service_ipc::{self as ipc, Request, Response, ShutdownPhase, Snapshot},
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
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
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
static CHECKPOINT: AtomicU32 = AtomicU32::new(0);
static STOP_RECEIVED_AT: AtomicU64 = AtomicU64::new(0);

const SHUTDOWN_GRACE: Duration = Duration::from_secs(4);
// A lost cancellation relay must not indefinitely disable ordinary power sync.
const SHUTDOWN_QUERY_LEASE: Duration = Duration::from_secs(60);
// Existing installation reserves 6 s for preshutdown. Leave time for cleanup.
const PROTECTED_SHUTDOWN_BUDGET: Duration = Duration::from_millis(5600);
static STOP_STARTED: OnceLock<Instant> = OnceLock::new();

struct Bridge {
    events: crate::sync::NoticeSender,
    stop: watch::Sender<u32>,
    shutdown: watch::Sender<ShutdownPhase>,
}
#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub owner_sid: String,
    pub automatic: bool,
    #[serde(default)]
    pub keep_tv_on: bool,
}
struct Envelope {
    request: Request,
    reply: oneshot::Sender<Response>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Screen(crate::screen_protection::Signal),
    On,
    Off,
    Refresh,
}
struct Work {
    kind: Kind,
    check_input: bool,
    urgent: bool,
    delay: Duration,
    shutdown: bool,
    reply: Option<oneshot::Sender<Response>>,
}

// All devices share one policy and cancellation boundary, but retain separate
// authenticated sockets and OFF evidence. A slow TV never delays another TV.
struct DeviceControl {
    panel: crate::screen_protection::Panel,
    config: Config,
    session: Option<Session>,
    off_attempted: AtomicBool,
    io_started: bool,
}
struct Fleet(Vec<DeviceControl>, Vec<u8>);
impl Fleet {
    fn new(mut devices: Vec<Config>) -> Result<Self> {
        anyhow::ensure!(devices.len() <= 16, "At most 16 TVs are supported");
        for (index, config) in devices.iter().enumerate() {
            config.validate()?;
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
        let configured = fingerprint(&devices)?; // Also validates every credential once.
        Ok(Self(
            devices
                .into_iter()
                .map(|config| DeviceControl {
                    panel: Default::default(),
                    config,
                    session: None,
                    off_attempted: AtomicBool::new(false),
                    io_started: false,
                })
                .collect(),
            configured,
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
    #[cfg(test)]
    fn clear_sessions(&mut self) {
        for device in &mut self.0 {
            device.session = None;
        }
    }
    fn cancel_sessions(&mut self) {
        for device in &mut self.0 {
            if device.io_started {
                device.session = None;
                device.panel.invalidate();
            }
        }
    }
    fn fingerprint(&self) -> Result<Vec<u8>> {
        // Runtime address recovery must not make an unchanged desktop export
        // look like a new user configuration and restart power control.
        Ok(self.1.clone())
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
            let initial_power = initial.power.clone();
            async move {
                sleep(work.delay).await;
                device.io_started = true;
                if let Kind::Screen(signal) = work.kind {
                    let result = device
                        .panel
                        .apply(signal, &device.config, &mut device.session, &mut journal)
                        .await;
                    device.io_started = false;
                    return result.map(|_| initial_power);
                }
                device.panel.invalidate();
                if matches!(work.kind, Kind::On) {
                    anyhow::ensure!(!crate::lifecycle::is_ending(), "Windows session is ending");
                    device.off_attempted.store(false, Ordering::Release);
                }
                if matches!(work.kind, Kind::Off) && device.off_attempted.load(Ordering::Acquire) {
                    if work.shutdown {
                        let _ = journal.record("shutdown_device_skipped", json!({"ip":device.config.ip,"reason":"previous_off_may_have_been_sent"}));
                    }
                    return Ok("off_requested".to_owned());
                }
                let result = perform(
                    work,
                    &mut device.config,
                    &mut device.session,
                    &mut journal,
                    &device.off_attempted,
                    &local_view,
                    work.check_input && initial.keep_tv_on,
                )
                .await;
                if work.shutdown {
                    let _ = journal.record("shutdown_device_finished", json!({"ip":device.config.ip,"completed":result.is_ok(),"error":result.as_ref().err().map(|e| format!("{e:#}")),"off_may_have_been_sent":device.off_attempted.load(Ordering::Acquire),"physical_state":"not_observed"}));
                }
                if result.is_err() {
                    device.session = None;
                }
                if result.is_ok() && matches!(work.kind, Kind::On) {
                    device.panel = Default::default();
                }
                // Other devices may still be working when a new event arrives.
                // A completed exchange no longer needs cancellation cleanup.
                device.io_started = false;
                result?;
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
        let (result, index) = {
            let waits = self
                .0
                .iter_mut()
                .enumerate()
                .filter_map(|(index, d)| {
                    d.session
                        .as_mut()
                        .map(|session| Box::pin(async move { (session.idle_step().await, index) }))
                })
                .collect::<Vec<_>>();
            if waits.is_empty() {
                return std::future::pending().await;
            }
            futures_util::future::select_all(waits).await.0
        };
        if result.is_err() {
            self.0[index].session = None;
            self.0[index].panel.disconnected();
        }
        Ok(result?)
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
    fn same_operation(&self, other: &Self) -> bool {
        self.kind == other.kind && (self.kind != Kind::Off || self.check_input == other.check_input)
    }
    fn target(target: Target, urgent: bool) -> Self {
        Self {
            check_input: true,
            kind: if target == Target::On {
                Kind::On
            } else {
                Kind::Off
            },
            urgent,
            delay: Duration::ZERO,
            shutdown: false,
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
                CHECKPOINT.fetch_add(1, Ordering::AcqRel) + 1
            } else {
                0
            },
            dwWaitHint: if state == SERVICE_STOP_PENDING {
                6000
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
                    STOP_RECEIVED_AT.store(timestamp(), Ordering::Release);
                    let _ = STOP_STARTED.set(Instant::now());
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
                        preceding: None,
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
            preceding: None,
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
    shutdown: watch::Sender<ShutdownPhase>,
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
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = async {
                let mut reader = BufReader::new(connected);
                let mut line = String::new();
                timeout(Duration::from_secs(2), (&mut reader).take(ipc::MAX_FRAME).read_line(&mut line)).await??;
                anyhow::ensure!(line.ends_with('\n'), "Incomplete IPC frame");
                let request: Request = serde_json::from_str(&line)?;
                // Intent bypasses the command queue so an in-flight input query
                // is gated before the native window returns to Windows.
                if let Request::Shutdown { phase } = request {
                    publish_shutdown(&shutdown, phase);
                    let response = Response { snapshot: snapshot(&view)?, error: None };
                    let mut bytes = serde_json::to_vec(&response)?; bytes.push(b'\n');
                    timeout(Duration::from_secs(2), reader.get_mut().write_all(&bytes)).await??;
                    return Ok(());
                }
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

fn publish_shutdown(sender: &watch::Sender<ShutdownPhase>, phase: ShutdownPhase) {
    sender.send_if_modified(|current| {
        if *current == ShutdownPhase::Confirm || *current == phase {
            return false;
        }
        *current = phase;
        true
    });
}

struct QueryLease {
    deadline: tokio::time::Instant,
    expired: bool,
}
impl Default for QueryLease {
    fn default() -> Self {
        Self {
            deadline: tokio::time::Instant::now(),
            expired: false,
        }
    }
}
impl QueryLease {
    /// Only an explicit cancellation replays the deferred desired target.
    fn observe(&mut self, phase: ShutdownPhase) -> bool {
        if phase == ShutdownPhase::Query {
            self.deadline = tokio::time::Instant::now() + SHUTDOWN_QUERY_LEASE;
            self.expired = false;
        }
        phase == ShutdownPhase::Cancel && !std::mem::take(&mut self.expired)
    }
    fn expire(&mut self, sender: &watch::Sender<ShutdownPhase>) -> bool {
        self.expired = sender.send_if_modified(|phase| {
            if *phase != ShutdownPhase::Query {
                return false;
            }
            *phase = ShutdownPhase::Cancel;
            true
        });
        self.expired
    }
}

fn protected_shutdown_pending() -> bool {
    BRIDGE.get().is_some_and(|bridge| {
        *bridge.shutdown.borrow() != ShutdownPhase::Cancel || *bridge.stop.borrow() != 0
    })
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
fn reply(
    work: &mut Work,
    followers: Vec<oneshot::Sender<Response>>,
    view: &RwLock<Snapshot>,
    error: Option<String>,
) {
    for reply in work.reply.take().into_iter().chain(followers) {
        if let Ok(snapshot) = snapshot(view) {
            let _ = reply.send(Response {
                snapshot,
                error: error.clone(),
            });
        }
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
    config: &mut Config,
    cache: &mut Option<Session>,
    journal: &mut Journal,
    attempted: &AtomicBool,
    view: &RwLock<Snapshot>,
    check_input: bool,
) -> Result<()> {
    match work.kind {
        Kind::Screen(_) => unreachable!("Panel requests use the retained device connection"),
        Kind::On => {
            // Reuse a healthy socket instead of waking an already active TV.
            let already_on = if let Some(session) = cache.as_mut() {
                matches!(
                    timeout(Duration::from_millis(250), session.power_state()).await,
                    Ok(Ok(PowerState::On))
                )
            } else {
                false
            };
            if !already_on {
                let (resolved, session) =
                    controller::wake_session_resolved(config, journal, Duration::from_secs(90))
                        .await?;
                *config = resolved;
                *cache = Some(session);
            }
            edit(view, |s| {
                s.power = "on".into();
                s.checked_at = Some(timestamp());
            });
        }
        Kind::Off => {
            if check_input && config.pc_hdmi.is_none() {
                let _ = journal.record("off_skipped_input", json!({"reason":"pc_hdmi_not_set"}));
                edit(view, |s| s.power = "unknown".into());
                return Ok(());
            }
            let off = async {
                if cache.is_none() {
                    let (resolved, session) = controller::paired_session_resolved(config).await?;
                    *config = resolved;
                    *cache = Some(session);
                }
                let session = cache.as_mut().context("Missing OFF session")?;
                if work.shutdown && check_input {
                    let _ = journal.record("shutdown_foreground_query", json!({"ip":config.ip}));
                }
                if check_input
                    && !controller::protected_input_active(session, config, journal).await
                {
                    return Ok(false);
                }
                // Intent can arrive while the foreground request is in flight.
                // Only the final SCM worker may send a protected shutdown OFF.
                if check_input && !work.shutdown && protected_shutdown_pending() {
                    let _ = journal.record("off_deferred_shutdown", json!({"ip":config.ip}));
                    return Ok(false);
                }
                let evidence = session.power_off_tracked(Some(attempted)).await?;
                let _ = journal.record(
                    "off_evidence",
                    json!({"response":evidence, "physical_state":"not_observed"}),
                );
                Ok::<_, anyhow::Error>(true)
            };
            let sent = timeout(
                if work.urgent {
                    Duration::from_millis(1400)
                } else {
                    Duration::from_secs(12)
                },
                off,
            )
            .await
            .context("OFF budget expired");
            let sent = match sent {
                Ok(Ok(sent)) => sent,
                // Protected OFF never turns a failed input check into an OFF.
                result if check_input && !attempted.load(Ordering::Acquire) => {
                    let (reason, error) = match result {
                        Err(error) => ("off_budget_expired", format!("{error:#}")),
                        Ok(Err(error)) => ("connection_unavailable", format!("{error:#}")),
                        Ok(Ok(_)) => unreachable!(),
                    };
                    let _ = journal.record(
                        "off_skipped_input",
                        json!({"ip":config.ip,"reason":reason,"error":error}),
                    );
                    false
                }
                result => result??,
            };
            *cache = None;
            edit(view, |s| {
                s.power = if sent { "off_requested" } else { "unknown" }.into();
                s.checked_at = Some(timestamp());
            });
        }
        Kind::Refresh => {
            let refresh = async {
                if cache.is_none() {
                    let (resolved, session) = controller::paired_session_resolved(config).await?;
                    *config = resolved;
                    *cache = Some(session);
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
    shutdown_pending: bool,
) -> (Option<Work>, Option<std::sync::mpsc::SyncSender<()>>) {
    let target = state.apply_notice(&notice);
    let protected_pending = shutdown_pending && snapshot(view).is_ok_and(|s| s.keep_tv_on);
    let work = if automatic && !protected_pending {
        if matches!(notice.event, Event::Suspend | Event::SessionEnding) {
            Some(Work::target(Target::Off, true))
        } else if notice.event == Event::NetworkChanged
            && snapshot(view).is_ok_and(|s| s.power == "unknown")
        {
            None
        } else {
            target.map(|target| Work::target(target, state.is_ending()))
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
        Request::ScreenProtection { signal, .. } => Kind::Screen(signal),
        Request::On => Kind::On,
        Request::Off | Request::Sleep => Kind::Off,
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
        Request::KeepTvOn { enabled } => {
            let previous = settings.keep_tv_on;
            settings.keep_tv_on = enabled;
            let error = save_settings(settings).err().map(|e| format!("{e:#}"));
            if error.is_some() {
                settings.keep_tv_on = previous;
            }
            edit(view, |s| s.keep_tv_on = settings.keep_tv_on);
            if let Ok(snapshot) = snapshot(view) {
                let _ = reply.send(Response { snapshot, error });
            }
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
        check_input: matches!(request, Request::Sleep),
        urgent: false,
        delay: Duration::ZERO,
        shutdown: false,
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
        keep_tv_on: settings.keep_tv_on,
        busy: false,
        checked_at: None,
        error: None,
    }));
    let (events_tx, mut events) = crate::sync::notice_channel();
    let (stop_tx, mut stop) = watch::channel(0);
    let (shutdown_tx, mut shutdown) = watch::channel(ShutdownPhase::Cancel);
    BRIDGE
        .set(Bridge {
            events: events_tx,
            stop: stop_tx,
            shutdown: shutdown_tx.clone(),
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
        shutdown_tx.clone(),
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
    let mut screen_sequence = crate::screen_protection::Sequence::default();
    let mut screen_owner_changed = false;
    let mut screen_session = crate::screen_protection::SessionPolicy::default();
    let mut query_lease = QueryLease::default();
    loop {
        if screen_owner_changed {
            for device in &mut fleet.0 {
                device.panel = Default::default();
            }
            screen_owner_changed = false;
        }
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
                    if let Err(error) = next.save() {
                        let _ = envelope.reply.send(Response {
                            snapshot: snapshot(&view)?,
                            error: Some(format!("{error:#}")),
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
        if settings.keep_tv_on
            && *shutdown.borrow() != ShutdownPhase::Cancel
            && work.as_ref().is_some_and(|work| work.reply.is_none())
        {
            work = None;
            if let Some(ack) = suspend_ack.take() {
                let _ = ack.try_send(());
            }
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
                    Kind::Refresh | Kind::Screen(_) => &s.power,
                }
                .into();
            });
            let _ = journal.record("service_action_started", json!({"kind":match current.kind { Kind::On => "on", Kind::Off => "off", Kind::Refresh => "refresh", Kind::Screen(_) => "panel" }, "automatic":is_auto}));
            let action_log = journal.clone();
            let fleet_empty = fleet.0.is_empty();
            let wake_available = fleet.wake_available();
            let active_fingerprint = fleet.fingerprint()?;
            let mut followers = Vec::new();
            let completed = {
                let operation = fleet.perform(&current, &action_log, &view);
                tokio::pin!(operation);
                loop {
                    tokio::select! {
                        biased;
                        _ = stop.changed() => break None,
                        _ = shutdown.changed() => {
                            let phase = *shutdown.borrow_and_update();
                            let resume = query_lease.observe(phase);
                            let _ = journal.record("shutdown_intent", json!({"source":"session_ipc", "phase":phase}));
                            if settings.keep_tv_on && is_auto {
                                work = (resume && settings.automatic)
                                    .then(|| state.desired_target().map(|target| Work::target(target, false))).flatten();
                                break None;
                            }
                        }
                        result = &mut server_task => { result??; bail!("Service IPC stopped unexpectedly"); }
                        notice = events.recv() => {
                            let notice = notice.context("Service events closed")?;
                            screen_session.apply(notice.event);
                            let panel_cancel = matches!(current.kind, Kind::Screen(_)) && !screen_session.allowed();
                            let _ = journal.record("windows_event", json!({"event":notice.event}));
                            let (next, ack) = event_work(notice, &mut state, settings.automatic && !fleet_empty, &view, *shutdown.borrow() != ShutdownPhase::Cancel);
                            if let Some(next) = next { work = Some(next); if let Some(old) = suspend_ack.take() { let _ = old.try_send(()); } suspend_ack = ack; break None; }
                            if let Some(ack) = ack { let _ = ack.try_send(()); }
                            if panel_cancel { work = None; break None; }
                        }
                        envelope = commands.recv() => {
                            let envelope = envelope.context("Service commands closed")?;
                            if let Request::ScreenProtection { sequence, source, .. } = envelope.request {
                                match screen_sequence.accept(source, sequence) {
                                    None => { let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: None }); continue; }
                                    Some(true) => screen_owner_changed = true,
                                    Some(false) => {}
                                }
                            }
                            if matches!(envelope.request, Request::ScreenProtection { .. }) && !matches!(current.kind, Kind::Screen(_)) {
                                let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: Some("Power control has priority over panel control".into()) });
                                continue;
                            }
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
                            match envelope.request { Request::On => state.set_desired(Target::On), Request::Off | Request::Sleep => state.set_desired(Target::Off), _ => {} }
                            let was_auto = settings.automatic;
                            let was_protected = settings.keep_tv_on;
                            let mut next = command_work(envelope, &mut settings, &view, true);
                            if next.as_ref().is_some_and(|next| current.same_operation(next)) {
                                if let Some(reply) = next.take().and_then(|next| next.reply) { followers.push(reply); }
                                continue;
                            }
                            if !was_auto && settings.automatic && !fleet_empty {
                                let target = current_target(&state);
                                state.set_desired(target);
                                work = Some(Work::target(target, true));
                                break None;
                            }
                            if was_protected != settings.keep_tv_on && current.check_input && matches!(current.kind, Kind::Off) {
                                work = Some(Work::target(Target::Off, current.urgent));
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
                if error.is_some() && !matches!(current.kind, Kind::Screen(_)) {
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
                            check_input: false,
                            urgent: false,
                            delay: Duration::from_secs(u64::from(repair_attempts)),
                            shutdown: false,
                            reply: None,
                        });
                    }
                } else if matches!(current.kind, Kind::On | Kind::Refresh) {
                    repair_attempts = 0;
                }
                reply(&mut current, followers, &view, error);
            } else {
                if !matches!(current.kind, Kind::Screen(_)) {
                    edit(&view, |s| {
                        s.power = if off_attempted {
                            "off_requested"
                        } else {
                            "unknown"
                        }
                        .into()
                    });
                }
                reply(
                    &mut current,
                    followers,
                    &view,
                    Some("Request superseded by a newer power event".into()),
                );
                // Preserve sockets for work cancelled before its I/O started.
                fleet.cancel_sessions();
            }
            continue;
        }
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            _ = shutdown.changed() => {
                let phase = *shutdown.borrow_and_update();
                let resume = query_lease.observe(phase);
                let _ = journal.record("shutdown_intent", json!({"source":"session_ipc", "phase":phase}));
                if settings.keep_tv_on && settings.automatic && !fleet.0.is_empty() && resume {
                    work = state.desired_target().map(|target| Work::target(target, false));
                }
            }
            _ = tokio::time::sleep_until(query_lease.deadline), if *shutdown.borrow() == ShutdownPhase::Query => {
                // Do not replay an old OFF on expiry. Subsequent ordinary
                // events work normally, without guessing that shutdown won.
                if query_lease.expire(&shutdown_tx) {
                    let _ = journal.record("shutdown_query_expired", json!({"reason":"session_confirmation_unavailable"}));
                }
            }
            result = &mut server_task => { result??; bail!("Service IPC stopped unexpectedly"); }
            notice = events.recv() => {
                let notice = notice.context("Service events closed")?;
                if screen_session.apply(notice.event) && !screen_session.allowed() {
                    for device in &mut fleet.0 { device.panel.invalidate(); }
                }
                let _ = journal.record("windows_event", json!({"event":notice.event}));
                // A late NIC becoming available re-arms a failed desired ON,
                // while an OFF policy never wakes the TV merely for discovery.
                let network_retry = notice.event == Event::NetworkChanged && settings.automatic && state.desired_target() == Some(Target::On) && snapshot(&view)?.power == "unknown";
                let pending = settings.keep_tv_on && *shutdown.borrow() != ShutdownPhase::Cancel;
                let (next, ack) = event_work(notice, &mut state, settings.automatic && !fleet.0.is_empty(), &view, pending);
                work = if network_retry && !pending { Some(Work::target(Target::On, true)) } else { next };
                if work.is_some() { suspend_ack = ack; } else if let Some(ack) = ack { let _ = ack.try_send(()); }
            }
            envelope = commands.recv() => {
                let envelope = envelope.context("Service commands closed")?;
                if let Request::ScreenProtection { sequence, source, .. } = envelope.request {
                    match screen_sequence.accept(source, sequence) {
                        None => { let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: None }); continue; }
                        Some(true) => { for device in &mut fleet.0 { device.panel = Default::default(); } }
                        Some(false) => {}
                    }
                }
                if matches!(envelope.request, Request::ScreenProtection { signal, .. } if signal != crate::screen_protection::Signal::Disabled) && (!screen_session.allowed() || state.is_ending() || state.desired_target() == Some(Target::Off) || crate::lifecycle::is_ending()) {
                    let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: None });
                    continue;
                }
                if matches!(envelope.request, Request::Configure { .. }) {
                    configuration = Some(envelope); continue;
                }
                if matches!(envelope.request, Request::Automatic { enabled: true }) && !fleet.wake_available() {
                    let _ = envelope.reply.send(Response { snapshot: snapshot(&view)?, error: Some("기기 정보에서 MAC 주소를 확인해주세요.".into()) });
                    continue;
                }
                match envelope.request { Request::On => state.set_desired(Target::On), Request::Off | Request::Sleep => state.set_desired(Target::Off), _ => {} }
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
                    edit(&view, |s| s.power = "unknown".into());
                    let _ = journal.record("idle_connection_closed", json!({"error":error.to_string()}));
                    if settings.automatic && state.desired_target() == Some(Target::On) {
                        repair_attempts = 1;
                        work = Some(Work { kind: Kind::Refresh, check_input: false, urgent: false, delay: Duration::from_millis(250), shutdown: false, reply: None });
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
    finish_shutdown(&mut fleet, &settings, reason, &journal, &view, || {
        status(SERVICE_STOP_PENDING, 0)
    })
    .await;
    let _ = journal.record("service_stopped", json!({"reason":reason}));
    Ok(())
}

async fn finish_shutdown(
    fleet: &mut Fleet,
    settings: &Settings,
    reason: u32,
    journal: &Journal,
    view: &RwLock<Snapshot>,
    mut checkpoint: impl FnMut(),
) {
    let source = match reason {
        SERVICE_CONTROL_PRESHUTDOWN => "scm_preshutdown",
        SERVICE_CONTROL_SHUTDOWN => "scm_shutdown",
        SERVICE_CONTROL_STOP => "scm_stop",
        _ => "unknown",
    };
    let mut log = journal.clone();
    let _ = log.record(
        "service_stop_received",
        json!({"source":source,"received_at_ms":STOP_RECEIVED_AT.load(Ordering::Acquire)}),
    );
    if !matches!(
        reason,
        SERVICE_CONTROL_PRESHUTDOWN | SERVICE_CONTROL_SHUTDOWN
    ) || !settings.automatic
        || fleet.0.is_empty()
        || fleet.all_off_attempted()
    {
        let _ = log.record("service_shutdown_off_skipped", json!({"ordinary_stop":reason == SERVICE_CONTROL_STOP,"automatic":settings.automatic,"previous_off_may_have_been_sent":fleet.all_off_attempted()}));
        return;
    }
    let protected = settings.keep_tv_on;
    let elapsed = STOP_STARTED.get().map_or(Duration::ZERO, Instant::elapsed);
    let budget = if protected {
        PROTECTED_SHUTDOWN_BUDGET.saturating_sub(elapsed)
    } else {
        Duration::from_millis(3500)
    };
    let operation = async {
        if protected {
            let _ = log.record("shutdown_grace_started", json!({"source":source,"grace_ms":SHUTDOWN_GRACE.as_millis(),"budget_ms":budget.as_millis()}));
            sleep(SHUTDOWN_GRACE.saturating_sub(elapsed)).await;
            let _ = log.record("shutdown_grace_finished", json!({"source":source}));
        }
        let mut work = Work::target(Target::Off, protected);
        work.shutdown = true;
        fleet.perform(&work, journal, view).await
    };
    let result = {
        let operation = timeout(budget, operation);
        tokio::pin!(operation);
        let mut progress = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                biased;
                result = &mut operation => break result,
                _ = progress.tick() => checkpoint(),
            }
        }
    };
    let error = match &result {
        Err(_) => Some("shutdown_budget_expired".to_owned()),
        Ok(Err(error)) => Some(format!("{error:#}")),
        Ok(Ok(())) => None,
    };
    let _ = log.record("service_shutdown_off", json!({"source":source,"success":error.is_none(),"error":error,"off_may_have_been_sent":fleet.0.iter().any(|d| d.off_attempted.load(Ordering::Acquire)),"physical_state":"not_observed"}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    fn shutdown_settings(protected: bool) -> Settings {
        Settings {
            owner_sid: "test".into(),
            automatic: true,
            keep_tv_on: protected,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_lost_cancel_expires_without_replaying_off_or_overriding_confirmation() {
        let (sender, mut receiver) = watch::channel(ShutdownPhase::Cancel);
        let mut lease = QueryLease::default();
        publish_shutdown(&sender, ShutdownPhase::Query);
        assert!(!lease.observe(*receiver.borrow_and_update()));
        let deadline = lease.deadline;
        tokio::time::advance(Duration::from_secs(59)).await;
        assert!(tokio::time::Instant::now() < deadline);
        assert_eq!(*receiver.borrow(), ShutdownPhase::Query);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(tokio::time::Instant::now(), deadline);
        assert!(lease.expire(&sender));
        assert_eq!(*receiver.borrow(), ShutdownPhase::Cancel);
        assert!(!lease.observe(*receiver.borrow_and_update()));
        publish_shutdown(&sender, ShutdownPhase::Query);
        assert!(!lease.observe(*receiver.borrow_and_update()));
        assert!(lease.deadline > deadline);
        publish_shutdown(&sender, ShutdownPhase::Cancel);
        assert!(lease.observe(*receiver.borrow_and_update()));
        publish_shutdown(&sender, ShutdownPhase::Confirm);
        assert!(!lease.expire(&sender));
        assert_eq!(*receiver.borrow(), ShutdownPhase::Confirm);
    }

    #[tokio::test]
    async fn shutdown_queries_latest_input_after_grace_and_sends_at_most_one_off() {
        for app in [
            "netflix",
            "youtube.leanback.v4",
            "com.webos.app.hdmi1",
            "com.webos.app.hdmi2",
        ] {
            let (session, mut peer) = crate::ssap::tests::peer().await;
            let queries = Arc::new(AtomicUsize::new(0));
            let offs = Arc::new(AtomicUsize::new(0));
            let foreground = Arc::new(RwLock::new("com.webos.app.hdmi2".to_owned()));
            let (query_count, off_count, current_app) =
                (queries.clone(), offs.clone(), foreground.clone());
            let remote = tokio::spawn(async move {
                while let Some(Ok(frame)) = peer.next().await {
                    if !frame.is_text() {
                        continue;
                    }
                    let frame: serde_json::Value =
                        serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    let payload = if frame["uri"] == crate::ssap::FOREGROUND_APP {
                        query_count.fetch_add(1, Ordering::AcqRel);
                        json!({"returnValue":true,"appId":current_app.read().unwrap().clone()})
                    } else {
                        assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
                        off_count.fetch_add(1, Ordering::AcqRel);
                        json!({"returnValue":true})
                    };
                    peer.send(Message::Text(
                        json!({"id":frame["id"],"type":"response","payload":payload})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
            });
            let mut config = config(1);
            config.set_manual_hdmi(2);
            let mut fleet = Fleet::new(vec![config]).unwrap();
            fleet.0[0].session = Some(session);
            let view = view();
            edit(&view, |s| s.keep_tv_on = true);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("events.jsonl");
            let journal = Journal::open(&path).unwrap();
            tokio::time::pause();
            let started = tokio::time::Instant::now();
            let task = tokio::spawn(async move {
                finish_shutdown(
                    &mut fleet,
                    &shutdown_settings(true),
                    SERVICE_CONTROL_PRESHUTDOWN,
                    &journal,
                    &view,
                    || {},
                )
                .await;
                fleet
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(2)).await;
            assert_eq!(queries.load(Ordering::Acquire), 0);
            assert_eq!(offs.load(Ordering::Acquire), 0);
            *foreground.write().unwrap() = app.into();
            tokio::time::advance(Duration::from_millis(1999)).await;
            assert_eq!(queries.load(Ordering::Acquire), 0);
            assert!(!task.is_finished());
            tokio::time::advance(Duration::from_millis(1)).await;
            tokio::time::resume(); // Real loopback readiness must not advance virtual deadlines.
            let mut fleet = timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
            assert!(started.elapsed() >= SHUTDOWN_GRACE);
            fleet.clear_sessions();
            remote.await.unwrap();
            assert_eq!(queries.load(Ordering::Acquire), 1);
            assert_eq!(
                offs.load(Ordering::Acquire),
                usize::from(app == "com.webos.app.hdmi2")
            );
            let log = std::fs::read_to_string(path).unwrap();
            assert_eq!(log.matches("shutdown_grace_started").count(), 1);
            assert_eq!(log.matches("shutdown_foreground_query").count(), 1);
            assert!(log.contains(if app == "com.webos.app.hdmi2" {
                "off_evidence"
            } else {
                "off_skipped_input"
            }));
        }
    }

    #[tokio::test]
    async fn shutdown_off_policy_is_immediate_and_ordinary_stop_never_queries() {
        for reason in [
            SERVICE_CONTROL_PRESHUTDOWN,
            SERVICE_CONTROL_SHUTDOWN,
            SERVICE_CONTROL_STOP,
        ] {
            let (session, mut peer) = crate::ssap::tests::peer().await;
            let remote = tokio::spawn(async move {
                let mut offs = 0;
                while let Some(Ok(frame)) = peer.next().await {
                    if !frame.is_text() {
                        continue;
                    }
                    let frame: serde_json::Value =
                        serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
                    offs += 1;
                    peer.send(Message::Text(
                        json!({"id":frame["id"],"type":"response","payload":{"returnValue":true}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
                offs
            });
            let mut fleet = Fleet::new(vec![config(1)]).unwrap();
            fleet.0[0].session = Some(session);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("events.jsonl");
            let journal = Journal::open(&path).unwrap();
            // STOP with protection ON is an update/restart, never a PC shutdown.
            let protected = reason == SERVICE_CONTROL_STOP;
            let view = view();
            edit(&view, |s| s.keep_tv_on = protected);
            timeout(
                Duration::from_millis(500),
                finish_shutdown(
                    &mut fleet,
                    &shutdown_settings(protected),
                    reason,
                    &journal,
                    &view,
                    || {},
                ),
            )
            .await
            .unwrap();
            fleet.clear_sessions();
            assert_eq!(
                remote.await.unwrap(),
                usize::from(reason != SERVICE_CONTROL_STOP)
            );
            assert!(
                !std::fs::read_to_string(path)
                    .unwrap()
                    .contains("shutdown_grace_started")
            );
        }
    }

    #[test]
    fn shutdown_query_cancels_cleanly_and_does_not_change_unprotected_or_sleep_timing() {
        let (sender, mut receiver) = watch::channel(ShutdownPhase::Cancel);
        publish_shutdown(&sender, ShutdownPhase::Query);
        assert_eq!(*receiver.borrow_and_update(), ShutdownPhase::Query);
        publish_shutdown(&sender, ShutdownPhase::Query);
        assert!(!receiver.has_changed().unwrap());
        let view = view();
        edit(&view, |s| s.keep_tv_on = true);
        let mut state = State::default();
        state.apply(Event::DisplayOn);
        for event in [Event::DisplayOff, Event::Suspend, Event::SessionEnding] {
            let (work, _) = event_work(
                Notice {
                    preceding: None,
                    event,
                    suspend_ack: None,
                },
                &mut state,
                true,
                &view,
                true,
            );
            assert!(work.is_none());
        }
        publish_shutdown(&sender, ShutdownPhase::Cancel);
        assert_eq!(*receiver.borrow_and_update(), ShutdownPhase::Cancel);
        let mut resumed = State::default();
        resumed.apply(Event::DisplayOn);
        let (deferred, _) = event_work(
            Notice {
                preceding: None,
                event: Event::DisplayOff,
                suspend_ack: None,
            },
            &mut resumed,
            true,
            &view,
            true,
        );
        assert!(deferred.is_none());
        let (on, _) = event_work(
            Notice {
                preceding: None,
                event: Event::DisplayOn,
                suspend_ack: None,
            },
            &mut resumed,
            true,
            &view,
            false,
        );
        assert!(matches!(on.unwrap().kind, Kind::On));
        let (off, _) = event_work(
            Notice {
                preceding: None,
                event: Event::DisplayOff,
                suspend_ack: None,
            },
            &mut resumed,
            true,
            &view,
            false,
        );
        assert!(matches!(off.unwrap().kind, Kind::Off));
        // A cancelled QUERY never publishes SessionEnding into the power state.
        let mut state = State::default();
        for protected in [false, true] {
            edit(&view, |s| s.keep_tv_on = protected);
            if !protected {
                let (work, _) = event_work(
                    Notice {
                        preceding: None,
                        event: Event::DisplayOff,
                        suspend_ack: None,
                    },
                    &mut State::default(),
                    true,
                    &view,
                    true,
                );
                assert_eq!(work.unwrap().delay, Duration::ZERO);
            }
            for event in [Event::Suspend, Event::SessionEnding] {
                let (work, _) = event_work(
                    Notice {
                        preceding: None,
                        event,
                        suspend_ack: None,
                    },
                    &mut State::default(),
                    true,
                    &view,
                    false,
                );
                let work = work.unwrap();
                assert!(work.urgent);
                assert_eq!(work.delay, Duration::ZERO);
            }
            let (work, _) = event_work(
                Notice {
                    preceding: None,
                    event: Event::DisplayOff,
                    suspend_ack: None,
                },
                &mut state,
                true,
                &view,
                false,
            );
            if let Some(work) = work {
                assert_eq!(work.delay, Duration::ZERO);
            }
        }
        publish_shutdown(&sender, ShutdownPhase::Confirm);
        publish_shutdown(&sender, ShutdownPhase::Cancel);
        assert_eq!(*receiver.borrow(), ShutdownPhase::Confirm);
    }

    #[tokio::test]
    async fn shutdown_slow_or_disconnected_tv_does_not_block_another_tv() {
        for disconnect in [false, true] {
            let (healthy, mut healthy_peer) = crate::ssap::tests::peer().await;
            let (stalled, mut stalled_peer) = crate::ssap::tests::peer().await;
            let healthy_done = Arc::new(AtomicBool::new(false));
            let completed = healthy_done.clone();
            let healthy_remote = tokio::spawn(async move {
                for uri in [crate::ssap::FOREGROUND_APP, crate::ssap::POWER_OFF] {
                    let frame = healthy_peer.next().await.unwrap().unwrap();
                    let frame: serde_json::Value =
                        serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    assert_eq!(frame["uri"], uri);
                    healthy_peer.send(Message::Text(json!({"id":frame["id"],"type":"response","payload":{"returnValue":true,"appId":"com.webos.app.hdmi2"}}).to_string().into())).await.unwrap();
                }
                completed.store(true, Ordering::Release);
            });
            let stalled_remote = tokio::spawn(async move {
                let mut queries = 0;
                while let Some(Ok(frame)) = stalled_peer.next().await {
                    if !frame.is_text() {
                        continue;
                    }
                    let frame: serde_json::Value =
                        serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    assert_eq!(frame["uri"], crate::ssap::FOREGROUND_APP);
                    queries += 1;
                    if disconnect {
                        break;
                    }
                }
                queries
            });
            let mut fleet = Fleet::new(vec![config(1), config(2)]).unwrap();
            for device in &mut fleet.0 {
                device.config.set_manual_hdmi(2);
            }
            fleet.0[0].session = Some(healthy);
            fleet.0[1].session = Some(stalled);
            let view = view();
            edit(&view, |s| s.keep_tv_on = true);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("events.jsonl");
            let journal = Journal::open(&path).unwrap();
            let checkpoints = Arc::new(AtomicUsize::new(0));
            let progress = checkpoints.clone();
            tokio::time::pause();
            let task = tokio::spawn(async move {
                finish_shutdown(
                    &mut fleet,
                    &shutdown_settings(true),
                    SERVICE_CONTROL_SHUTDOWN,
                    &journal,
                    &view,
                    || {
                        progress.fetch_add(1, Ordering::AcqRel);
                    },
                )
                .await;
                fleet
            });
            tokio::task::yield_now().await;
            for _ in 0..4 {
                tokio::time::advance(Duration::from_secs(1)).await;
            }
            tokio::time::resume();
            timeout(Duration::from_millis(300), healthy_remote)
                .await
                .unwrap()
                .unwrap();
            assert!(healthy_done.load(Ordering::Acquire));
            if !disconnect {
                assert!(!task.is_finished());
            }
            let fleet = timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert!(fleet.0[0].off_attempted.load(Ordering::Acquire));
            assert!(!fleet.0[1].off_attempted.load(Ordering::Acquire));
            assert_eq!(
                stalled_remote.await.unwrap(),
                if disconnect { 1 } else { 2 }
            );
            assert!(checkpoints.load(Ordering::Acquire) >= 4);
            let log = std::fs::read_to_string(path).unwrap();
            assert!(log.contains("off_skipped_input"));
            assert_eq!(log.matches("off_evidence").count(), 1);
        }
    }

    #[tokio::test]
    async fn shutdown_missing_ack_keeps_transmission_evidence_without_resending() {
        let (session, mut peer) = crate::ssap::tests::peer().await;
        let remote = tokio::spawn(async move {
            let frame = peer.next().await.unwrap().unwrap();
            let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(frame["uri"], crate::ssap::FOREGROUND_APP);
            peer.send(Message::Text(json!({"id":frame["id"],"type":"response","payload":{"returnValue":true,"appId":"com.webos.app.hdmi2"}}).to_string().into())).await.unwrap();
            let mut offs = 0;
            while let Some(Ok(frame)) = peer.next().await {
                if !frame.is_text() {
                    continue;
                }
                let frame: serde_json::Value =
                    serde_json::from_str(frame.to_text().unwrap()).unwrap();
                assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
                offs += 1;
            }
            offs
        });
        let mut config = config(1);
        config.set_manual_hdmi(2);
        let mut fleet = Fleet::new(vec![config]).unwrap();
        fleet.0[0].session = Some(session);
        let view = view();
        edit(&view, |s| s.keep_tv_on = true);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let journal = Journal::open(&path).unwrap();
        tokio::time::pause();
        let task = tokio::spawn(async move {
            finish_shutdown(
                &mut fleet,
                &shutdown_settings(true),
                SERVICE_CONTROL_PRESHUTDOWN,
                &journal,
                &view,
                || {},
            )
            .await;
            finish_shutdown(
                &mut fleet,
                &shutdown_settings(true),
                SERVICE_CONTROL_SHUTDOWN,
                &journal,
                &view,
                || {},
            )
            .await;
            fleet
        });
        tokio::task::yield_now().await;
        tokio::time::advance(SHUTDOWN_GRACE).await;
        tokio::time::resume();
        let fleet = timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(fleet.all_off_attempted());
        assert_eq!(remote.await.unwrap(), 1);
        let log = std::fs::read_to_string(path).unwrap();
        assert_eq!(log.matches("shutdown_grace_started").count(), 1);
        assert!(log.contains("service_shutdown_off_skipped"));
        assert!(log.contains("OFF budget expired"));
        assert!(log.contains("\"success\":false"));
        assert!(!log.contains("off_evidence"));
    }

    #[tokio::test]
    async fn completed_tv_connection_survives_another_tvs_cancelled_query() {
        let (healthy, mut peer) = crate::ssap::tests::peer().await;
        let (stalled, _stalled_peer) = crate::ssap::tests::peer().await;
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
            peer
        });
        let mut fleet = Fleet::new(vec![config(1), config(2)]).unwrap();
        fleet.0[0].session = Some(healthy);
        fleet.0[1].session = Some(stalled);
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("events.jsonl")).unwrap();
        let refresh = Work {
            kind: Kind::Refresh,
            check_input: false,
            urgent: false,
            delay: Duration::ZERO,
            shutdown: false,
            reply: None,
        };
        assert!(
            timeout(
                Duration::from_millis(100),
                fleet.perform(&refresh, &journal, &view())
            )
            .await
            .is_err()
        );
        let _peer = remote.await.unwrap();
        fleet.cancel_sessions();
        assert!(fleet.0[0].session.is_some());
        assert!(fleet.0[1].session.is_none());
    }

    #[tokio::test]
    async fn duplicate_commands_share_results_but_different_off_policies_do_not_merge() {
        let mut current = Work::target(Target::On, false);
        let mut duplicate = Work::target(Target::On, false);
        duplicate.check_input = false;
        assert!(current.same_operation(&duplicate));
        assert_eq!(Work::target(Target::Off, false).delay, Duration::ZERO);
        current.kind = Kind::Off;
        duplicate.kind = Kind::Off;
        assert!(!current.same_operation(&duplicate));
        let (first, first_result) = oneshot::channel();
        let (second, second_result) = oneshot::channel();
        current.reply = Some(first);
        reply(
            &mut current,
            vec![second],
            &view(),
            Some("cancelled".into()),
        );
        assert_eq!(
            first_result.await.unwrap().error.as_deref(),
            Some("cancelled")
        );
        assert_eq!(
            second_result.await.unwrap().error.as_deref(),
            Some("cancelled")
        );
    }

    #[test]
    fn runtime_address_recovery_does_not_restart_an_identical_configuration() {
        let configured = config(1);
        let expected = fingerprint(std::slice::from_ref(&configured)).unwrap();
        let mut fleet = Fleet::new(vec![configured.clone()]).unwrap();
        fleet.0[0].config.ip = "127.0.0.9".parse().unwrap();
        assert_eq!(fleet.fingerprint().unwrap(), expected);
        let mut changed = configured;
        changed.set_manual_hdmi(4);
        assert_ne!(fingerprint(&[changed]).unwrap(), expected);
    }

    #[tokio::test]
    async fn hdmi_protection_disabled_preserves_existing_off_policy() {
        let (session, mut peer) = crate::ssap::tests::peer().await;
        let remote = tokio::spawn(async move {
            let frame = peer.next().await.unwrap().unwrap();
            let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            // No identity or foreground request is required with protection OFF.
            assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
            peer.send(Message::Text(
                json!({"id":frame["id"],"type":"response","payload":{"returnValue":true}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        });
        let mut fleet = Fleet::new(vec![config(1)]).unwrap();
        fleet.0[0].session = Some(session);
        let directory = tempfile::tempdir().unwrap();
        let journal = Journal::open(&directory.path().join("test.jsonl")).unwrap();
        fleet
            .perform(&Work::target(Target::Off, true), &journal, &view())
            .await
            .unwrap();
        timeout(Duration::from_secs(1), remote)
            .await
            .unwrap()
            .unwrap();
        assert!(fleet.all_off_attempted());
    }

    fn config(last: u8) -> Config {
        let mut config = Config {
            ip: std::net::Ipv4Addr::new(127, 0, 0, last),
            mac: Some("02:00:00:00:00:01".parse().unwrap()),
            broadcast: "127.0.0.1".parse().unwrap(),
            transport: crate::config::Transport::Ws,
            device: None,
            source_path: None,
            wake_verified: false,
            pc_hdmi: None,
            pc_hdmi_source: Default::default(),
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
            keep_tv_on: false,
            busy: false,
            checked_at: None,
            error: None,
        })
    }

    #[test]
    fn legacy_service_policy_defaults_to_input_protection_off() {
        let settings: Settings =
            serde_json::from_str(r#"{"owner_sid":"test","automatic":true}"#).unwrap();
        assert!(!settings.keep_tv_on);
    }

    #[tokio::test]
    async fn hdmi_protection_compares_saved_port_and_retries_only_errors() {
        for source in [
            crate::hdmi::InputSource::Automatic,
            crate::hdmi::InputSource::Manual,
            crate::hdmi::InputSource::Legacy,
        ] {
            for (payloads, should_off) in [
                (
                    vec![json!({"appId":"com.webos.app.hdmi2","returnValue":true})],
                    true,
                ),
                (
                    vec![json!({"appId":"com.webos.app.hdmi1","returnValue":true})],
                    false,
                ),
                (vec![json!({"appId":"netflix","returnValue":true})], false),
                (
                    vec![json!({"appId":"youtube.leanback.v4","returnValue":true})],
                    false,
                ),
                (
                    vec![json!({"appId":"com.webos.app.hdmi20","returnValue":true})],
                    false,
                ),
                (
                    vec![
                        json!({"returnValue":false}),
                        json!({"appId":"com.webos.app.hdmi2","returnValue":true}),
                    ],
                    true,
                ),
                (
                    vec![json!({"returnValue":true}), json!({"returnValue":true})],
                    false,
                ),
            ] {
                let (session, mut peer) = crate::ssap::tests::peer().await;
                let remote = tokio::spawn(async move {
                    for payload in payloads {
                        let frame = peer.next().await.unwrap().unwrap();
                        let frame: serde_json::Value =
                            serde_json::from_str(frame.to_text().unwrap()).unwrap();
                        assert_eq!(frame["uri"], crate::ssap::FOREGROUND_APP);
                        peer.send(Message::Text(
                            json!({"id":frame["id"],"type":"response","payload":payload})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                    }
                    if should_off {
                        let frame = peer.next().await.unwrap().unwrap();
                        let frame: serde_json::Value =
                            serde_json::from_str(frame.to_text().unwrap()).unwrap();
                        assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
                        peer.send(Message::Text(json!({"id":frame["id"],"type":"response","payload":{"returnValue":true}}).to_string().into())).await.unwrap();
                    }
                    while let Some(Ok(frame)) = peer.next().await {
                        assert!(!frame.is_text(), "Unexpected extra query or OFF");
                    }
                });
                let mut config = config(1);
                config.pc_hdmi = Some(2);
                config.pc_hdmi_source = source;
                let mut fleet = Fleet::new(vec![config]).unwrap();
                fleet.0[0].session = Some(session);
                let view = view();
                edit(&view, |s| s.keep_tv_on = true);
                let dir = tempfile::tempdir().unwrap();
                let log = dir.path().join("test.jsonl");
                let journal = Journal::open(&log).unwrap();
                fleet
                    .perform(&Work::target(Target::Off, true), &journal, &view)
                    .await
                    .unwrap();
                assert_eq!(fleet.all_off_attempted(), should_off);
                fleet.clear_sessions();
                timeout(Duration::from_secs(1), remote)
                    .await
                    .unwrap()
                    .unwrap();
                let events = std::fs::read_to_string(log).unwrap();
                assert!(events.contains(if should_off {
                    "off_input_verified"
                } else {
                    "off_skipped_input"
                }));
            }
        }
    }

    #[tokio::test]
    async fn hdmi_timeout_retries_once_and_missing_port_skips_off() {
        for pc_hdmi in [Some(2), None] {
            let (session, mut peer) = crate::ssap::tests::peer().await;
            let remote = tokio::spawn(async move {
                let mut queries = 0;
                while let Some(Ok(frame)) = peer.next().await {
                    if !frame.is_text() {
                        continue;
                    }
                    let frame: serde_json::Value =
                        serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    assert_eq!(frame["uri"], crate::ssap::FOREGROUND_APP);
                    queries += 1;
                }
                assert_eq!(queries, if pc_hdmi.is_some() { 2 } else { 0 });
            });
            let mut config = config(1);
            config.pc_hdmi = pc_hdmi;
            config.pc_hdmi_source = crate::hdmi::InputSource::Automatic;
            let mut fleet = Fleet::new(vec![config]).unwrap();
            fleet.0[0].session = Some(session);
            let view = view();
            edit(&view, |s| s.keep_tv_on = true);
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("test.jsonl");
            let journal = Journal::open(&log).unwrap();
            fleet
                .perform(&Work::target(Target::Off, true), &journal, &view)
                .await
                .unwrap();
            assert!(!fleet.all_off_attempted());
            fleet.clear_sessions();
            timeout(Duration::from_secs(1), remote)
                .await
                .unwrap()
                .unwrap();
            let events = std::fs::read_to_string(log).unwrap();
            assert!(events.contains(if pc_hdmi.is_some() {
                "input_timeout"
            } else {
                "pc_hdmi_not_set"
            }));
        }
    }

    #[tokio::test]
    async fn each_tv_uses_its_own_pc_hdmi_input() {
        let mut fleet = Fleet::new(vec![config(1), config(2)]).unwrap();
        let mut remotes = Vec::new();
        for (index, device) in fleet.0.iter_mut().enumerate() {
            let (session, mut peer) = crate::ssap::tests::peer().await;
            device.session = Some(session);
            device.config.set_manual_hdmi(index as u8 + 1);
            remotes.push(tokio::spawn(async move {
                let frame = peer.next().await.unwrap().unwrap();
                let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                assert_eq!(frame["uri"], crate::ssap::FOREGROUND_APP);
                peer.send(Message::Text(json!({"id":frame["id"],"type":"response","payload":{"appId":"com.webos.app.hdmi1","returnValue":true}}).to_string().into())).await.unwrap();
                if index == 0 {
                    let frame = peer.next().await.unwrap().unwrap();
                    let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    assert_eq!(frame["uri"], crate::ssap::POWER_OFF);
                    peer.send(Message::Text(json!({"id":frame["id"],"type":"response","payload":{"returnValue":true}}).to_string().into())).await.unwrap();
                } else {
                    while let Some(Ok(frame)) = peer.next().await { assert!(!frame.is_text()); }
                }
            }));
        }
        let view = view();
        edit(&view, |s| s.keep_tv_on = true);
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("test.jsonl")).unwrap();
        fleet
            .perform(&Work::target(Target::Off, true), &journal, &view)
            .await
            .unwrap();
        for remote in remotes {
            remote.await.unwrap();
        }
        assert!(fleet.0[0].off_attempted.load(Ordering::Acquire));
        assert!(!fleet.0[1].off_attempted.load(Ordering::Acquire));
        assert_eq!(snapshot(&view).unwrap().power, "unknown");
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
    async fn cancelled_pending_work_keeps_connection_and_active_on_sends_no_wol() {
        let (session, mut peer) = crate::ssap::tests::peer().await;
        let mut fleet = Fleet::new(vec![config(1)]).unwrap();
        fleet.0[0].session = Some(session);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let journal = Journal::open(&path).unwrap();
        let view = view();
        let mut pending = Work::target(Target::Off, false);
        pending.delay = Duration::from_secs(1);
        assert!(
            timeout(
                Duration::from_millis(20),
                fleet.perform(&pending, &journal, &view)
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
        let off = Work::target(Target::Off, false);
        timeout(
            Duration::from_millis(300),
            fleet.perform(&off, &journal, &view),
        )
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
        assert!(fleet.0[0].session.is_some());
        assert!(fleet.0[1].session.is_none());
    }

    #[test]
    fn disabled_policy_and_maintenance_resume_do_not_schedule_wake() {
        let view = view();
        let mut state = State::default();
        let (work, _) = event_work(
            Notice {
                preceding: None,
                event: Event::DisplayOn,
                suspend_ack: None,
            },
            &mut state,
            false,
            &view,
            false,
        );
        assert!(work.is_none());
        state.apply(Event::Suspend);
        let (work, _) = event_work(
            Notice {
                preceding: None,
                event: Event::ResumeAutomatic,
                suspend_ack: None,
            },
            &mut state,
            true,
            &view,
            false,
        );
        assert!(work.is_none());
        let (work, _) = event_work(
            Notice {
                preceding: None,
                event: Event::ResumeInteractive,
                suspend_ack: None,
            },
            &mut state,
            true,
            &view,
            false,
        );
        assert!(matches!(work.unwrap().kind, Kind::On));
    }

    #[tokio::test]
    async fn panel_failure_is_independent_and_successful_devices_keep_their_owned_session() {
        use crate::screen_protection::{Mode, Signal};
        let mut fleet = Fleet::new(vec![config(1), config(2)]).unwrap();
        let mut remotes = Vec::new();
        for (index, device) in fleet.0.iter_mut().enumerate() {
            device.config.pc_hdmi = Some(2);
            let (session, mut peer) = crate::ssap::tests::peer().await;
            device.session = Some(session);
            remotes.push(tokio::spawn(async move {
                for (uri, payload) in [
                    (crate::ssap::POWER_STATE, json!({"state":"Active"})),
                    (
                        crate::ssap::FOREGROUND_APP,
                        json!({"returnValue":true,"appId":"com.webos.app.hdmi2"}),
                    ),
                    (crate::ssap::SCREEN_OFF, json!({"returnValue":index == 1})),
                ] {
                    let frame = peer.next().await.unwrap().unwrap();
                    let frame: serde_json::Value =
                        serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    assert_eq!(frame["uri"], uri);
                    peer.send(Message::Text(
                        json!({"type":"response","id":frame["id"],"payload":payload})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
                if index == 1 {
                    for (uri, payload) in [
                        (crate::ssap::POWER_STATE, json!({"state":"Screen Off"})),
                        (
                            crate::ssap::FOREGROUND_APP,
                            json!({"returnValue":true,"appId":"com.webos.app.hdmi2"}),
                        ),
                        (crate::ssap::SCREEN_ON, json!({"returnValue":true})),
                    ] {
                        let frame = peer.next().await.unwrap().unwrap();
                        let frame: serde_json::Value =
                            serde_json::from_str(frame.to_text().unwrap()).unwrap();
                        assert_eq!(frame["uri"], uri);
                        peer.send(Message::Text(
                            json!({"type":"response","id":frame["id"],"payload":payload})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                    }
                }
                while let Some(Ok(frame)) = peer.next().await {
                    assert!(!frame.is_text());
                }
            }));
        }
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("panel.jsonl")).unwrap();
        let view = view();
        let mut work = Work::target(Target::On, false);
        work.kind = Kind::Screen(Signal::Idle);
        assert!(fleet.perform(&work, &journal, &view).await.is_err());
        assert_eq!(fleet.0[0].panel.mode, Mode::Uncertain);
        assert_eq!(fleet.0[1].panel.mode, Mode::AutoScreenOff);
        assert!(fleet.0[1].session.is_some());
        fleet.perform(&work, &journal, &view).await.unwrap();
        work.kind = Kind::Screen(Signal::Active);
        fleet.perform(&work, &journal, &view).await.unwrap();
        assert_eq!(fleet.0[1].panel.mode, Mode::Idle);
        fleet.clear_sessions();
        for remote in remotes {
            remote.await.unwrap();
        }
    }
}
