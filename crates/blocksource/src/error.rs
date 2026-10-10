use std::{error::Error, fmt, io, ops::Range};

/// 読み取りの失敗。読めなかった範囲を持つ。
///
/// 上位の層はこの範囲を不良セクタとして記録し、先へ進む（最後にまとめてリトライする）。
#[derive(Debug)]
pub struct ReadError {
    range: Range<u64>,
    kind: ReadErrorKind,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ReadErrorKind {
    /// OSが返した入出力エラー（不良セクタなど）。
    Io(io::Error),
    /// 応答がなかった。
    Timeout,
    /// セクタ境界に揃っていない読み込みを、デバイスが受け付けなかった。
    Misaligned,
    /// 終端に達して、要求した長さを読めなかった。
    UnexpectedEof,
}

impl ReadError {
    pub fn new(range: Range<u64>, kind: ReadErrorKind) -> Self {
        Self { range, kind }
    }

    /// 読めなかった範囲（バイト）。
    pub fn range(&self) -> Range<u64> {
        self.range.clone()
    }

    pub fn kind(&self) -> &ReadErrorKind {
        &self.kind
    }

    pub fn into_kind(self) -> ReadErrorKind {
        self.kind
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Range { start, end } = self.range;
        write!(f, "{start:#x}..{end:#x} の読み取りに失敗: ")?;
        match &self.kind {
            ReadErrorKind::Io(e) => write!(f, "{e}"),
            ReadErrorKind::Timeout => f.write_str("タイムアウト"),
            ReadErrorKind::Misaligned => f.write_str("セクタ境界に揃っていない"),
            ReadErrorKind::UnexpectedEof => f.write_str("終端に達した"),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            ReadErrorKind::Io(e) => Some(e),
            _ => None,
        }
    }
}
