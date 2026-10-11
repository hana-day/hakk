//! Schema versions and migrations.
//!
//! The schema version is kept in SQLite's `user_version`. Each migration upgrades the
//! schema by one version and runs in its own transaction, so a failed upgrade leaves the
//! database at the previous version. Never edit a released migration; add a new one.

use rusqlite::{Connection, OptionalExtension};

use crate::Error;

/// `PRAGMA application_id` marking a hakk index ("hakk" in ASCII).
pub(crate) const APPLICATION_ID: i32 = 0x6861_6b6b;

/// One schema upgrade, from version `i` to `i + 1` where `i` is its position in the list.
pub(crate) struct Migration {
    pub(crate) description: &'static str,
    pub(crate) sql: &'static str,
}

pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        description: "initial schema",
        sql: V1,
    },
    Migration {
        description: "volumes, unknown offsets, confidence, guessed names, local dates",
        sql: V2,
    },
];

/// The schema version this build reads and writes. Always equals `MIGRATIONS.len()`.
pub const SCHEMA_VERSION: u32 = 2;

const V1: &str = "
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value ANY
) STRICT, WITHOUT ROWID;

-- Folder tree recovered from file system metadata.
CREATE TABLE folders (
    id        INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES folders (id),
    name      TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX folders_parent_name ON folders (ifnull(parent_id, 0), name);

-- One row per found file. Only the location is stored, never the data.
CREATE TABLE files (
    id           INTEGER PRIMARY KEY,
    origin       INTEGER NOT NULL,  -- 1 = file system, 2 = carving
    kind         TEXT NOT NULL,     -- FileKind code
    status       INTEGER NOT NULL,  -- Status code
    disk_offset  INTEGER NOT NULL,  -- start of the first extent
    size         INTEGER NOT NULL,
    name         TEXT,
    folder_id    INTEGER REFERENCES folders (id),
    fs_ref       TEXT,
    deleted      INTEGER NOT NULL,
    merged_into  INTEGER REFERENCES files (id),
    width        INTEGER,
    height       INTEGER,
    duration_ms  INTEGER,
    taken_at     INTEGER,
    camera       TEXT,
    thumb_offset INTEGER,
    thumb_len    INTEGER,
    created_at   INTEGER,
    modified_at  INTEGER
) STRICT;
CREATE INDEX files_disk_offset ON files (disk_offset);
CREATE INDEX files_folder ON files (folder_id);
-- Re-reporting the same file (e.g. after resuming a scan) must not add a second row.
CREATE UNIQUE INDEX files_carved_identity ON files (disk_offset, kind) WHERE origin = 2;
CREATE UNIQUE INDEX files_fs_identity ON files (fs_ref) WHERE origin = 1 AND fs_ref IS NOT NULL;

-- Data location of each file, in file order.
CREATE TABLE extents (
    file_id     INTEGER NOT NULL REFERENCES files (id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    disk_offset INTEGER NOT NULL,
    len         INTEGER NOT NULL,
    PRIMARY KEY (file_id, seq)
) STRICT, WITHOUT ROWID;

-- Scan coverage: what to read, what has been read, and what could not be read.
CREATE TABLE scan_planned (
    range_start INTEGER PRIMARY KEY,
    range_end   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE scan_done (
    range_start INTEGER PRIMARY KEY,
    range_end   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE bad_ranges (
    range_start INTEGER PRIMARY KEY,
    range_end   INTEGER NOT NULL,
    attempts    INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
";

/// v2:
/// - `volumes`: partitions; folders and file system records belong to one, so two
///   partitions with the same folder names no longer share folders.
/// - `files.disk_offset` may be NULL: a file system entry whose location is lost.
/// - `files.name_exact`, `files.confidence` from file system analysis.
/// - Dates become ISO 8601 local date-time text: EXIF and FAT record local time with no
///   zone, so converting them to an instant would invent one.
///
/// SQLite cannot relax a NOT NULL column in place, so `files` is rebuilt (the documented
/// "create new table, copy, drop, rename" procedure; foreign keys are off during
/// migrations, see [`migrate_with`]).
const V2: &str = "
CREATE TABLE volumes (
    id         INTEGER PRIMARY KEY,
    part_index INTEGER NOT NULL UNIQUE,
    label      TEXT
) STRICT;

ALTER TABLE folders ADD COLUMN volume_id INTEGER REFERENCES volumes (id);
DROP INDEX folders_parent_name;
CREATE UNIQUE INDEX folders_parent_name
    ON folders (ifnull(volume_id, 0), ifnull(parent_id, 0), name);

CREATE TABLE files_v2 (
    id           INTEGER PRIMARY KEY,
    origin       INTEGER NOT NULL,  -- 1 = file system, 2 = carving
    kind         TEXT NOT NULL,     -- FileKind code
    status       INTEGER NOT NULL,  -- Status code
    disk_offset  INTEGER,           -- start of the first extent; NULL if unknown
    size         INTEGER NOT NULL,
    name         TEXT,
    volume_id    INTEGER REFERENCES volumes (id),
    folder_id    INTEGER REFERENCES folders (id),
    name_exact   INTEGER NOT NULL,
    fs_ref       TEXT,
    deleted      INTEGER NOT NULL,
    confidence   INTEGER,           -- Confidence code; NULL if not judged
    merged_into  INTEGER REFERENCES files (id),
    width        INTEGER,
    height       INTEGER,
    duration_ms  INTEGER,
    taken_at     TEXT,              -- ISO 8601 local date-time, no zone
    camera       TEXT,
    thumb_offset INTEGER,
    thumb_len    INTEGER,
    created_at   TEXT,
    modified_at  TEXT
) STRICT;

-- v1 stored dates as Unix seconds; render them as they were written (UTC).
INSERT INTO files_v2 (id, origin, kind, status, disk_offset, size, name, volume_id,
    folder_id, name_exact, fs_ref, deleted, confidence, merged_into, width, height,
    duration_ms, taken_at, camera, thumb_offset, thumb_len, created_at, modified_at)
SELECT id, origin, kind, status, disk_offset, size, name, NULL,
    folder_id, 1, fs_ref, deleted, NULL, merged_into, width, height,
    duration_ms,
    strftime('%Y-%m-%dT%H:%M:%S', taken_at, 'unixepoch'),
    camera, thumb_offset, thumb_len,
    strftime('%Y-%m-%dT%H:%M:%S', created_at, 'unixepoch'),
    strftime('%Y-%m-%dT%H:%M:%S', modified_at, 'unixepoch')
FROM files;

DROP TABLE files;
ALTER TABLE files_v2 RENAME TO files;

CREATE INDEX files_disk_offset ON files (disk_offset);
CREATE INDEX files_folder ON files (folder_id);
CREATE UNIQUE INDEX files_carved_identity ON files (disk_offset, kind) WHERE origin = 2;
CREATE UNIQUE INDEX files_fs_identity ON files (fs_ref) WHERE origin = 1 AND fs_ref IS NOT NULL;
";

/// Brings the schema up to the latest version.
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), Error> {
    migrate_with(conn, MIGRATIONS)
}

pub(crate) fn migrate_with(conn: &mut Connection, migrations: &[Migration]) -> Result<(), Error> {
    let supported = u32::try_from(migrations.len()).unwrap_or(u32::MAX);
    let current = user_version(conn)?;
    if current > supported {
        return Err(Error::TooNew {
            found: current,
            supported,
        });
    }
    if current == 0 {
        check_empty(conn)?;
    } else {
        let app_id: i32 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if app_id != APPLICATION_ID {
            return Err(Error::NotAnIndex);
        }
    }

    // Rebuilding a table (drop + rename) must not trigger ON DELETE CASCADE, so foreign
    // keys are off while migrating (the pragma has no effect inside a transaction) and
    // checked before each commit instead.
    conn.pragma_update(None, "foreign_keys", false)?;
    for (version, migration) in (1u32..).zip(migrations).skip(current as usize) {
        let failed = |source| Error::Migration {
            version,
            description: migration.description,
            source,
        };
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql).map_err(failed)?;
        let violation: Option<String> = tx
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()?;
        if let Some(table) = violation {
            return Err(Error::Corrupt(format!(
                "schema version {version} leaves a dangling reference in {table}"
            )));
        }
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

pub(crate) fn user_version(conn: &Connection) -> Result<u32, Error> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    u32::try_from(v).map_err(|_| Error::Corrupt(format!("negative schema version {v}")))
}

/// A database at version 0 must be empty; otherwise it is some other application's file.
fn check_empty(conn: &Connection) -> Result<(), Error> {
    let any: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match any {
        Some(_) => Err(Error::NotAnIndex),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MIGRATIONS: &[Migration] = &[
        Migration {
            description: "v1",
            sql: "CREATE TABLE t (a INTEGER);",
        },
        Migration {
            description: "v2",
            sql: "ALTER TABLE t ADD COLUMN b INTEGER NOT NULL DEFAULT 7;",
        },
    ];

    #[test]
    fn schema_version_matches_migrations() {
        assert_eq!(SCHEMA_VERSION as usize, MIGRATIONS.len());
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// A database written by schema version 1, with data, upgrades without losing any.
    #[test]
    fn v1_data_survives_the_upgrade_to_v2() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..1]).unwrap();
        conn.execute_batch(
            "INSERT INTO folders (id, parent_id, name) VALUES (1, NULL, 'DCIM');
             INSERT INTO files (id, origin, kind, status, disk_offset, size, name, folder_id,
                 fs_ref, deleted, merged_into, taken_at, modified_at)
             VALUES (1, 1, 'mov', 1, 4096, 8192, 'clip.mov', 1, 'x:1', 1, NULL,
                 1723649422, 1723649400),
                    (2, 2, 'mp4', 0, 4096, 8192, NULL, NULL, NULL, 1, 1, NULL, NULL);
             INSERT INTO extents (file_id, seq, disk_offset, len)
             VALUES (1, 0, 4096, 4096), (1, 1, 65536, 4096), (2, 0, 4096, 8192);",
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 2);
        // Rebuilding `files` must not cascade into `extents`.
        let extents: i64 = conn
            .query_row("SELECT count(*) FROM extents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(extents, 3);
        let (taken, modified, exact, merged): (String, String, bool, Option<i64>) = conn
            .query_row(
                "SELECT taken_at, modified_at, name_exact, (SELECT merged_into FROM files \
                 WHERE id = 2) FROM files WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(taken, "2024-08-14T15:30:22");
        assert_eq!(modified, "2024-08-14T15:30:00");
        assert!(exact);
        assert_eq!(merged, Some(1));
        let fk: bool = conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert!(fk, "foreign keys are back on");
        // The rebuilt table still enforces its references.
        assert!(
            conn.execute("UPDATE files SET merged_into = 99 WHERE id = 2", [])
                .is_err()
        );
    }

    #[test]
    fn upgrades_one_version_at_a_time_and_keeps_data() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_with(&mut conn, &TEST_MIGRATIONS[..1]).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 1);
        conn.execute("INSERT INTO t (a) VALUES (1)", []).unwrap();

        migrate_with(&mut conn, TEST_MIGRATIONS).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 2);
        let (a, b): (i64, i64) = conn
            .query_row("SELECT a, b FROM t", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((a, b), (1, 7));
    }

    #[test]
    fn failed_migration_leaves_previous_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_with(&mut conn, &TEST_MIGRATIONS[..1]).unwrap();
        let broken = [
            Migration {
                description: "v1",
                sql: TEST_MIGRATIONS[0].sql,
            },
            Migration {
                description: "broken",
                sql: "ALTER TABLE t ADD COLUMN c INTEGER; SELECT * FROM missing_table;",
            },
        ];
        let err = migrate_with(&mut conn, &broken).unwrap_err();
        assert!(matches!(err, Error::Migration { version: 2, .. }), "{err}");
        assert_eq!(user_version(&conn).unwrap(), 1);
        // The partial change was rolled back.
        assert!(conn.prepare("SELECT c FROM t").is_err());
    }

    #[test]
    fn refuses_newer_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_with(&mut conn, TEST_MIGRATIONS).unwrap();
        let err = migrate_with(&mut conn, &TEST_MIGRATIONS[..1]).unwrap_err();
        assert!(matches!(
            err,
            Error::TooNew {
                found: 2,
                supported: 1
            }
        ));
    }

    #[test]
    fn refuses_foreign_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE other (x);").unwrap();
        assert!(matches!(migrate(&mut conn), Err(Error::NotAnIndex)));

        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE other (x); PRAGMA user_version = 1;")
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(Error::NotAnIndex)));
    }
}
