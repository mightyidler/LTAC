#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desktop;
mod updater;

fn main() {
    if let Err(error) = desktop::run() {
        let message: Vec<u16> = format!("LTAC를 시작할 수 없습니다.\n\n{error:#}\0")
            .encode_utf16()
            .collect();
        let title: Vec<u16> = "LTAC\0".encode_utf16().collect();
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                    | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }
}
