//! Connection to the privileged helper.
//!
//! The GUI talks to the helper only through [`Helper`]. Requests are method calls; everything
//! the helper reports on its own (progress, found files, results) arrives as [`HelperEvent`]s
//! through the [`EventSink`] given when the connection is made. The real implementation will
//! forward both over IPC to the elevated helper process; [`mock::MockHelper`] simulates a
//! helper in-process so the GUI can be built and tried without one.

pub mod mock;

use hakk_protocol::{
    Destination, Drive, DriveId, HelperError, HelperEvent, RecoveryId, RecoveryRequest,
    RepairRequest, ScanId,
};

/// Receives events from the helper. Called from a helper thread, so it must not block.
pub type EventSink = Box<dyn Fn(HelperEvent) + Send + Sync>;

pub trait Helper: Send + Sync {
    /// Drives that can be scanned.
    fn list_drives(&self) -> Result<Vec<Drive>, HelperError>;

    /// Places recovered files can be written to while `source` is scanned. Destinations on
    /// the same device as `source` are listed, but blocked.
    fn list_destinations(&self, source: &DriveId) -> Result<Vec<Destination>, HelperError>;

    /// Starts a scan. Any previous scan is cancelled.
    fn start_scan(&self, drive: &DriveId) -> Result<ScanId, HelperError>;

    fn pause_scan(&self, scan: ScanId) -> Result<(), HelperError>;

    fn resume_scan(&self, scan: ScanId) -> Result<(), HelperError>;

    /// Stops a scan for good. Files found so far remain available.
    fn cancel_scan(&self, scan: ScanId) -> Result<(), HelperError>;

    /// Copies files to a destination. A running scan is paused until the copy finishes.
    fn recover(&self, request: RecoveryRequest) -> Result<RecoveryId, HelperError>;

    /// Writes a repaired copy of a video to a destination.
    fn repair(&self, request: RepairRequest) -> Result<(), HelperError>;
}
