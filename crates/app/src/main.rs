//! hakk desktop GUI. Runs with normal privileges; disk access goes through the helper.

// Release builds on Windows must not open a console window next to the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod helper;

use hakk_helper::RealHelper;
use hakk_protocol::{EventSink, Helper};
use tauri::{Emitter, Manager};

use crate::commands::HelperState;
use crate::helper::mock::{MockConfig, MockHelper};

/// Name of the Tauri event that carries every `HelperEvent` to the web view.
/// Must match `HELPER_EVENT` in `ui/src/lib/api.ts`.
const HELPER_EVENT: &str = "helper-event";

/// `HAKK_HELPER=mock` runs the GUI against simulated drives instead of the real helper;
/// `HAKK_HELPER=inprocess` runs the real helper inside the GUI without asking for
/// administrator privileges (disk images only, unless the GUI itself runs as root).
const HELPER_ENV: &str = "HAKK_HELPER";

/// Text of the administrator password prompt.
const ELEVATION_PROMPT: &str = "hakk needs administrator privileges to read drives directly.";

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

fn helper(sink: &dyn Fn() -> EventSink) -> Box<dyn Helper> {
    let mode = std::env::var(HELPER_ENV).unwrap_or_default();
    if mode == "mock" {
        return Box::new(MockHelper::new(sink(), mock_config()));
    }
    let helper = elevated(sink, &mode).unwrap_or_else(|| Box::new(RealHelper::new(sink())));
    if let Some(paths) = std::env::var_os(OPEN_IMAGE_ENV) {
        for path in std::env::split_paths(&paths) {
            if let Err(err) = helper.open_image(&path.to_string_lossy()) {
                eprintln!("cannot open {}: {err}", path.display());
            }
        }
    }
    helper
}

/// The helper in a separate process with administrator privileges, if the user grants
/// them. `None` means the helper runs inside the GUI (already root, asked not to, the
/// prompt was declined, or the system has no such helper yet).
#[cfg(target_os = "macos")]
fn elevated(sink: &dyn Fn() -> EventSink, mode: &str) -> Option<Box<dyn Helper>> {
    // SAFETY: geteuid cannot fail and has no preconditions.
    if mode == "inprocess" || unsafe { libc::geteuid() } == 0 {
        return None;
    }
    match hakk_helper::remote::RemoteHelper::launch_elevated(sink(), ELEVATION_PROMPT) {
        Ok(h) => Some(Box::new(h)),
        Err(err) => {
            eprintln!("running without administrator privileges: {err}");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn elevated(_sink: &dyn Fn() -> EventSink, _mode: &str) -> Option<Box<dyn Helper>> {
    None
}

/// `hakk --serve-helper SOCKET [--owner UID:GID] [--data-dir DIR]`: run as the elevated
/// helper instead of the GUI. Returns whether it did.
#[cfg(unix)]
fn serve_helper() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let Some(i) = args
        .iter()
        .position(|a| a == hakk_helper::remote::SERVE_ARG)
    else {
        return false;
    };
    let Some(socket) = args.get(i + 1) else {
        eprintln!("{} needs a socket path", hakk_helper::remote::SERVE_ARG);
        std::process::exit(2);
    };
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|j| args.get(j + 1))
    };
    let owner = value("--owner").and_then(|o| hakk_helper::remote::parse_owner(o));
    let data_dir = value("--data-dir").map(std::path::PathBuf::from);
    if let Err(err) = hakk_helper::remote::serve(std::path::Path::new(socket), owner, data_dir) {
        eprintln!("helper: {err}");
        std::process::exit(1);
    }
    true
}

#[cfg(not(unix))]
fn serve_helper() -> bool {
    false
}

/// Sends this process's error output to `~/Library/Logs/hakk/hakk.log` when it was not
/// started from a terminal (launched from Finder, it would otherwise be lost).
#[cfg(target_os = "macos")]
fn log_to_file() {
    use std::os::fd::AsRawFd;
    // SAFETY: isatty only inspects the descriptor.
    if unsafe { libc::isatty(2) } == 1 {
        return;
    }
    let Some(dir) = hakk_helper::remote::log_dir() else {
        return;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("hakk.log"))
    else {
        return;
    };
    // SAFETY: both descriptors are valid; dup2 makes 2 refer to the log file, which stays
    // open for the life of the process (the original `file` handle is leaked on purpose).
    unsafe { libc::dup2(file.as_raw_fd(), 2) };
    std::mem::forget(file);
}

#[cfg(not(target_os = "macos"))]
fn log_to_file() {}

fn main() {
    if serve_helper() {
        return;
    }
    log_to_file();
    eprintln!("hakk {} starting", env!("CARGO_PKG_VERSION"));
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let sink = move || -> EventSink {
                let handle = handle.clone();
                Box::new(move |event| {
                    if let Err(err) = handle.emit(HELPER_EVENT, &event) {
                        eprintln!("failed to forward a helper event: {err}");
                    }
                })
            };
            app.manage(HelperState(helper(&sink)));
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
