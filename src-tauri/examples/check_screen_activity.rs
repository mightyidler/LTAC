//! Native, TV-free diagnostic for device activity and process CPU time.
//! --inject sends an unused F24 key release to demonstrate SendInput isolation.
use anyhow::{Result, ensure};
use ltac::screen_protection::{InputSample, Policy, Sampler};
#[path = "../src/screen_input.rs"]
mod screen_input;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::FILETIME,
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemInformation::GetTickCount,
        Threading::{GetCurrentProcess, GetCurrentThreadId, GetProcessTimes},
    },
    UI::{
        Input::KeyboardAndMouse::{
            GetLastInputInfo, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            LASTINPUTINFO, SendInput, VK_F24,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
            LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
            SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_QUIT,
        },
    },
};

// Optional diagnostic counts only; never record keys, coordinates or text.
static COUNTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
unsafe extern "system" fn trace_keyboard(code: i32, wparam: usize, lparam: isize) -> isize {
    if code == HC_ACTION as i32 && lparam != 0 {
        let flags = unsafe { (*(lparam as *const KBDLLHOOKSTRUCT)).flags };
        COUNTS[usize::from(flags & LLKHF_INJECTED != 0)].fetch_add(1, Ordering::Relaxed);
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}
unsafe extern "system" fn trace_mouse(code: i32, wparam: usize, lparam: isize) -> isize {
    if code == HC_ACTION as i32 && lparam != 0 {
        let flags = unsafe { (*(lparam as *const MSLLHOOKSTRUCT)).flags };
        COUNTS[2 + usize::from(flags & LLMHF_INJECTED != 0)].fetch_add(1, Ordering::Relaxed);
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}
struct TraceHooks([HHOOK; 2]);
impl Drop for TraceHooks {
    fn drop(&mut self) {
        for hook in self.0 {
            if !hook.is_null() {
                unsafe {
                    UnhookWindowsHookEx(hook);
                }
            }
        }
    }
}

fn cpu_ms() -> f64 {
    unsafe {
        let mut times = [FILETIME::default(); 4];
        let [created, exited, kernel, user] = &mut times;
        if GetProcessTimes(GetCurrentProcess(), created, exited, kernel, user) == 0 {
            return 0.;
        }
        let ticks = |time: &FILETIME| {
            (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
        };
        (ticks(kernel) + ticks(user)) as f64 / 10_000.
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let seconds = args
        .iter()
        .find_map(|arg| arg.parse::<u64>().ok())
        .unwrap_or(10);
    let tracking = !args.iter().any(|arg| arg == "--baseline");
    let inject = args.iter().any(|arg| arg == "--inject");
    let trace = args.iter().any(|arg| arg == "--trace");
    let activity = screen_input::Activity::new();
    let input = activity.clone();
    let (ready, started) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || -> Result<()> {
        let mut message = MSG::default();
        unsafe {
            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
        let _registration = tracking.then(|| input.register()).transpose()?;
        let _trace = if trace {
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let hooks = TraceHooks(unsafe {
                [
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(trace_keyboard), instance, 0),
                    SetWindowsHookExW(WH_MOUSE_LL, Some(trace_mouse), instance, 0),
                ]
            });
            ensure!(
                hooks.0.iter().all(|hook| !hook.is_null()),
                "Trace hook registration failed"
            );
            Some(hooks)
        } else {
            None
        };
        let _ = ready.send(unsafe { GetCurrentThreadId() });
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {}
        Ok(())
    });
    let thread_id = started.recv()?;
    let start = Instant::now();
    let cpu_start = cpu_ms();
    let mut sampler = Sampler::default();
    while start.elapsed() < Duration::from_secs(seconds) {
        if inject {
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_F24,
                        dwFlags: KEYEVENTF_KEYUP,
                        ..Default::default()
                    },
                },
            };
            ensure!(
                unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } == 1,
                "SendInput was rejected"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        let system_idle = unsafe {
            (GetLastInputInfo(&mut info) != 0).then(|| GetTickCount().wrapping_sub(info.dwTime))
        };
        let device_input = activity.sample();
        let signal = sampler.update(
            Policy {
                enabled: true,
                allowed: true,
                shortcut_enabled: false,
                timeout_seconds: 60,
            },
            device_input.map(|(token, idle_ms)| InputSample {
                token,
                idle_ms,
                allow_auto: true,
            }),
            false,
            start.elapsed().as_millis() as u64,
        );
        println!(
            "{}",
            json!({"elapsed_ms":start.elapsed().as_millis(), "device_input":device_input, "system_idle_ms":system_idle,"signal":signal})
        );
    }
    let elapsed = start.elapsed().as_secs_f64();
    let cpu = cpu_ms() - cpu_start;
    println!(
        "{}",
        json!({"summary":true,"tracking":tracking,"injected":inject,"elapsed_seconds":elapsed,"cpu_ms":cpu,"one_core_percent":cpu/(elapsed*10.),
            "event_counts":trace.then(|| COUNTS.each_ref().map(|count|count.load(Ordering::Relaxed)))})
    );
    unsafe {
        PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
    }
    thread
        .join()
        .map_err(|_| anyhow::anyhow!("Input thread panicked"))??;
    Ok(())
}
