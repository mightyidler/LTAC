//! Physical activity only. Runs on the existing screen-hotkey message thread;
//! callbacks retain only a timestamp and always pass input to the next hook.
use anyhow::{Context, Result, ensure};
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, SystemInformation::GetTickCount64},
    UI::WindowsAndMessaging::*,
};

thread_local! {
    static ACTIVITY: RefCell<Option<Arc<Activity>>> = const { RefCell::new(None) };
}

pub struct Activity {
    last: AtomicU64,
    registered: AtomicBool,
}

impl Activity {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            last: AtomicU64::new(unsafe { GetTickCount64() }),
            registered: AtomicBool::new(false),
        })
    }

    pub fn registered(&self) -> bool {
        self.registered.load(Ordering::Acquire)
    }

    pub fn sample(&self) -> Option<(u32, u32)> {
        self.registered().then(|| {
            let last = self.last.load(Ordering::Relaxed);
            (last as u32, elapsed(last, unsafe { GetTickCount64() }))
        })
    }

    /// Must be installed and dropped on the same responsive message thread.
    pub fn register(self: &Arc<Self>) -> Result<Registration> {
        ensure!(
            ACTIVITY.with(|slot| slot.borrow().is_none()),
            "Input already registered on this thread"
        );
        ACTIVITY.with(|slot| *slot.borrow_mut() = Some(self.clone()));
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let mut registration = Registration {
            activity: self.clone(),
            keyboard: std::ptr::null_mut(),
            mouse: std::ptr::null_mut(),
        };
        registration.keyboard =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), instance, 0) };
        if registration.keyboard.is_null() {
            return Err(std::io::Error::last_os_error())
                .context("Register physical keyboard activity");
        }
        registration.mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), instance, 0) };
        if registration.mouse.is_null() {
            return Err(std::io::Error::last_os_error())
                .context("Register physical mouse activity");
        }
        self.last
            .store(unsafe { GetTickCount64() }, Ordering::Relaxed);
        self.registered.store(true, Ordering::Release);
        Ok(registration)
    }
}

fn elapsed(last: u64, now: u64) -> u32 {
    now.saturating_sub(last).min(u64::from(u32::MAX)) as u32
}

fn record(flags: u32, injected: u32) {
    if flags & injected != 0 {
        return;
    }
    ACTIVITY.with(|slot| {
        if let Ok(slot) = slot.try_borrow()
            && let Some(activity) = slot.as_ref()
            && activity.registered()
        {
            activity
                .last
                .store(unsafe { GetTickCount64() }, Ordering::Relaxed);
        }
    });
}

unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        let flags = unsafe { (*(lparam as *const KBDLLHOOKSTRUCT)).flags };
        record(flags, LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED);
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        let flags = unsafe { (*(lparam as *const MSLLHOOKSTRUCT)).flags };
        record(flags, LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED);
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

pub struct Registration {
    activity: Arc<Activity>,
    keyboard: HHOOK,
    mouse: HHOOK,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.activity.registered.store(false, Ordering::Release);
        unsafe {
            if !self.keyboard.is_null() {
                UnhookWindowsHookEx(self.keyboard);
            }
            if !self.mouse.is_null() {
                UnhookWindowsHookEx(self.mouse);
            }
        }
        ACTIVITY.with(|slot| slot.borrow_mut().take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injected_input_is_ignored_and_hardware_advances_the_clock() {
        let activity = Activity::new();
        ACTIVITY.with(|slot| *slot.borrow_mut() = Some(activity.clone()));
        activity.registered.store(true, Ordering::Release);
        for mask in [
            LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED,
            LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED,
        ] {
            activity.last.store(1, Ordering::Relaxed);
            for flag in [mask, mask & mask.wrapping_neg(), mask & (mask - 1)] {
                record(flag, mask);
                assert_eq!(activity.sample().unwrap().0, 1);
            }
            record(0, mask);
            assert_ne!(activity.sample().unwrap().0, 1);
        }
        ACTIVITY.with(|slot| slot.borrow_mut().take());
    }
    #[test]
    fn clock_handles_long_uptime_and_unregistered_input_is_unavailable() {
        assert_eq!(
            elapsed(u64::from(u32::MAX) - 10, u64::from(u32::MAX) + 20),
            30
        );
        assert_eq!(elapsed(100, 99), 0);
        assert_eq!(elapsed(0, u64::MAX), u32::MAX);
        assert!(Activity::new().sample().is_none());
    }
}
