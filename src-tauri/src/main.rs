// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    if let Some(code) = xarchive_lib::cli::run_if_requested() {
        return std::process::ExitCode::from(code);
    }
    xarchive_lib::run();
    std::process::ExitCode::SUCCESS
}
