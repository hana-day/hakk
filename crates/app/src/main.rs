//! hakk desktop GUI. Runs with normal privileges; disk access goes through the helper.

// Release builds on Windows must not open a console window next to the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod helper;

use tauri::{Emitter, Manager};

use crate::commands::HelperState;
use crate::helper::EventSink;
use crate::helper::mock::{MockConfig, MockHelper};

/// Name of the Tauri event that carries every `HelperEvent` to the web view.
/// Must match `HELPER_EVENT` in `ui/src/lib/api.ts`.
const HELPER_EVENT: &str = "helper-event";

/// Overrides the mock scan length (wall-clock seconds), e.g. `HAKK_MOCK_SCAN_SECONDS=10`.
const MOCK_SCAN_SECONDS_ENV: &str = "HAKK_MOCK_SCAN_SECONDS";

fn mock_config() -> MockConfig {
    let mut config = MockConfig::default();
    if let Some(seconds) = std::env::var(MOCK_SCAN_SECONDS_ENV)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|s| *s > 0.0)
    {
        config.scan_seconds = seconds;
    }
    config
}

fn main() {
    let result = tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let sink: EventSink = Box::new(move |event| {
                if let Err(err) = handle.emit(HELPER_EVENT, &event) {
                    eprintln!("failed to forward a helper event: {err}");
                }
            });
            // No real helper exists yet; the mock simulates one in-process.
            app.manage(HelperState(Box::new(MockHelper::new(sink, mock_config()))));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_drives,
            commands::list_destinations,
            commands::start_scan,
            commands::pause_scan,
            commands::resume_scan,
            commands::cancel_scan,
            commands::recover,
            commands::repair,
        ])
        .run(tauri::generate_context!());
    if let Err(err) = result {
        eprintln!("hakk failed to start: {err}");
        std::process::exit(1);
    }
}
