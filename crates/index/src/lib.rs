//! Scan result index (SQLite).
//!
//! A scan records where each file is (`offset`, extents, kind, preview information),
//! never the data itself. Results from file system analysis (with names and folders) and
//! from carving (without names) live in the same table; when a carving result turns out
//! to be the same file as a named one, it is merged into it ([`Index::merge_duplicates`]).
//!
//! The index also stores scan coverage (planned / done / bad ranges) so a scan can be
//! resumed after the app is closed. Findings and coverage are committed in one
//! transaction ([`Index::commit`]), so after a crash the index never claims a range is
//! done without also holding what was found in it.
//!
//! The database is opened in WAL mode: the GUI can read while the scan writes.

mod error;
mod model;
mod schema;

use std::{collections::HashMap, ops::Range, path::Path, time::Duration};

use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, params, params_from_iter, types::Value,
};

pub use error::Error;
pub use model::{
    BadRange, Confidence, Counts, FileKind, FileRecord, Media, NewFile, Origin, Preview, ScanPhase,
    ScanProgress, ScanState, SourceInfo, Status, UnknownKind, Volume, is_local_datetime,
};
pub use schema::SCHEMA_VERSION;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Filter for [`Index::files`].
#[derive(Debug, Clone, Default)]
pub struct Query {
    pub origin: Option<Origin>,
    pub media: Option<Media>,
    /// Only files with an id greater than this (files found after a given point).
    pub after_id: Option<i64>,
    /// Only files added or changed (status, merge, preview, ...) after this revision
    /// (see [`FileRecord::rev`]). Combine with `include_merged` to also see merges.
    pub changed_after: Option<i64>,
    /// Only files in this folder.
    pub folder_id: Option<i64>,
    /// Include records merged into another one.
    pub include_merged: bool,
    pub limit: Option<u32>,
}

/// A scan result index backed by SQLite.
pub struct Index {
    conn: Connection,
    ids: IdCache,
}

/// Ids of volumes and folders already in the database, to avoid looking them up for
/// every file. Cleared when a transaction fails (it may hold rolled-back rows).
#[derive(Default)]
struct IdCache {
    /// Partition index -> (volume id, whether its label is known).
    volumes: HashMap<u32, (i64, bool)>,
    /// (volume id, parent folder id, name) -> folder id.
    folders: HashMap<(Option<i64>, Option<i64>, String), i64>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index").finish_non_exhaustive()
    }
}

/// Columns read by `file_from_row`, from `FILE_FROM`.
const FILE_COLUMNS: &str = "f.id, f.origin, f.kind, f.status, f.disk_offset, f.size, f.name, \
     f.folder_id, f.fs_ref, f.deleted, f.merged_into, f.width, f.height, f.duration_ms, \
     f.taken_at, f.camera, f.thumb_offset, f.thumb_len, f.created_at, f.modified_at, \
     f.name_exact, f.confidence, v.part_index, v.label, f.rev";
const FILE_FROM: &str = "files AS f LEFT JOIN volumes AS v ON v.id = f.volume_id";

