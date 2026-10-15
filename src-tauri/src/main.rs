// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    if let Some(code) = xarchive_lib::cli::run_if_requested() {
        return std::process::ExitCode::from(code);
    }
    match xarchive_lib::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            startup_error(&error);
            std::process::ExitCode::FAILURE
        }
    }
}

fn startup_error(error: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let message: Vec<u16> = format!("无法启动 XArchive：\n{error}")
            .encode_utf16()
            .chain([0])
            .collect();
        let title: Vec<u16> = "XArchive".encode_utf16().chain([0]).collect();
        // No Tauri runtime or WebView is needed to explain an early startup failure.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "无法启动 XArchive：{error}");
    }
}
