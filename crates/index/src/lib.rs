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
    BadRange, Counts, FileKind, FileRecord, Media, NewFile, Origin, Preview, ScanPhase,
    ScanProgress, ScanState, SourceInfo, Status, UnknownKind,
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
    /// Only files in this folder.
    pub folder_id: Option<i64>,
    /// Include records merged into another one.
    pub include_merged: bool,
    pub limit: Option<u32>,
}

/// A scan result index backed by SQLite.
pub struct Index {
    conn: Connection,
    /// (parent, name) -> folder id.
    folders: HashMap<(Option<i64>, String), i64>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index").finish_non_exhaustive()
    }
}

const FILE_COLUMNS: &str = "id, origin, kind, status, disk_offset, size, name, folder_id, fs_ref, \
     deleted, merged_into, width, height, duration_ms, taken_at, camera, thumb_offset, \
     thumb_len, created_at, modified_at";

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
            folders: HashMap::new(),
        })
    }

    pub fn schema_version(&self) -> Result<u32> {
        schema::user_version(&self.conn)
    }

    // ---- Scan state ------------------------------------------------------------------

    /// Starts a new scan: records the source and the planned ranges, and clears coverage
    /// left by a previous scan. Files found earlier are kept.
    pub fn begin_scan(&mut self, source: &SourceInfo, planned: &[Range<u64>]) -> Result<()> {
        let tx = self.conn.transaction()?;
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
        let tx = self.conn.transaction()?;
        let added = insert_files(&tx, &mut self.folders, files)?;
        write_progress(&tx, progress)?;
        match tx.commit() {
            Ok(()) => Ok(added),
            Err(e) => {
                // Cached folder ids may refer to rolled-back rows.
                self.folders.clear();
                Err(e.into())
            }
        }
    }

    // ---- Files -----------------------------------------------------------------------

    /// Stores files. Returns how many were new (re-reported files are ignored).
    pub fn insert(&mut self, files: &[NewFile]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let added = insert_files(&tx, &mut self.folders, files)?;
        match tx.commit() {
            Ok(()) => Ok(added),
            Err(e) => {
                self.folders.clear();
                Err(e.into())
            }
        }
    }

    pub fn file(&self, id: i64) -> Result<Option<FileRecord>> {
        let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?1");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query([id])?;
        rows.next()?.map(file_from_row).transpose()
    }

    /// Lists files in the order they were found.
    pub fn files(&self, query: &Query) -> Result<Vec<FileRecord>> {
        let mut sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE 1");
        let mut args: Vec<Value> = Vec::new();
        if let Some(origin) = query.origin {
            args.push(Value::Integer(origin.code()));
            sql.push_str(&format!(" AND origin = ?{}", args.len()));
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
            sql.push_str(&format!(" AND kind IN ({})", placeholders.join(", ")));
        }
        if let Some(after) = query.after_id {
            args.push(Value::Integer(after));
            sql.push_str(&format!(" AND id > ?{}", args.len()));
        }
        if let Some(folder) = query.folder_id {
            args.push(Value::Integer(folder));
            sql.push_str(&format!(" AND folder_id = ?{}", args.len()));
        }
        if !query.include_merged {
            sql.push_str(" AND merged_into IS NULL");
        }
        sql.push_str(" ORDER BY id");
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

    /// Child folders of `parent` (`None` for the root) as `(id, name)`.
    pub fn subfolders(&self, parent: Option<i64>) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, name FROM folders WHERE parent_id IS ?1 ORDER BY name, id",
        )?;
        let rows = stmt.query_map([parent], |r| Ok((r.get(0)?, r.get(1)?)))?;
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
            }
        }
        Ok(c)
    }

    // ---- Merging duplicates ----------------------------------------------------------

    /// Merges record `from` into `into`: `from` is hidden from listings, and preview
    /// information `into` lacks is taken from `from`. Records already merged into `from`
    /// are moved to `into`.
    pub fn merge(&mut self, from: i64, into: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        merge_in(&tx, from, into)?;
        tx.commit()?;
        Ok(())
    }

    /// Merges carving results into file system records describing the same file: same
    /// start offset and same media category (a `.MOV` named file may be carved as MP4).
    /// Returns how many records were merged.
    pub fn merge_duplicates(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
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
        let mut merged = 0;
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
            merge_in(&tx, carved, named)?;
            last_carved = Some(carved);
            merged += 1;
        }
        tx.commit()?;
        Ok(merged)
    }
}

fn merge_in(tx: &Transaction<'_>, from: i64, into: i64) -> Result<()> {
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
    tx.execute(
        "UPDATE files SET \
           status = CASE WHEN files.status = 0 THEN src.status ELSE files.status END, \
           width = ifnull(files.width, src.width), \
           height = ifnull(files.height, src.height), \
           duration_ms = ifnull(files.duration_ms, src.duration_ms), \
           taken_at = ifnull(files.taken_at, src.taken_at), \
           camera = ifnull(files.camera, src.camera), \
           thumb_offset = ifnull(files.thumb_offset, src.thumb_offset), \
           thumb_len = ifnull(files.thumb_len, src.thumb_len) \
         FROM (SELECT * FROM files WHERE id = ?1) AS src \
         WHERE files.id = ?2",
        params![from, into],
    )?;
    Ok(())
}

fn insert_files(
    tx: &Transaction<'_>,
    folders: &mut HashMap<(Option<i64>, String), i64>,
    files: &[NewFile],
) -> Result<usize> {
    let mut added = 0;
    for file in files {
        let Some(first) = file.extents.first() else {
            return Err(Error::InvalidFile("a file needs at least one extent"));
        };
        if file.extents.iter().any(|e| e.start > e.end) {
            return Err(Error::InvalidFile("extent end before start"));
        }
        let folder_id = if file.origin == Origin::FileSystem {
            folder_id(tx, folders, &file.folder)?
        } else {
            None
        };
        let p = &file.preview;
        let thumb = p.thumbnail.as_ref();
        let n = tx
            .prepare_cached(
                "INSERT INTO files (origin, kind, status, disk_offset, size, name, folder_id, \
                   fs_ref, deleted, width, height, duration_ms, taken_at, camera, \
                   thumb_offset, thumb_len, created_at, modified_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                   ?16, ?17, ?18) \
                 ON CONFLICT DO NOTHING",
            )?
            .execute(params![
                file.origin.code(),
                file.kind.code(),
                file.status.code(),
                to_i64(first.start)?,
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

fn folder_id(
    tx: &Transaction<'_>,
    cache: &mut HashMap<(Option<i64>, String), i64>,
    path: &[String],
) -> Result<Option<i64>> {
    let mut parent = None;
    for name in path {
        let key = (parent, name.clone());
        if let Some(&id) = cache.get(&key) {
            parent = Some(id);
            continue;
        }
        tx.prepare_cached(
            "INSERT INTO folders (parent_id, name) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
        )?
        .execute(params![parent, name])?;
        let id: i64 = tx
            .prepare_cached("SELECT id FROM folders WHERE parent_id IS ?1 AND name = ?2")?
            .query_row(params![parent, name], |r| r.get(0))?;
        cache.insert(key, id);
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
        offset: from_i64(r.get(4)?)?,
        size: from_i64(r.get(5)?)?,
        name: r.get(6)?,
        folder_id: r.get(7)?,
        fs_ref: r.get(8)?,
        deleted: r.get(9)?,
        merged_into: r.get(10)?,
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
