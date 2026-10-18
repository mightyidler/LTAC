//! Event-only Windows power notifications. A hidden top-level window receives
//! WM_POWERBROADCAST; no timer, polling, service or visible window is created.
use crate::sync::{Event, Notice};
use anyhow::{Context, Result, bail};
use std::{
    ptr::{null, null_mut},
    time::Duration,
};
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    NetworkManagement::IpHelper::{
        CancelMibChangeNotify2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, NotifyIpInterfaceChange,
    },
    Networking::WinSock::AF_INET,
    System::{
        LibraryLoader::GetModuleHandleW,
        Power::{
            POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
            RegisterSuspendResumeNotification, UnregisterPowerSettingNotification,
            UnregisterSuspendResumeNotification,
        },
        RemoteDesktop::{
            NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
            WTSUnRegisterSessionNotification,
        },
        SystemServices::GUID_SESSION_DISPLAY_STATUS,
    },
    UI::WindowsAndMessaging::*,
};

pub struct Source {
    hwnd: usize,
    thread: Option<std::thread::JoinHandle<()>>,
}
struct ContextData {
    sender: UnboundedSender<Notice>,
    wait_for_suspend: bool,
}

struct NetworkRegistration(windows_sys::Win32::Foundation::HANDLE);
impl Drop for NetworkRegistration {
    fn drop(&mut self) {
        unsafe {
            CancelMibChangeNotify2(self.0);
        }
    }
}
unsafe extern "system" fn network_changed(
    context: *const std::ffi::c_void,
    _: *const MIB_IPINTERFACE_ROW,
    _: MIB_NOTIFICATION_TYPE,
) {
    // Runs on an OS callback thread. Nonblocking send only; cancellation is
    // performed by the window-owning thread before ContextData is dropped.
    if !context.is_null() {
        let context = unsafe { &*context.cast::<ContextData>() };
        let _ = context.sender.send(Notice {
            event: Event::NetworkChanged,
            suspend_ack: None,
        });
    }
}

