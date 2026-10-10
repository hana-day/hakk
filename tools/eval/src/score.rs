//! 採点。
//!
//! # 用語
//!
//! - **対象**：削除済みで、中身が上書きされずに完全に残っているファイル。復元率の分母
//! - **完全復元**：SHA-256 が一致する復元ファイルがある
//! - **部分復元**：完全一致はないが、512 バイトのブロックが同じ位置で一致する復元ファイルがある。
//!   対象のブロックのうち最も多く一致した割合を「ブロック一致率」とする
//! - **上書き済み**：削除済みで一部が上書きされたファイル。分母には入れず、件数だけ示す
//! - **誤検出・不明**：どのファイルとも一致しない復元ファイル。上書き済みファイルの断片を
//!   復元したものもここに入る（上書き前の中身はマニフェストからは分からないため）

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BLOCK: usize = 512;
const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    version: u32,
    scenario: String,
    seed: u64,
    image: String,
    image_size: u64,
    files: Vec<FileEntry>,
}

#[derive(Debug, Deserialize)]
struct FileEntry {
    id: u32,
    path: String,
    kind: String,
    size: u64,
    sha256: String,
    deleted_at: Option<usize>,
    extents: Option<Vec<Extent>>,
    fragmented: bool,
    intact_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct Extent {
    file_offset: u64,
    image_offset: u64,
    len: u64,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("{} を読めない", path.display()))?;
        let m: Self = serde_json::from_str(&text)
            .with_context(|| format!("{} の形式が不正", path.display()))?;
        if m.version != MANIFEST_VERSION {
            bail!(
                "マニフェストの版 {} には対応していない（{MANIFEST_VERSION} のみ）",
                m.version
            );
        }
        Ok(m)
    }
}

impl FileEntry {
    fn is_target(&self) -> bool {
        self.deleted_at.is_some() && self.extents.is_some() && self.intact_bytes == self.size
    }

    fn is_damaged(&self) -> bool {
        self.deleted_at.is_some() && !self.is_target()
    }

    fn intact(&self) -> bool {
        self.extents.is_some() && self.intact_bytes == self.size
    }

    fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    tool: String,
    scenario: String,
    seed: u64,
    /// 復元率の分母（削除済みで中身が完全に残っているファイル）。
    targets: usize,
    complete: usize,
    complete_rate: f64,
    partial: usize,
    /// 対象ごとのブロック一致率の平均（完全復元は 1.0、見つからなければ 0.0）。
    mean_block_ratio: f64,
    /// 完全復元のうち、元のファイル名で復元できた件数。
    names: usize,
    fragmented_targets: usize,
    fragmented_complete: usize,
    by_kind: BTreeMap<String, KindScore>,
    /// 削除済みで一部が上書きされたファイル（分母に入れない）。
    damaged: usize,
    /// 残っているファイルのうち完全復元されたもの（カービングは残っているファイルも見つける）。
    live_complete: usize,
    recovered_files: usize,
    /// 同じファイルを2回以上完全復元した余分な件数。
    duplicates: usize,
    false_positives_or_unknown: usize,
    elapsed_secs: Option<f64>,
    mb_per_sec: Option<f64>,
    details: Vec<Detail>,
}

#[derive(Debug, Default, Serialize)]
struct KindScore {
    targets: usize,
    complete: usize,
}

#[derive(Debug, Serialize)]
struct Detail {
    id: u32,
    path: String,
    result: Outcome,
    block_ratio: f64,
    /// 完全復元したファイル（復元結果のディレクトリからの相対パス）。
    #[serde(skip_serializing_if = "Option::is_none")]
    recovered_as: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Complete,
    Partial,
    Missing,
}