impl Index {
    /// Opens (creating if needed) an index file and upgrades its schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::migrate(&mut conn)?;
        Ok(Self {
            conn,
            ids: IdCache::default(),
        })
    }

    pub fn schema_version(&self) -> Result<u32> {
        schema::user_version(&self.conn)
    }

    // ---- Scan state ------------------------------------------------------------------

    /// Starts a new scan: records the source and the planned ranges, and clears coverage
    /// left by a previous scan. Files found earlier are kept.
    pub fn begin_scan(&mut self, source: &SourceInfo, planned: &[Range<u64>]) -> Result<()> {
        let tx = write(&mut self.conn)?;
        set_meta(&tx, "source_len", Value::Integer(to_i64(source.len)?))?;
        set_meta(
            &tx,
            "source_sector_size",
            Value::Integer(i64::from(source.sector_size)),
        )?;
        set_meta(&tx, "source_label", Value::Text(source.label.clone()))?;
        tx.execute("DELETE FROM scan_planned", [])?;
        write_ranges(&tx, "scan_planned", planned)?;
        write_progress(&tx, &ScanProgress::default())?;
        tx.commit()?;
        Ok(())
    }

    /// The saved scan, if [`Index::begin_scan`] was ever called.
    pub fn scan_state(&self) -> Result<Option<ScanState>> {
        let Some(len) = get_meta_i64(&self.conn, "source_len")? else {
            return Ok(None);
        };
        let sector_size = get_meta_i64(&self.conn, "source_sector_size")?.unwrap_or(0);
        let label: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'source_label'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let source = SourceInfo {
            len: from_i64(len)?,
            sector_size: u32::try_from(sector_size)
                .map_err(|_| Error::Corrupt(format!("bad sector size {sector_size}")))?,
            label: label.unwrap_or_default(),
        };
        let phase_code = get_meta_i64(&self.conn, "scan_phase")?.unwrap_or(0);
        let phase = ScanPhase::from_code(phase_code)
            .ok_or_else(|| Error::Corrupt(format!("bad scan phase {phase_code}")))?;
        let retry_passes = get_meta_i64(&self.conn, "scan_retry_passes")?.unwrap_or(0);

        let mut bad = Vec::new();
        let mut stmt = self.conn.prepare_cached(
            "SELECT range_start, range_end, attempts FROM bad_ranges ORDER BY range_start",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let attempts: i64 = r.get(2)?;
            bad.push(BadRange {
                range: from_i64(r.get(0)?)?..from_i64(r.get(1)?)?,
                attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
            });
        }

        Ok(Some(ScanState {
            source,
            planned: read_ranges(&self.conn, "scan_planned")?,
            progress: ScanProgress {
                done: read_ranges(&self.conn, "scan_done")?,
                bad,
                phase,
                retry_passes: u32::try_from(retry_passes).unwrap_or(u32::MAX),
            },
        }))
    }

    /// Atomically stores newly found files together with the scan progress.
    /// Returns how many files were new (re-reported files are ignored).
    pub fn commit(&mut self, files: &[NewFile], progress: &ScanProgress) -> Result<usize> {
        let tx = write(&mut self.conn)?;
        let added = insert_files(&tx, &mut self.ids, files)?;
        write_progress(&tx, progress)?;
        match tx.commit() {
            Ok(()) => Ok(added),
            Err(e) => {
                // Cached folder ids may refer to rolled-back rows.
                self.ids = IdCache::default();
                Err(e.into())
            }
        }
    }

    // ---- Files -----------------------------------------------------------------------

    /// Stores files. Returns how many were new (re-reported files are ignored).
    pub fn insert(&mut self, files: &[NewFile]) -> Result<usize> {
        let tx = write(&mut self.conn)?;
        let added = insert_files(&tx, &mut self.ids, files)?;
        match tx.commit() {
            Ok(()) => Ok(added),
            Err(e) => {
                self.ids = IdCache::default();
                Err(e.into())
            }
        }
    }

    pub fn file(&self, id: i64) -> Result<Option<FileRecord>> {
        let sql = format!("SELECT {FILE_COLUMNS} FROM {FILE_FROM} WHERE f.id = ?1");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query([id])?;
        rows.next()?.map(file_from_row).transpose()
    }

    /// Lists files in the order they were found.
    pub fn files(&self, query: &Query) -> Result<Vec<FileRecord>> {
        let mut sql = format!("SELECT {FILE_COLUMNS} FROM {FILE_FROM} WHERE 1");
        let mut args: Vec<Value> = Vec::new();
        if let Some(origin) = query.origin {
            args.push(Value::Integer(origin.code()));
            sql.push_str(&format!(" AND f.origin = ?{}", args.len()));
        }
        if let Some(media) = query.media {
            let kinds: Vec<&str> = model::FileKind::all()
                .filter(|k| k.media() == media)
                .map(FileKind::code)
                .collect();
            let mut placeholders = Vec::new();
            for kind in kinds {
                args.push(Value::Text(kind.to_owned()));
                placeholders.push(format!("?{}", args.len()));
            }
            sql.push_str(&format!(" AND f.kind IN ({})", placeholders.join(", ")));
        }
        if let Some(after) = query.after_id {
            args.push(Value::Integer(after));
            sql.push_str(&format!(" AND f.id > ?{}", args.len()));
        }
        if let Some(rev) = query.changed_after {
            args.push(Value::Integer(rev));
            sql.push_str(&format!(" AND f.rev > ?{}", args.len()));
        }
        if let Some(folder) = query.folder_id {
            args.push(Value::Integer(folder));
            sql.push_str(&format!(" AND f.folder_id = ?{}", args.len()));
        }
        if !query.include_merged {
            sql.push_str(" AND f.merged_into IS NULL");
        }
        sql.push_str(" ORDER BY f.id");
        if let Some(limit) = query.limit {
            args.push(Value::Integer(i64::from(limit)));
            sql.push_str(&format!(" LIMIT ?{}", args.len()));
        }
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query(params_from_iter(args))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(file_from_row(row)?);
        }
        Ok(out)
    }

    /// The latest revision (0 if no file was ever stored). Every added or changed file
    /// record gets a new, larger revision.
    pub fn revision(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT value FROM revision WHERE id = 1", [], |r| r.get(0))?)
    }

    /// The largest file id so far (0 if none). Pair with [`Query::after_id`] to show
    /// "N new files found" without reordering what is on screen.
    pub fn last_id(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT ifnull(max(id), 0) FROM files", [], |r| r.get(0))?)
    }

    /// Where a file's data is, in file order.
    pub fn extents(&self, file_id: i64) -> Result<Vec<Range<u64>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT disk_offset, len FROM extents WHERE file_id = ?1 ORDER BY seq",
        )?;
        let mut rows = stmt.query([file_id])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let start = from_i64(r.get(0)?)?;
            let len = from_i64(r.get(1)?)?;
            out.push(start..start.saturating_add(len));
        }
        Ok(out)
    }

    /// Folder path components from the root.
    pub fn folder_path(&self, folder_id: i64) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT parent_id, name FROM folders WHERE id = ?1")?;
        let mut path = Vec::new();
        let mut next = Some(folder_id);
        while let Some(id) = next {
            // A cycle cannot be created through this API, but do not loop forever on a
            // corrupted file.
            if path.len() > 4096 {
                return Err(Error::Corrupt("folder nesting too deep".into()));
            }
            let (parent, name): (Option<i64>, String) = stmt
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| Error::Corrupt(format!("missing folder {id}")))?;
            path.push(name);
            next = parent;
        }
        path.reverse();
        Ok(path)
    }

    /// Top-level folders of a volume (by partition index) as `(id, name)`.
    pub fn root_folders(&self, volume: u32) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.id, f.name FROM folders AS f JOIN volumes AS v ON v.id = f.volume_id \
             WHERE v.part_index = ?1 AND f.parent_id IS NULL ORDER BY f.name, f.id",
        )?;
        let rows = stmt.query_map([volume], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Child folders of a folder as `(id, name)`.
    pub fn subfolders(&self, parent: i64) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, name FROM folders WHERE parent_id = ?1 ORDER BY name, id",
        )?;
        let rows = stmt.query_map([parent], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Volumes that have file system records, by partition index.
    pub fn volumes(&self) -> Result<Vec<Volume>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT part_index, label FROM volumes ORDER BY part_index")?;
        let rows = stmt.query_map([], |r| {
            Ok(Volume {
                index: r.get(0)?,
                label: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Updates a file's status (e.g. after verifying it decodes).
    pub fn set_status(&mut self, file_id: i64, status: Status) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE files SET status = ?2 WHERE id = ?1",
            params![file_id, status.code()],
        )?;
        if n == 0 {
            return Err(Error::NoSuchFile(file_id));
        }
        Ok(())
    }

    /// Number of (unmerged) files per category.
    pub fn counts(&self) -> Result<Counts> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT kind, status, count(*) FROM files WHERE merged_into IS NULL \
             GROUP BY kind, status",
        )?;
        let mut rows = stmt.query([])?;
        let mut c = Counts::default();
        while let Some(r) = rows.next()? {
            let kind: String = r.get(0)?;
            let kind: FileKind = kind.parse()?;
            let status = status_from(r.get(1)?)?;
            let n = from_i64(r.get(2)?)?;
            match kind.media() {
                Media::Photo => c.photos += n,
                Media::Video => c.videos += n,
            }
            match status {
                Status::Unknown => c.unknown += n,
                Status::Recoverable => c.recoverable += n,
                Status::Damaged => c.damaged += n,
                Status::MissingIndex => c.missing_index += n,
                Status::Repaired => c.repaired += n,
            }
        }
        Ok(c)
    }

    // ---- Merging duplicates ----------------------------------------------------------

    /// Merges record `from` into `into`: `from` is hidden from listings, and preview
    /// information `into` lacks is taken from `from`. Records already merged into `from`
    /// are moved to `into`.
    pub fn merge(&mut self, from: i64, into: i64) -> Result<()> {
        let tx = write(&mut self.conn)?;
        merge_in(&tx, from, into, Copy::PreviewAndStatus)?;
        tx.commit()?;
        Ok(())
    }

    /// Folds carving results into the records they duplicate. Returns the merges made as
    /// `(from, into)` pairs (for telling the GUI which entries to drop). Idempotent; safe to
    /// call during a scan.
    ///
    /// 1. A carving result at the same start offset as a file system record of the same
    ///    media category (a `.MOV` named file may be carved as MP4) is that file: it is
    ///    merged into the named record, which keeps its name and folder and takes over the
    ///    preview information it lacks. Where the data is:
    ///    - If the file system did not know the placement for sure (confidence below
    ///      High: e.g. a deleted FAT file assumed to be contiguous) and the carver
    ///      verified a complete file of exactly the recorded size, the carver's extents
    ///      replace the guessed ones (it reassembled the fragments the guess got wrong).
    ///    - Otherwise the file system's extents stay. The carver's status is taken only if
    ///      both describe the same data; a verdict on other bytes says nothing about these.
    /// 2. A carving result lying entirely inside another complete, contiguous carving
    ///    result is part of it (e.g. a JPEG frame inside a video, which parallel carving
    ///    cannot always skip): it is merged into the container, without copying anything.
    pub fn merge_duplicates(&mut self) -> Result<Vec<(i64, i64)>> {
        let tx = write(&mut self.conn)?;
        let pairs: Vec<(i64, String, i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT c.id, c.kind, f.id, f.kind FROM files c \
                 JOIN files f ON f.disk_offset = c.disk_offset \
                 WHERE c.origin = 2 AND c.merged_into IS NULL \
                   AND f.origin = 1 AND f.merged_into IS NULL \
                 ORDER BY c.id, f.id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut merged = Vec::new();
        let mut last_carved = None;
        for (carved, carved_kind, named, named_kind) in pairs {
            if last_carved == Some(carved) {
                continue;
            }
            let carved_kind: FileKind = carved_kind.parse()?;
            let named_kind: FileKind = named_kind.parse()?;
            if carved_kind.media() != named_kind.media() {
                continue;
            }
            let copy = match placement(&tx, carved, named)? {
                Placement::Same => Copy::PreviewAndStatus,
                Placement::TakeCarved(extents) => {
                    replace_extents(&tx, named, &extents)?;
                    Copy::PreviewAndStatus
                }
                Placement::KeepNamed => Copy::Preview,
            };
            merge_in(&tx, carved, named, copy)?;
            last_carved = Some(carved);
            merged.push((carved, named));
        }

        // Carving results inside a complete one, by a sweep in offset order.
        let carved: Vec<(i64, i64, i64, i64, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT f.id, f.disk_offset, f.size, f.status, \
                   (SELECT count(*) FROM extents AS e WHERE e.file_id = f.id) \
                 FROM files AS f \
                 WHERE f.origin = 2 AND f.merged_into IS NULL AND f.disk_offset IS NOT NULL \
                 ORDER BY f.disk_offset, f.size DESC, f.id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        // (id, start, end) of the complete file reaching furthest so far.
        let mut container: Option<(i64, u64, u64)> = None;
        for (id, offset, size, status, extents) in carved {
            let start = from_i64(offset)?;
            let end = start.saturating_add(from_i64(size)?);
            if let Some((cid, cstart, cend)) = container
                && start > cstart
                && end <= cend
            {
                merge_in(&tx, id, cid, Copy::Nothing)?;
                merged.push((id, cid));
                continue;
            }
            let complete = status == Status::Recoverable.code() && extents == 1;
            if complete && container.is_none_or(|(_, _, cend)| end > cend) {
                container = Some((id, start, end));
            }
        }
        tx.commit()?;
        Ok(merged)
    }
}

/// Starts a write transaction holding the write lock from the start. Another
/// connection (e.g. the GUI side merging duplicates while the scan commits) then waits
/// for it through the busy timeout. A deferred transaction would read first and fail
/// at once with "database is locked" if the other writer committed in between.
fn write(conn: &mut Connection) -> Result<Transaction<'_>> {
    Ok(conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?)
}

/// What a merged record passes on to the record it is merged into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Copy {
    Nothing,
    /// Preview information the target lacks (dimensions, capture time...).
    Preview,
    /// Also the verification status, if the target's is unknown.
    PreviewAndStatus,
}