impl Source {
    pub async fn start(sender: UnboundedSender<Notice>, wait_for_suspend: bool) -> Result<Self> {
        let (ready_tx, ready_rx) = oneshot::channel();
        let thread = std::thread::spawn(move || {
            let mut ready_tx = Some(ready_tx);
            let result = unsafe {
                message_loop(
                    ContextData {
                        sender,
                        wait_for_suspend,
                    },
                    &mut ready_tx,
                )
            };
            if let Err(error) = result
                && let Some(tx) = ready_tx
            {
                let _ = tx.send(Err(format!("{error:#}")));
            }
        });
        let hwnd = ready_rx
            .await
            .context("Windows event thread exited during startup")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            hwnd,
            thread: Some(thread),
        })
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        // The handle belongs to our thread. WM_CLOSE destroys it there, then
        // WM_DESTROY stops its message pump and releases registrations.
        unsafe {
            PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

unsafe fn message_loop(
    context: ContextData,
    ready: &mut Option<oneshot::Sender<std::result::Result<usize, String>>>,
) -> Result<()> {
    // All pointer-based calls are restricted to this window-owning thread.
    // Context outlives CreateWindow + the entire message pump + DestroyWindow.
    unsafe {
        let class: Vec<u16> = format!("LTAC.PowerEvents.{}\0", std::process::id())
            .encode_utf16()
            .collect();
        let instance = GetModuleHandleW(null());
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&window_class) == 0 {
            return Err(std::io::Error::last_os_error())
                .context("Register power event window class");
        }
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            (&context as *const ContextData).cast(),
        );
        if hwnd.is_null() {
            UnregisterClassW(class.as_ptr(), instance);
            return Err(std::io::Error::last_os_error())
                .context("Create hidden power event window");
        }
        let display = RegisterPowerSettingNotification(
            hwnd,
            &GUID_SESSION_DISPLAY_STATUS,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        let resume = RegisterSuspendResumeNotification(hwnd, DEVICE_NOTIFY_WINDOW_HANDLE);
        if display == 0 || resume == 0 {
            let error = std::io::Error::last_os_error();
            if display != 0 {
                UnregisterPowerSettingNotification(display);
            }
            if resume != 0 {
                UnregisterSuspendResumeNotification(resume);
            }
            DestroyWindow(hwnd);
            UnregisterClassW(class.as_ptr(), instance);
            return Err(error).context("Register Windows power notifications");
        }
        if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) == 0 {
            let error = std::io::Error::last_os_error();
            UnregisterPowerSettingNotification(display);
            UnregisterSuspendResumeNotification(resume);
            DestroyWindow(hwnd);
            UnregisterClassW(class.as_ptr(), instance);
            return Err(error).context("Register Windows session change notifications");
        }
        let mut network = null_mut();
        let code = NotifyIpInterfaceChange(
            AF_INET,
            Some(network_changed),
            (&context as *const ContextData).cast(),
            false,
            &mut network,
        );
        if code != 0 {
            WTSUnRegisterSessionNotification(hwnd);
            UnregisterPowerSettingNotification(display);
            UnregisterSuspendResumeNotification(resume);
            DestroyWindow(hwnd);
            UnregisterClassW(class.as_ptr(), instance);
            return Err(std::io::Error::from_raw_os_error(code as i32))
                .context("Register network change notifications");
        }
        let _network_registration = NetworkRegistration(network);
        let attached = ready
            .take()
            .is_some_and(|tx| tx.send(Ok(hwnd as usize)).is_ok());
        if !attached {
            WTSUnRegisterSessionNotification(hwnd);
            UnregisterPowerSettingNotification(display);
            UnregisterSuspendResumeNotification(resume);
            DestroyWindow(hwnd);
            UnregisterClassW(class.as_ptr(), instance);
            return Ok(());
        }
        let mut message: MSG = std::mem::zeroed();
        let failed = loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);
            if result <= 0 {
                break result == -1;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        };
        WTSUnRegisterSessionNotification(hwnd);
        UnregisterPowerSettingNotification(display);
        UnregisterSuspendResumeNotification(resume);
        if IsWindow(hwnd) != 0 {
            DestroyWindow(hwnd);
        }
        UnregisterClassW(class.as_ptr(), instance);
        if failed {
            bail!("Windows GetMessageW failed");
        }
        Ok(())
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = &*(lparam as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                return 1;
            }
            WM_QUERYENDSESSION => {
                // A query can be cancelled by another application. Never send
                // OFF here and never veto Windows shutdown.
                return 1;
            }
            WM_ENDSESSION => {
                if wparam != 0 && (lparam as u32 & ENDSESSION_CLOSEAPP) == 0 {
                    crate::lifecycle::begin_ending();
                    let context = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ContextData;
                    if !context.is_null() {
                        let (ack, wait) = if (*context).wait_for_suspend {
                            let (tx, rx) = std::sync::mpsc::sync_channel(1);
                            (Some(tx), Some(rx))
                        } else {
                            (None, None)
                        };
                        if (*context)
                            .sender
                            .send(Notice {
                                event: Event::SessionEnding,
                                suspend_ack: ack,
                            })
                            .is_ok()
                            && let Some(wait) = wait
                        {
                            // Completion must happen before returning: Windows
                            // may terminate the process immediately afterwards.
                            let _ = wait.recv_timeout(Duration::from_millis(1700));
                        }
                    }
                }
                return 0;
            }
            WM_WTSSESSION_CHANGE => {
                let context = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ContextData;
                let event = match wparam as u32 {
                    WTS_SESSION_LOCK => Some(Event::SessionLocked),
                    WTS_SESSION_UNLOCK => Some(Event::SessionUnlocked),
                    _ => None,
                };
                if !context.is_null()
                    && let Some(event) = event
                {
                    let _ = (*context).sender.send(Notice {
                        event,
                        suspend_ack: None,
                    });
                }
                return 0;
            }
            WM_POWERBROADCAST => {
                let context = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ContextData;
                if !context.is_null() {
                    let event = match wparam as u32 {
                        PBT_APMSUSPEND => Some(Event::Suspend),
                        PBT_APMRESUMEAUTOMATIC => Some(Event::ResumeAutomatic),
                        PBT_APMRESUMESUSPEND => Some(Event::ResumeInteractive),
                        PBT_POWERSETTINGCHANGE if lparam != 0 => {
                            let setting = &*(lparam as *const POWERBROADCAST_SETTING);
                            let guid = setting.PowerSetting;
                            if guid.data1 == GUID_SESSION_DISPLAY_STATUS.data1
                                && guid.data2 == GUID_SESSION_DISPLAY_STATUS.data2
                                && guid.data3 == GUID_SESSION_DISPLAY_STATUS.data3
                                && guid.data4 == GUID_SESSION_DISPLAY_STATUS.data4
                                && setting.DataLength == 4
                            {
                                match setting.Data.as_ptr().cast::<u32>().read_unaligned() {
                                    0 => Some(Event::DisplayOff),
                                    1 => Some(Event::DisplayOn),
                                    2 => Some(Event::DisplayDimmed),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(event) = event {
                        let (ack, wait) = if event == Event::Suspend && (*context).wait_for_suspend
                        {
                            let (tx, rx) = std::sync::mpsc::sync_channel(1);
                            (Some(tx), Some(rx))
                        } else {
                            (None, None)
                        };
                        if (*context)
                            .sender
                            .send(Notice {
                                event,
                                suspend_ack: ack,
                            })
                            .is_ok()
                            && let Some(wait) = wait
                        {
                            // Windows allows about two seconds for suspend work.
                            let _ = wait.recv_timeout(Duration::from_millis(1700));
                        }
                    }
                }
                return 1;
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                return 0;
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return 0;
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_registration_and_shutdown_release_window_and_class() {
        // Actual Win32 APIs, no TV traffic. Reopening proves both registration
        // handles, the hidden HWND and its class have been released.
        for _ in 0..2 {
            let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let source = Source::start(sender, false).await.unwrap();
            let hwnd = source.hwnd;
            assert_ne!(unsafe { IsWindow(hwnd as HWND) }, 0);
            drop(source);
            assert_eq!(unsafe { IsWindow(hwnd as HWND) }, 0);
        }
    }
}
