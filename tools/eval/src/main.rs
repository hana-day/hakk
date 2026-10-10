//! 評価ツール：正解マニフェストと復元結果を照合して復元率を出す。
//!
//! 使い方：`hakk-eval <manifest.json> <復元結果のディレクトリ> [--tool 名前] [--elapsed 秒] [--json 出力先]`
//!
//! 復元ツールを問わない。PhotoRec の出力も hakk の出力も同じ方法で採点する。

// 開発用ツールで、扱う値は小さいので桁落ちの lint は外す
#![allow(clippy::cast_possible_truncation)]

mod score;

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};

use crate::score::{Manifest, Report};

struct Args {
    manifest: PathBuf,
    recovered: PathBuf,
    tool: String,
    elapsed: Option<f64>,
    json: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let mut pos = Vec::new();
    let mut tool = "unknown".to_owned();
    let mut elapsed = None;
    let mut json = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tool" => tool = it.next().context("--tool の値がない")?,
            "--elapsed" => {
                elapsed = Some(it.next().context("--elapsed の値がない")?.parse()?);
            }
            "--json" => json = Some(it.next().context("--json の値がない")?.into()),
            _ if a.starts_with("--") => bail!("不明なオプション: {a}"),
            _ => pos.push(PathBuf::from(a)),
        }
    }
    let [manifest, recovered] = <[PathBuf; 2]>::try_from(pos).map_err(|_| {
        anyhow::anyhow!(
            "使い方: hakk-eval <manifest.json> <復元結果のディレクトリ> [--tool 名前] [--elapsed 秒] [--json 出力先]"
        )
    })?;
    Ok(Args {
        manifest,
        recovered,
        tool,
        elapsed,
        json,
    })
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let manifest = Manifest::load(&args.manifest)?;
    let report = Report::compute(
        &manifest,
        &args.manifest,
        &args.recovered,
        &args.tool,
        args.elapsed,
    )?;
    print!("{}", report.table());
    if let Some(path) = &args.json {
        fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("{} に書けない", path.display()))?;
    }
    Ok(())
}
