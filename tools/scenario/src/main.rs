//! シナリオ生成器：シードから削除ファイル入りのディスクイメージと正解マニフェストを作る。
//!
//! 使い方：`hakk-scenario <シナリオ.toml> <出力先ディレクトリ>`
//! 出力先に `image.img`（イメージ）と `manifest.json`（正解マニフェスト）を作る。

// 開発用ツールで、扱う値（イメージの大きさなど）は小さいので桁落ちの lint は外す
#![allow(clippy::cast_possible_truncation)]

mod backend;
mod content;
mod locate;
mod manifest;
mod rng;
mod runner;
mod scenario;
mod size;

use std::{fs, path::PathBuf, time::Instant};

use anyhow::{Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [scenario, out] = args.as_slice() else {
        bail!("使い方: hakk-scenario <シナリオ.toml> <出力先ディレクトリ>");
    };
    let sc = scenario::Scenario::load(scenario.as_ref())?;
    let out = PathBuf::from(out);

    let started = Instant::now();
    let manifest = runner::run(&sc, &out)?;
    fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    let files = &manifest.files;
    let deleted: Vec<_> = files.iter().filter(|f| f.deleted_at.is_some()).collect();
    let recoverable = deleted.iter().filter(|f| f.intact_bytes == f.size).count();
    let unlocated = files.iter().filter(|f| f.extents.is_none()).count();
    let fragmented = files.iter().filter(|f| f.fragmented).count();
    println!(
        "{}: ファイル {} 件（削除 {} 件、うち中身が完全に残っている {} 件）、断片化 {} 件、配置不明 {} 件、{:.1} 秒",
        manifest.scenario,
        files.len(),
        deleted.len(),
        recoverable,
        fragmented,
        unlocated,
        started.elapsed().as_secs_f64(),
    );
    println!("→ {}", out.display());
    Ok(())
}
