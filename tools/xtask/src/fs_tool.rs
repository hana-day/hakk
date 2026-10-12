//! The `fs` tool of the benchmark: recovers files through file system analysis.
//!
//! Every file the `fs` crate lists (live and deleted) is written out and scored by
//! `hakk-eval` like the other tools. On top of that, deleted files are checked against the
//! manifest directly:
//!
//! - **name recovery**: a deleted entry with the same path (ASCII case-insensitive)
//! - **placement match**: that entry's extents equal the manifest's
//!
//! both broken down by the confidence the `fs` crate assigned. The result is merged into
//! the tool's report under `"fs"`.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result};
use blocksource::{BlockSource, ImageFile};
use hakk_fs::{Confidence, Entry, Extent};
use serde_json::{Value, json};

pub fn run(bin: &Path, image: &Path, manifest: &Path, dir: &Path) -> Result<Value> {
    let out = dir.join("fs");
    // Images without a file system (`fs = "none"`) yield nothing; score that as zero.
    fs::create_dir_all(&out)?;
    let started = Instant::now();
    let entries = recover(image, &out)?;
    let elapsed = started.elapsed().as_secs_f64();
    let mut report = super::evaluate(bin, manifest, &out, "fs", elapsed, dir)?;

    let manifest: Value = serde_json::from_slice(&fs::read(manifest)?)?;
    let score = score_deleted(&manifest, &entries);
    println!(
        "  fs: deleted names {}/{}, placement {}/{} (targets: names {}/{}, placement {}/{})",
        score.named,
        score.deleted,
        score.placed,
        score.deleted,
        score.target_named,
        score.targets,
        score.target_placed,
        score.targets,
    );
    for (c, s) in &score.by_confidence {
        println!(
            "      {c:<6} named {:>4}, placement correct {:>4}",
            s.named, s.placed
        );
    }
    let detail = json!({
        "deleted": score.deleted,
        "names": score.named,
        "name_rate": rate(score.named, score.deleted),
        "placements": score.placed,
        "placement_rate": rate(score.placed, score.deleted),
        "targets": score.targets,
        "target_names": score.target_named,
        "target_placements": score.target_placed,
        "by_confidence": score.by_confidence.iter().map(|(c, s)| {
            (c.clone(), json!({ "named": s.named, "placed": s.placed }))
        }).collect::<serde_json::Map<_, _>>(),
        "misses": score.misses,
    });
    fs::write(
        dir.join("fs-names.json"),
        serde_json::to_string_pretty(&detail)?,
    )?;
    if let Value::Object(m) = &mut report {
        m.insert("fs".into(), detail);
    }
    Ok(report)
}

/// Scans every volume and writes each listed file to `out/{live,deleted}/<path>`.
fn recover(image: &Path, out: &Path) -> Result<Vec<Entry>> {
    let src = ImageFile::open(image)?;
    let mut all = Vec::new();
    for (part, vol) in hakk_fs::scan_disk(&src)? {
        let vol = match vol {
            Ok(v) => v,
            Err(e) => {
                eprintln!("fs: partition at {:#x}: {e}", part.offset);
                continue;
            }
        };
        for w in &vol.warnings {
            eprintln!("fs: {w}");
        }
        for e in vol
            .entries
            .iter()
            .filter(|e| !e.is_dir && e.size > 0 && (!e.extents.is_empty() || e.resident.is_some()))
        {
            let sub = if e.deleted { "deleted" } else { "live" };
            let path = unique(out.join(sub).join(&e.path));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, read_file(&src, e)?)
                .with_context(|| format!("cannot write {}", path.display()))?;
        }
        all.extend(vol.entries);
    }
    Ok(all)
}