/// Which extents a carved record and a file system record for the same file should
/// end up with.
enum Placement {
    /// Both describe the same bytes.
    Same,
    /// The file system's placement was a guess; the carver verified a complete file of
    /// the recorded size elsewhere in places.
    TakeCarved(Vec<Range<u64>>),
    KeepNamed,
}

fn placement(tx: &Transaction<'_>, carved: i64, named: i64) -> Result<Placement> {
    let carved_extents = extents_in(tx, carved)?;
    let named_extents = extents_in(tx, named)?;
    if carved_extents == named_extents {
        return Ok(Placement::Same);
    }
    let (carved_status, carved_size): (i64, i64) = tx.query_row(
        "SELECT status, size FROM files WHERE id = ?1",
        [carved],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (named_confidence, named_size): (Option<i64>, i64) = tx.query_row(
        "SELECT confidence, size FROM files WHERE id = ?1",
        [named],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let guessed = named_confidence != Some(Confidence::High.code());
    if guessed && carved_status == Status::Recoverable.code() && carved_size == named_size {
        Ok(Placement::TakeCarved(carved_extents))
    } else {
        Ok(Placement::KeepNamed)
    }
}

fn extents_in(tx: &Transaction<'_>, file_id: i64) -> Result<Vec<Range<u64>>> {
    let mut stmt =
        tx.prepare_cached("SELECT disk_offset, len FROM extents WHERE file_id = ?1 ORDER BY seq")?;
    let mut rows = stmt.query([file_id])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let start = from_i64(r.get(0)?)?;
        out.push(start..start.saturating_add(from_i64(r.get(1)?)?));
    }
    Ok(out)
}

fn replace_extents(tx: &Transaction<'_>, file_id: i64, extents: &[Range<u64>]) -> Result<()> {
    tx.execute("DELETE FROM extents WHERE file_id = ?1", [file_id])?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO extents (file_id, seq, disk_offset, len) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (seq, e) in (0i64..).zip(extents) {
        stmt.execute(params![
            file_id,
            seq,
            to_i64(e.start)?,
            to_i64(e.end - e.start)?
        ])?;
    }
    // The start does not change (records are paired by it), but keep it in step; this
    // also bumps the record's revision.
    tx.execute(
        "UPDATE files SET disk_offset = ?2 WHERE id = ?1",
        params![
            file_id,
            extents.first().map(|e| to_i64(e.start)).transpose()?
        ],
    )?;
    Ok(())
}

/// Merges `from` into `into`, passing on what `copy` says.
fn merge_in(tx: &Transaction<'_>, from: i64, into: i64, copy: Copy) -> Result<()> {
    if from == into {
        return Err(Error::InvalidMerge("cannot merge a file into itself"));
    }
    let target: Option<Option<i64>> = tx
        .query_row("SELECT merged_into FROM files WHERE id = ?1", [into], |r| {
            r.get(0)
        })
        .optional()?;
    match target {
        None => return Err(Error::NoSuchFile(into)),
        Some(Some(_)) => return Err(Error::InvalidMerge("target is itself merged")),
        Some(None) => {}
    }
    let n = tx.execute(
        "UPDATE files SET merged_into = ?2 WHERE id = ?1",
        params![from, into],
    )?;
    if n == 0 {
        return Err(Error::NoSuchFile(from));
    }
    tx.execute(
        "UPDATE files SET merged_into = ?2 WHERE merged_into = ?1",
        params![from, into],
    )?;
    if copy == Copy::Nothing {
        return Ok(());
    }
    tx.execute(
        "UPDATE files SET \
           status = CASE WHEN files.status = 0 AND ?3 THEN src.status ELSE files.status END, \
           width = ifnull(files.width, src.width), \
           height = ifnull(files.height, src.height), \
           duration_ms = ifnull(files.duration_ms, src.duration_ms), \
           taken_at = ifnull(files.taken_at, src.taken_at), \
           camera = ifnull(files.camera, src.camera), \
           thumb_offset = ifnull(files.thumb_offset, src.thumb_offset), \
           thumb_len = ifnull(files.thumb_len, src.thumb_len) \
         FROM (SELECT * FROM files WHERE id = ?1) AS src \
         WHERE files.id = ?2",
        params![from, into, copy == Copy::PreviewAndStatus],
    )?;
    Ok(())
}

fn insert_files(tx: &Transaction<'_>, ids: &mut IdCache, files: &[NewFile]) -> Result<usize> {
    let mut added = 0;
    for file in files {
        if file.extents.iter().any(|e| e.start > e.end) {
            return Err(Error::InvalidFile("extent end before start"));
        }
        if file.origin == Origin::Carving && file.extents.is_empty() {
            return Err(Error::InvalidFile("a carving result needs its location"));
        }
        let p = &file.preview;
        let dates = [&p.taken_at, &p.created_at, &p.modified_at];
        if dates
            .iter()
            .any(|d| d.as_deref().is_some_and(|d| !is_local_datetime(d)))
        {
            return Err(Error::InvalidFile(
                "dates must be ISO 8601 local date-times (YYYY-MM-DDTHH:MM:SS)",
            ));
        }
        let (volume_id, folder_id) = if file.origin == Origin::FileSystem {
            let volume_id = file
                .volume
                .as_ref()
                .map(|v| volume_id(tx, ids, v))
                .transpose()?;
            (volume_id, folder_id(tx, ids, volume_id, &file.folder)?)
        } else {
            (None, None)
        };
        let thumb = p.thumbnail.as_ref();
        let n = tx
            .prepare_cached(
                "INSERT INTO files (origin, kind, status, disk_offset, size, name, folder_id, \
                   fs_ref, deleted, width, height, duration_ms, taken_at, camera, \
                   thumb_offset, thumb_len, created_at, modified_at, volume_id, name_exact, \
                   confidence) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                   ?16, ?17, ?18, ?19, ?20, ?21) \
                 ON CONFLICT DO NOTHING",
            )?
            .execute(params![
                file.origin.code(),
                file.kind.code(),
                file.status.code(),
                file.extents.first().map(|e| to_i64(e.start)).transpose()?,
                to_i64(file.size)?,
                file.name,
                folder_id,
                file.fs_ref,
                file.deleted,
                p.width,
                p.height,
                p.duration_ms.map(to_i64).transpose()?,
                p.taken_at,
                p.camera,
                thumb.map(|t| to_i64(t.start)).transpose()?,
                thumb
                    .map(|t| to_i64(t.end.saturating_sub(t.start)))
                    .transpose()?,
                p.created_at,
                p.modified_at,
                volume_id,
                file.name_exact,
                file.confidence.map(Confidence::code),
            ])?;
        if n == 0 {
            continue;
        }
        let id = tx.last_insert_rowid();
        let mut stmt = tx.prepare_cached(
            "INSERT INTO extents (file_id, seq, disk_offset, len) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (seq, e) in (0i64..).zip(&file.extents) {
            stmt.execute(params![id, seq, to_i64(e.start)?, to_i64(e.end - e.start)?])?;
        }
        added += 1;
    }
    Ok(added)
}

fn volume_id(tx: &Transaction<'_>, ids: &mut IdCache, volume: &Volume) -> Result<i64> {
    if let Some(&(id, labelled)) = ids.volumes.get(&volume.index)
        && (labelled || volume.label.is_none())
    {
        return Ok(id);
    }
    // A later report may know the label when an earlier one did not.
    tx.prepare_cached(
        "INSERT INTO volumes (part_index, label) VALUES (?1, ?2) \
         ON CONFLICT (part_index) DO UPDATE SET label = ifnull(excluded.label, label)",
    )?
    .execute(params![volume.index, volume.label])?;
    let id: i64 = tx
        .prepare_cached("SELECT id FROM volumes WHERE part_index = ?1")?
        .query_row([volume.index], |r| r.get(0))?;
    ids.volumes
        .insert(volume.index, (id, volume.label.is_some()));
    Ok(id)
}

fn folder_id(
    tx: &Transaction<'_>,
    ids: &mut IdCache,
    volume: Option<i64>,
    path: &[String],
) -> Result<Option<i64>> {
    let mut parent = None;
    for name in path {
        let key = (volume, parent, name.clone());
        if let Some(&id) = ids.folders.get(&key) {
            parent = Some(id);
            continue;
        }
        tx.prepare_cached(
            "INSERT INTO folders (volume_id, parent_id, name) VALUES (?1, ?2, ?3) \
             ON CONFLICT DO NOTHING",
        )?
        .execute(params![volume, parent, name])?;
        let id: i64 = tx
            .prepare_cached(
                "SELECT id FROM folders WHERE volume_id IS ?1 AND parent_id IS ?2 AND name = ?3",
            )?
            .query_row(params![volume, parent, name], |r| r.get(0))?;
        ids.folders.insert(key, id);
        parent = Some(id);
    }
    Ok(parent)
}

fn file_from_row(r: &Row<'_>) -> Result<FileRecord> {
    let origin_code: i64 = r.get(1)?;
    let kind: String = r.get(2)?;
    let thumb_offset: Option<i64> = r.get(16)?;
    let thumb_len: Option<i64> = r.get(17)?;
    let thumbnail = match (thumb_offset, thumb_len) {
        (Some(o), Some(l)) => {
            let o = from_i64(o)?;
            Some(o..o.saturating_add(from_i64(l)?))
        }
        _ => None,
    };
    Ok(FileRecord {
        id: r.get(0)?,
        origin: Origin::from_code(origin_code)
            .ok_or_else(|| Error::Corrupt(format!("bad origin {origin_code}")))?,
        kind: kind.parse()?,
        status: status_from(r.get(3)?)?,
        offset: r.get::<_, Option<i64>>(4)?.map(from_i64).transpose()?,
        size: from_i64(r.get(5)?)?,
        name: r.get(6)?,
        volume: match r.get::<_, Option<u32>>(22)? {
            Some(index) => Some(Volume {
                index,
                label: r.get(23)?,
            }),
            None => None,
        },
        folder_id: r.get(7)?,
        name_exact: r.get(20)?,
        fs_ref: r.get(8)?,
        deleted: r.get(9)?,
        confidence: match r.get::<_, Option<i64>>(21)? {
            Some(c) => Some(
                Confidence::from_code(c)
                    .ok_or_else(|| Error::Corrupt(format!("bad confidence {c}")))?,
            ),
            None => None,
        },
        merged_into: r.get(10)?,
        rev: r.get(24)?,
        preview: Preview {
            width: r.get(11)?,
            height: r.get(12)?,
            duration_ms: r.get::<_, Option<i64>>(13)?.map(from_i64).transpose()?,
            taken_at: r.get(14)?,
            camera: r.get(15)?,
            thumbnail,
            created_at: r.get(18)?,
            modified_at: r.get(19)?,
        },
    })
}

fn status_from(code: i64) -> Result<Status> {
    Status::from_code(code).ok_or_else(|| Error::Corrupt(format!("bad status {code}")))
}

fn write_progress(tx: &Transaction<'_>, p: &ScanProgress) -> Result<()> {
    set_meta(tx, "scan_phase", Value::Integer(p.phase.code()))?;
    set_meta(
        tx,
        "scan_retry_passes",
        Value::Integer(i64::from(p.retry_passes)),
    )?;
    tx.execute("DELETE FROM scan_done", [])?;
    write_ranges(tx, "scan_done", &p.done)?;
    tx.execute("DELETE FROM bad_ranges", [])?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO bad_ranges (range_start, range_end, attempts) VALUES (?1, ?2, ?3)",
    )?;
    for b in &p.bad {
        stmt.execute(params![
            to_i64(b.range.start)?,
            to_i64(b.range.end)?,
            b.attempts
        ])?;
    }
    Ok(())
}

