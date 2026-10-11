//! Tauri commands: the GUI's requests, forwarded to the helper.
//!
//! Commands run on Tauri's async thread pool (`async` attribute) so a slow helper never
//! blocks the window's event loop.

use hakk_protocol::{
    Destination, Drive, DriveId, HelperError, RecoveryId, RecoveryRequest, RepairRequest, ScanId,
};
use tauri::State;

use crate::helper::Helper;

/// The helper connection, shared by all commands.
pub struct HelperState(pub Box<dyn Helper>);

#[tauri::command(async)]
pub fn list_drives(helper: State<'_, HelperState>) -> Result<Vec<Drive>, HelperError> {
    helper.0.list_drives()
}

#[tauri::command(async)]
pub fn list_destinations(
    helper: State<'_, HelperState>,
    source: DriveId,
) -> Result<Vec<Destination>, HelperError> {
    helper.0.list_destinations(&source)
}

#[tauri::command(async)]
pub fn start_scan(helper: State<'_, HelperState>, drive: DriveId) -> Result<ScanId, HelperError> {
    helper.0.start_scan(&drive)
}

#[tauri::command(async)]
pub fn pause_scan(helper: State<'_, HelperState>, scan: ScanId) -> Result<(), HelperError> {
    helper.0.pause_scan(scan)
}

#[tauri::command(async)]
pub fn resume_scan(helper: State<'_, HelperState>, scan: ScanId) -> Result<(), HelperError> {
    helper.0.resume_scan(scan)
}

#[tauri::command(async)]
pub fn cancel_scan(helper: State<'_, HelperState>, scan: ScanId) -> Result<(), HelperError> {
    helper.0.cancel_scan(scan)
}

#[tauri::command(async)]
pub fn recover(
    helper: State<'_, HelperState>,
    request: RecoveryRequest,
) -> Result<RecoveryId, HelperError> {
    helper.0.recover(request)
}

#[tauri::command(async)]
pub fn repair(helper: State<'_, HelperState>, request: RepairRequest) -> Result<(), HelperError> {
    helper.0.repair(request)
}
