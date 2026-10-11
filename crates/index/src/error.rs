use std::{error, fmt};

use crate::model::UnknownKind;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Sqlite(rusqlite::Error),
    /// The file was written by a newer version of the app.
    TooNew {
        found: u32,
        supported: u32,
    },
    /// The file is some other SQLite database, not a hakk index.
    NotAnIndex,
    /// A schema upgrade failed; the file stays at the previous version.
    Migration {
        version: u32,
        description: &'static str,
        source: rusqlite::Error,
    },
    /// The file holds values this version cannot interpret.
    Corrupt(String),
    /// A value too large to store (SQLite integers are signed 64-bit).
    OutOfRange(u64),
    InvalidFile(&'static str),
    InvalidMerge(&'static str),
    NoSuchFile(i64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "index database error: {e}"),
            Error::TooNew { found, supported } => write!(
                f,
                "index schema version {found} is newer than this app supports ({supported})"
            ),
            Error::NotAnIndex => f.write_str("not a hakk index file"),
            Error::Migration {
                version,
                description,
                source,
            } => write!(
                f,
                "failed to upgrade index to schema version {version} ({description}): {source}"
            ),
            Error::Corrupt(what) => write!(f, "corrupt index: {what}"),
            Error::OutOfRange(x) => write!(f, "value out of range for the index: {x}"),
            Error::InvalidFile(why) => write!(f, "invalid file record: {why}"),
            Error::InvalidMerge(why) => write!(f, "invalid merge: {why}"),
            Error::NoSuchFile(id) => write!(f, "no file with id {id}"),
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::Sqlite(e) | Error::Migration { source: e, .. } => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<UnknownKind> for Error {
    fn from(e: UnknownKind) -> Self {
        Error::Corrupt(e.to_string())
    }
}
