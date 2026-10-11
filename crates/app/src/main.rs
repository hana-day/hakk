//! hakk desktop GUI. Runs with normal privileges; disk access goes through the helper.

// Release builds on Windows must not open a console window next to the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod helper;

use std::sync::Arc;

use hakk_helper::RealHelper;
use hakk_protocol::{EventSink, Helper};
use tauri::{Emitter, Manager};

use crate::commands::HelperState;
use crate::helper::mock::{MockConfig, MockHelper};

/// Name of the Tauri event that carries every `HelperEvent` to the web view.
/// Must match `HELPER_EVENT` in `ui/src/lib/api.ts`.
const HELPER_EVENT: &str = "helper-event";

/// `HAKK_HELPER=mock` runs the GUI against simulated drives instead of the real helper.
const HELPER_ENV: &str = "HAKK_HELPER";

/// Disk images to open at startup, separated like `PATH` (for development and tests).
const OPEN_IMAGE_ENV: &str = "HAKK_OPEN_IMAGE";

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

fn helper(sink: EventSink) -> Box<dyn Helper> {
    if std::env::var(HELPER_ENV).is_ok_and(|v| v == "mock") {
        return Box::new(MockHelper::new(sink, mock_config()));
    }
    // The file system and carving analyzers are being added to the scan crate; until then
    // scans read the disk and report progress but find nothing.
    let helper = RealHelper::new(sink, Arc::new(Vec::new));
    if let Some(paths) = std::env::var_os(OPEN_IMAGE_ENV) {
        for path in std::env::split_paths(&paths) {
            if let Err(err) = helper.open_image(&path.to_string_lossy()) {
                eprintln!("cannot open {}: {err}", path.display());
            }
        }
    }
    Box::new(helper)
}

fn main() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let sink: EventSink = Box::new(move |event| {
                if let Err(err) = handle.emit(HELPER_EVENT, &event) {
                    eprintln!("failed to forward a helper event: {err}");
                }
            });
            app.manage(HelperState(helper(sink)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_drives,
            commands::open_image,
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