/// `table` is one of our own table names, never user input.
fn write_ranges(tx: &Transaction<'_>, table: &str, ranges: &[Range<u64>]) -> Result<()> {
    let mut stmt = tx.prepare_cached(&format!(
        "INSERT INTO {table} (range_start, range_end) VALUES (?1, ?2)"
    ))?;
    for r in ranges {
        stmt.execute(params![to_i64(r.start)?, to_i64(r.end)?])?;
    }
    Ok(())
}

fn read_ranges(conn: &Connection, table: &str) -> Result<Vec<Range<u64>>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT range_start, range_end FROM {table} ORDER BY range_start"
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(from_i64(r.get(0)?)?..from_i64(r.get(1)?)?);
    }
    Ok(out)
}

fn set_meta(tx: &Transaction<'_>, key: &str, value: Value) -> Result<()> {
    tx.prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")?
        .execute(params![key, value])?;
    Ok(())
}

fn get_meta_i64(conn: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(conn
        .prepare_cached("SELECT value FROM meta WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?)
}

/// SQLite integers are signed; offsets above `i64::MAX` cannot be stored.
fn to_i64(x: u64) -> Result<i64> {
    i64::try_from(x).map_err(|_| Error::OutOfRange(x))
}

fn from_i64(x: i64) -> Result<u64> {
    u64::try_from(x).map_err(|_| Error::Corrupt(format!("negative value {x}")))
}