/// Reads an entry's data; holes (NTFS sparse runs) and bytes past its valid size read as
/// zeros (exFAT ValidDataLength, NTFS initialized size).
fn read_file(src: &ImageFile, e: &Entry) -> Result<Vec<u8>> {
    if let Some(data) = &e.resident {
        return Ok(data.clone());
    }
    let mut data = Vec::new();
    let mut holes = e.holes.iter().peekable();
    let mut fill_holes = |data: &mut Vec<u8>| -> Result<()> {
        while let Some(h) = holes.next_if(|h| h.file_offset <= data.len() as u64) {
            data.resize(data.len() + usize::try_from(h.len)?, 0);
        }
        Ok(())
    };
    for x in &e.extents {
        fill_holes(&mut data)?;
        let start = data.len();
        data.resize(start + usize::try_from(x.len)?, 0);
        let got = src.read_at(x.offset, data.get_mut(start..).unwrap_or_default())?;
        data.truncate(start + got);
    }
    fill_holes(&mut data)?;
    if let Ok(valid) = usize::try_from(e.valid_size)
        && let Some(tail) = data.get_mut(valid..)
    {
        tail.fill(0);
    }
    Ok(data)
}

/// Appends `~N` to the file name until the path is free.
fn unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    (1..)
        .map(|n| {
            let mut name = path.file_name().unwrap_or_default().to_os_string();
            name.push(format!("~{n}"));
            path.with_file_name(name)
        })
        .find(|p| !p.exists())
        .unwrap_or(path)
}

#[derive(Default)]
struct Score {
    deleted: usize,
    named: usize,
    placed: usize,
    targets: usize,
    target_named: usize,
    target_placed: usize,
    by_confidence: BTreeMap<String, ConfidenceScore>,
    /// Deleted files whose name or placement was not recovered.
    misses: Vec<Value>,
}

#[derive(Default)]
struct ConfidenceScore {
    named: usize,
    placed: usize,
}

fn score_deleted(manifest: &Value, entries: &[Entry]) -> Score {
    let mut s = Score::default();
    let files = manifest.get("files").and_then(Value::as_array);
    for f in files.into_iter().flatten() {
        let path = f.get("path").and_then(Value::as_str).unwrap_or_default();
        let deleted = f.get("deleted_at").is_some_and(|v| !v.is_null());
        let Some(truth) = manifest_extents(f) else {
            continue;
        };
        if !deleted {
            continue;
        }
        let size = f.get("size").and_then(Value::as_u64).unwrap_or(0);
        let intact = f.get("intact_bytes").and_then(Value::as_u64) == Some(size);
        s.deleted += 1;
        s.targets += usize::from(intact);
        let named: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.deleted && !e.is_dir && e.path.eq_ignore_ascii_case(path))
            .collect();
        let placed = named.iter().find(|e| merge(&e.extents) == truth);
        if let Some(e) = placed.or(named.first()) {
            s.named += 1;
            s.target_named += usize::from(intact);
            let c = s
                .by_confidence
                .entry(confidence_name(e.confidence))
                .or_default();
            c.named += 1;
            if placed.is_some() {
                s.placed += 1;
                s.target_placed += usize::from(intact);
                c.placed += 1;
            }
        }
        if placed.is_none() {
            s.misses.push(json!({
                "path": path,
                "intact": intact,
                "named": !named.is_empty(),
                "confidence": named.first().map(|e| confidence_name(e.confidence)),
            }));
        }
    }
    s
}

fn manifest_extents(f: &Value) -> Option<Vec<Extent>> {
    let mut v: Vec<(u64, Extent)> = f
        .get("extents")?
        .as_array()?
        .iter()
        .map(|x| {
            let n = |k: &str| x.get(k).and_then(Value::as_u64).unwrap_or(0);
            (
                n("file_offset"),
                Extent {
                    offset: n("image_offset"),
                    len: n("len"),
                },
            )
        })
        .collect();
    v.sort_by_key(|(o, _)| *o);
    Some(merge(&v.into_iter().map(|(_, x)| x).collect::<Vec<_>>()))
}

/// Joins physically adjacent extents (in file order).
fn merge(extents: &[Extent]) -> Vec<Extent> {
    let mut out: Vec<Extent> = Vec::new();
    for x in extents.iter().filter(|x| x.len > 0) {
        match out.last_mut() {
            Some(last) if last.end() == x.offset => last.len += x.len,
            _ => out.push(*x),
        }
    }
    out
}

fn confidence_name(c: Confidence) -> String {
    format!("{c:?}").to_lowercase()
}

fn rate(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}