impl Report {
    pub fn compute(
        m: &Manifest,
        manifest_path: &Path,
        recovered_dir: &Path,
        tool: &str,
        elapsed: Option<f64>,
    ) -> Result<Self> {
        let image_path = manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(&m.image);
        let image = fs::read(&image_path)
            .with_context(|| format!("{} を読めない", image_path.display()))?;

        // 中身が完全に残っているファイルの中身を、イメージから組み立てる
        let contents: Vec<Option<Vec<u8>>> = m
            .files
            .iter()
            .map(|f| f.intact().then(|| assemble(&image, f)).flatten())
            .collect();

        // ブロック → (ファイル番号, ブロック番号)
        let mut index: HashMap<&[u8], Vec<(usize, usize)>> = HashMap::new();
        for (fi, c) in contents.iter().enumerate() {
            let Some(c) = c else { continue };
            for (k, b) in c.chunks_exact(BLOCK).enumerate() {
                if b.iter().any(|&x| x != 0) {
                    index.entry(b).or_default().push((fi, k));
                }
            }
        }
        let by_hash: HashMap<&str, usize> = m
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.sha256.as_str(), i))
            .collect();

        let mut complete_as: Vec<Option<String>> = vec![None; m.files.len()];
        let mut best_ratio = vec![0.0f64; m.files.len()];
        let mut duplicates = 0;
        let mut false_positives = 0;
        let recovered = walk(recovered_dir)?;
        for path in &recovered {
            let data = fs::read(path).with_context(|| format!("{} を読めない", path.display()))?;
            let rel = path
                .strip_prefix(recovered_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            let sha = hex(&Sha256::digest(&data));
            if let Some(&fi) = by_hash.get(sha.as_str()) {
                match complete_as.get_mut(fi) {
                    Some(slot @ None) => *slot = Some(rel),
                    Some(Some(_)) => duplicates += 1,
                    None => {}
                }
                if let Some(r) = best_ratio.get_mut(fi) {
                    *r = 1.0;
                }
                continue;
            }

            // 同じ位置で一致するブロックを数える
            let mut hits: HashMap<usize, usize> = HashMap::new();
            for (k, b) in data.chunks_exact(BLOCK).enumerate() {
                for &(fi, bk) in index.get(b).into_iter().flatten() {
                    if bk == k {
                        *hits.entry(fi).or_default() += 1;
                    }
                }
            }
            if hits.is_empty() {
                false_positives += 1;
            }
            for (fi, n) in hits {
                let total = m
                    .files
                    .get(fi)
                    .map_or(1, |f| (f.size as usize / BLOCK).max(1));
                if let Some(r) = best_ratio.get_mut(fi) {
                    *r = r.max(n as f64 / total as f64);
                }
            }
        }

        let mut details = Vec::new();
        let mut by_kind: BTreeMap<String, KindScore> = BTreeMap::new();
        let (mut complete, mut partial, mut names) = (0, 0, 0);
        let (mut frag_targets, mut frag_complete) = (0, 0);
        let mut ratio_sum = 0.0;
        for (fi, f) in m.files.iter().enumerate() {
            if !f.is_target() {
                continue;
            }
            let as_ = complete_as.get(fi).cloned().flatten();
            let ratio = best_ratio.get(fi).copied().unwrap_or(0.0);
            let result = match (&as_, ratio) {
                (Some(_), _) => Outcome::Complete,
                (None, r) if r > 0.0 => Outcome::Partial,
                _ => Outcome::Missing,
            };
            let ks = by_kind.entry(f.kind.clone()).or_default();
            ks.targets += 1;
            if f.fragmented {
                frag_targets += 1;
            }
            match result {
                Outcome::Complete => {
                    complete += 1;
                    ks.complete += 1;
                    if f.fragmented {
                        frag_complete += 1;
                    }
                    let got = as_.as_deref().and_then(|p| Path::new(p).file_name());
                    if got.is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(f.file_name()))
                    {
                        names += 1;
                    }
                }
                Outcome::Partial => partial += 1,
                Outcome::Missing => {}
            }
            ratio_sum += ratio;
            details.push(Detail {
                id: f.id,
                path: f.path.clone(),
                result,
                block_ratio: ratio,
                recovered_as: as_,
            });
        }

        let targets = details.len();
        let live_complete = m
            .files
            .iter()
            .zip(&complete_as)
            .filter(|(f, c)| f.deleted_at.is_none() && c.is_some())
            .count();
        let rate = |n: usize| {
            if targets == 0 {
                0.0
            } else {
                n as f64 / targets as f64
            }
        };
        Ok(Self {
            tool: tool.to_owned(),
            scenario: m.scenario.clone(),
            seed: m.seed,
            targets,
            complete,
            complete_rate: rate(complete),
            partial,
            mean_block_ratio: if targets == 0 {
                0.0
            } else {
                ratio_sum / targets as f64
            },
            names,
            fragmented_targets: frag_targets,
            fragmented_complete: frag_complete,
            by_kind,
            damaged: m.files.iter().filter(|f| f.is_damaged()).count(),
            live_complete,
            recovered_files: recovered.len(),
            duplicates,
            false_positives_or_unknown: false_positives,
            elapsed_secs: elapsed,
            mb_per_sec: elapsed
                .filter(|&s| s > 0.0)
                .map(|s| m.image_size as f64 / 1e6 / s),
            details,
        })
    }

    pub fn table(&self) -> String {
        let pct = |n: usize, d: usize| {
            if d == 0 {
                "-".to_owned()
            } else {
                format!("{:.1}%", n as f64 * 100.0 / d as f64)
            }
        };
        let mut s = String::new();
        let _ = writeln!(
            s,
            "[{}] シナリオ {}（seed {}）",
            self.tool, self.scenario, self.seed
        );
        let _ = writeln!(
            s,
            "  完全復元       {:>4} / {:<4} {:>7}",
            self.complete,
            self.targets,
            pct(self.complete, self.targets)
        );
        for (kind, k) in &self.by_kind {
            let _ = writeln!(
                s,
                "    {kind:<12} {:>4} / {:<4} {:>7}",
                k.complete,
                k.targets,
                pct(k.complete, k.targets)
            );
        }
        let _ = writeln!(
            s,
            "    断片化       {:>4} / {:<4} {:>7}",
            self.fragmented_complete,
            self.fragmented_targets,
            pct(self.fragmented_complete, self.fragmented_targets)
        );
        let _ = writeln!(s, "  部分復元       {:>4}", self.partial);
        let _ = writeln!(
            s,
            "  ブロック一致率 {:>11.1}%",
            self.mean_block_ratio * 100.0
        );
        let _ = writeln!(
            s,
            "  ファイル名     {:>4} / {:<4}",
            self.names, self.complete
        );
        let _ = writeln!(
            s,
            "  復元ファイル   {:>4}（重複 {}、誤検出・不明 {}、残っているファイル {}）",
            self.recovered_files,
            self.duplicates,
            self.false_positives_or_unknown,
            self.live_complete
        );
        let _ = writeln!(s, "  上書き済み     {:>4}（分母に含めない）", self.damaged);
        if let (Some(t), Some(r)) = (self.elapsed_secs, self.mb_per_sec) {
            let _ = writeln!(s, "  速度           {r:>8.1} MB/秒（{t:.2} 秒）");
        }
        s
    }
}

/// 中身が完全に残っているファイルを、配置に従ってイメージから組み立てる。
fn assemble(image: &[u8], f: &FileEntry) -> Option<Vec<u8>> {
    let mut out = vec![0u8; usize::try_from(f.size).ok()?];
    for e in f.extents.as_ref()? {
        let (fo, io, len) = (
            usize::try_from(e.file_offset).ok()?,
            usize::try_from(e.image_offset).ok()?,
            usize::try_from(e.len).ok()?,
        );
        out.get_mut(fo..fo.checked_add(len)?)?
            .copy_from_slice(image.get(io..io.checked_add(len)?)?);
    }
    (hex(&Sha256::digest(&out)) == f.sha256).then_some(out)
}

/// ディレクトリ以下の通常ファイルを、パス順に列挙する。
fn walk(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).with_context(|| format!("{} を読めない", d.display()))? {
            let e = e?;
            let ty = e.file_type()?;
            if ty.is_dir() {
                stack.push(e.path());
            } else if ty.is_file() {
                out.push(e.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
