//! Local, bounded, typed IPC. No arbitrary URLs, files, commands or credentials.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::ClientOptions,
};
use windows_sys::Win32::System::Services::*;

pub const NAME: &str = "LTACPower";
pub const PIPE: &str = r"\\.\pipe\LTAC.Power.v1";
pub const MAX_FRAME: u64 = 65536;

/// Installer-only administrative operation; never starts the service or sends
/// a power command. sc.exe does not expose a `preshutdown` command on Windows.
pub fn configure_preshutdown() -> Result<()> {
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        anyhow::ensure!(
            !scm.is_null(),
            "Open service manager: {}",
            std::io::Error::last_os_error()
        );
        let service = OpenServiceW(
            scm,
            wide(NAME).as_ptr(),
            SERVICE_CHANGE_CONFIG | SERVICE_QUERY_CONFIG,
        );
        let error = std::io::Error::last_os_error();
        CloseServiceHandle(scm);
        anyhow::ensure!(
            !service.is_null(),
            "Open LTAC service configuration: {error}"
        );
        let result = (|| {
            let value = SERVICE_PRESHUTDOWN_INFO {
                dwPreshutdownTimeout: 6000,
            };
            if ChangeServiceConfig2W(
                service,
                SERVICE_CONFIG_PRESHUTDOWN_INFO,
                (&value as *const SERVICE_PRESHUTDOWN_INFO).cast(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error()).context("Set preshutdown timeout");
            }
            let mut actual = SERVICE_PRESHUTDOWN_INFO {
                dwPreshutdownTimeout: 0,
            };
            let mut needed = 0;
            if QueryServiceConfig2W(
                service,
                SERVICE_CONFIG_PRESHUTDOWN_INFO,
                (&mut actual as *mut SERVICE_PRESHUTDOWN_INFO).cast(),
                std::mem::size_of_val(&actual) as u32,
                &mut needed,
            ) == 0
            {
                return Err(std::io::Error::last_os_error()).context("Read preshutdown timeout");
            }
            anyhow::ensure!(
                actual.dwPreshutdownTimeout == 6000,
                "Preshutdown timeout was not applied"
            );
            Ok(())
        })();
        CloseServiceHandle(service);
        result
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Subscribe,
    Snapshot,
    Refresh,
    On,
    Off,
    Automatic { enabled: bool },
    Configure { devices: Vec<crate::config::Config> },
}

pub async fn subscribe(mut observe: impl FnMut(Snapshot)) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut pipe = open_verified()?;
    pipe.write_all(b"{\"command\":\"subscribe\"}\n").await?;
    let mut reader = BufReader::new(pipe);
    loop {
        let mut line = String::new();
        (&mut reader).take(MAX_FRAME).read_line(&mut line).await?;
        anyhow::ensure!(line.ends_with('\n'), "Service event stream ended");
        let response: Response = serde_json::from_str(&line)?;
        observe(response.snapshot);
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub power: String,
    pub automatic: bool,
    pub busy: bool,
    pub checked_at: Option<u64>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Response {
    pub snapshot: Snapshot,
    pub error: Option<String>,
}

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn open_verified() -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let pipe = ClientOptions::new()
        .open(PIPE)
        .context("LTAC 전원 서비스에 연결할 수 없습니다. 서비스 실행 상태를 확인해 주세요.")?;
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let service = OpenServiceW(scm, wide(NAME).as_ptr(), SERVICE_QUERY_STATUS);
        let error = std::io::Error::last_os_error();
        CloseServiceHandle(scm);
        if service.is_null() {
            return Err(error.into());
        }
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0;
        let ok = QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        );
        let error = std::io::Error::last_os_error();
        CloseServiceHandle(service);
        if ok == 0 {
            return Err(error.into());
        }
        let mut server_pid = 0;
        if GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut server_pid) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        anyhow::ensure!(
            status.dwCurrentState == SERVICE_RUNNING
                && status.dwProcessId != 0
                && status.dwProcessId == server_pid,
            "Pipe server does not match the running LTAC service"
        );
    }
    Ok(pipe)
}

/// Presence is independent of readiness: an installed but unavailable service
/// must never cause a second controller to take ownership of the TV.
pub fn installed() -> Result<bool> {
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let service = OpenServiceW(scm, wide(NAME).as_ptr(), SERVICE_QUERY_STATUS);
        let error = std::io::Error::last_os_error();
        CloseServiceHandle(scm);
        if service.is_null() {
            if error.raw_os_error() == Some(1060) {
                return Ok(false);
            }
            return Err(error.into());
        }
        CloseServiceHandle(service);
        Ok(true)
    }
}

pub async fn request(request: Request) -> Result<Snapshot> {
    use tokio::io::AsyncReadExt;
    // ClientOptions defaults to SECURITY_IDENTIFICATION, so a pipe server
    // cannot impersonate the desktop user's token.
    let operation = async {
        let mut pipe = open_verified()?;
        let mut frame = serde_json::to_vec(&request)?;
        frame.push(b'\n');
        anyhow::ensure!(
            frame.len() as u64 <= MAX_FRAME,
            "Service request exceeds frame limit"
        );
        pipe.write_all(&frame).await?;
        let mut line = String::new();
        BufReader::new(pipe)
            .take(MAX_FRAME)
            .read_line(&mut line)
            .await?;
        anyhow::ensure!(line.ends_with('\n'), "Incomplete service response");
        let response: Response = serde_json::from_str(&line)?;
        if let Some(error) = response.error {
            bail!("{error}");
        }
        Ok(response.snapshot)
    };
    tokio::time::timeout(Duration::from_secs(95), operation)
        .await
        .context("Service request timed out")?
}
