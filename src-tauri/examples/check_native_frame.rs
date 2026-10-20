//! Read-only DWM verification using the same window config, without service/TV setup.
use std::{ffi::c_void, mem::size_of, time::Duration};

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmGetWindowAttribute(
        hwnd: *mut c_void,
        attribute: u32,
        value: *mut c_void,
        size: u32,
    ) -> i32;
}

fn attribute(window: &tauri::WebviewWindow, id: u32) -> Result<u32, String> {
    let mut value = 0u32;
    let hwnd = window.hwnd().map_err(|e| e.to_string())?;
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd.0,
            id,
            (&mut value as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    if result < 0 {
        return Err(format!("DWM attribute {id}: HRESULT {result:#x}"));
    }
    Ok(value)
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let mut config = app.config().app.windows.first().expect("main config").clone();
            assert!(!config.transparent && config.shadow && !config.decorations);
            assert_eq!((config.width, config.height), (375.0, 500.0));
            config.url = tauri::WebviewUrl::External("about:blank".parse()?);
            let window = tauri::WebviewWindowBuilder::from_config(app, &config)?.build()?;
            // First show resolves Windows' hidden non-client caption metrics.
            // Measure the displayed client; never resize it to make the test pass.
            window.show()?;
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let check = async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let scale = window.scale_factor().map_err(|e| e.to_string())?;
                    let size = window.inner_size().map_err(|e| e.to_string())?;
                    if size.width != (375.0_f64 * scale).round() as u32 || size.height != (500.0_f64 * scale).round() as u32 { return Err(format!("Client size mismatch: {}x{}, scale={scale}", size.width,size.height)); }
                    assert!(!window.is_decorated().map_err(|e| e.to_string())?);
                    window.set_theme(Some(tauri::Theme::Dark)).map_err(|e| e.to_string())?;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let dark = attribute(&window, 20)?;
                    window.set_theme(Some(tauri::Theme::Light)).map_err(|e| e.to_string())?;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let light = attribute(&window, 20)?;
                    if dark != 1 || light != 0 { return Err(format!("Native themes: dark={dark}, light={light}")); }
                    // Windows 11 attributes: default policy permits system corners,
                    // and the system owns the visible non-client border.
                    let corners = attribute(&window, 33)?;
                    let border = attribute(&window, 37)?;
                    if corners == 1 || border == 0 { return Err(format!("Native corners/border disabled: corners={corners},border={border}")); }
                    println!("Native DWM frame: client={}x{} physical, scale={scale}, dark={dark}, light={light}, corner_policy={corners}, border={border}px", size.width, size.height);
                    Ok::<(), String>(())
                }.await;
                if let Err(error) = check { eprintln!("{error}"); app_handle.exit(1); }
                else { app_handle.exit(0); }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("native frame verification");
}
