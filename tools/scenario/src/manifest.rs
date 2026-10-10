//! The ground-truth manifest. The evaluator scores recovered files against it.

use serde::Serialize;

use crate::{
    content::Meta,
    locate::Extent,
    scenario::{ContentKind, FsKind},
};

pub const VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Manifest {
    pub version: u32,
    pub scenario: String,
    pub seed: u64,
    pub fs: FsKind,
    /// Path relative to the manifest.
    pub image: String,
    pub image_size: u64,
    /// Where the file system starts (the first partition if there is a partition table).
    pub partition_offset: u64,
    /// Cluster size of the file system (read from its boot sector).
    pub cluster_size: Option<u64>,
    /// Where the image was built (recorded because layout depends on the OS driver).
    pub generator: Generator,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Serialize)]
pub struct Generator {
    pub os: String,
    pub os_version: String,
    pub tool_version: String,
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub id: u32,
    /// Path within the volume (`/`-separated).
    pub path: String,
    pub kind: ContentKind,
    /// Device profile the file was generated with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub size: u64,
    pub sha256: String,
    /// Index of the step that wrote the file.
    pub written_at: usize,
    /// Index of the step that deleted the file (including formats); `None` if still live.
    pub deleted_at: Option<usize>,
    /// Placement right after writing; `None` if it could not be determined.
    pub extents: Option<Vec<Extent>>,
    pub fragmented: bool,
    /// Bytes still holding the original content in the final image.
    pub intact_bytes: u64,
    #[serde(flatten)]
    pub meta: Meta,
}
