//! 開発用のタスク。
//!
//! `cargo xtask bench [シナリオ.toml ...]`
//!   シナリオからイメージを生成し、各復元ツールで復元して採点する。
//!   シナリオを省略すると `scenarios/*.toml` をすべて使う。
//!   結果は `work/bench/<シナリオ>/` に置き、まとめを `work/bench/summary.json` に書く。
//!
//! 復元ツール：
//! - `naive`：比較の基準にする最小限の自作カーバー（`carver::naive`）
//! - `photorec`：PATH に `photorec` があれば外部コマンドとして実行する

// 開発用ツールで、扱う値は小さいので桁落ちの lint は外す
#![allow(clippy::cast_possible_truncation)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use blocksource::{BlockSource, ImageFile};

const WORK: &str = "work/bench";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        Some((cmd, rest)) if cmd == "bench" => bench(rest),
        _ => bail!("使い方: cargo xtask bench [シナリオ.toml ...]"),
    }
}

fn bench(args: &[String]) -> Result<()> {
    let root = workspace_root()?;
    std::env::set_current_dir(&root)?;

    let scenarios: Vec<PathBuf> = if args.is_empty() {
        let mut v: Vec<PathBuf> = fs::read_dir("scenarios")?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        v.sort();
        v
    } else {
        args.iter().map(PathBuf::from).collect()
    };

    run(Command::new("cargo").args([
        "build",
        "-q",
        "--release",
        "-p",
        "hakk-scenario",
        "-p",
        "hakk-eval",
    ]))?;
    let bin = root.join("target/release");
    let photorec = which("photorec");
    if photorec.is_none() {
        eprintln!(
            "photorec が見つからないので省略する（brew install testdisk / apt install testdisk）"
        );
    }

    let mut reports = Vec::new();
    for sc in &scenarios {
        let name = sc
            .file_stem()
            .context("シナリオのファイル名が不正")?
            .to_string_lossy()
            .into_owned();
        let dir = Path::new(WORK).join(&name);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        eprintln!("== {name}: イメージを生成");
        run(Command::new(bin.join("hakk-scenario")).arg(sc).arg(&dir))?;
        let image = dir.join("image.img");
        let manifest = dir.join("manifest.json");

        // 自作の最小限のカーバー
        let out = dir.join("naive");
        let started = Instant::now();
        naive(&image, &out)?;
        let elapsed = started.elapsed().as_secs_f64();
        reports.push(evaluate(&bin, &manifest, &out, "naive", elapsed, &dir)?);

        if let Some(photorec) = &photorec {
            let out = dir.join("photorec");
            // 出力先の親ディレクトリが無いと、PhotoRec は何も書き出さずに終わる
            fs::create_dir_all(&out)?;
            let started = Instant::now();
            // 出力先は recup_dir.1 のように番号が付く。ログはカレントに出るので出力先に移す
            run(Command::new(photorec)
                .arg("/log")
                .arg("/d")
                .arg(out.join("recup"))
                .arg("/cmd")
                .arg(&image)
                .arg("partition_none,fileopt,everything,disable,jpg,enable,mov,enable,wholespace,search"))?;
            let elapsed = started.elapsed().as_secs_f64();
            if Path::new("photorec.log").exists() {
                fs::rename("photorec.log", dir.join("photorec.log"))?;
            }
            // 出力先に書かれる実行記録は復元ファイルではないので外す
            for e in fs::read_dir(&out)? {
                let report = e?.path().join("report.xml");
                if report.exists() {
                    fs::rename(&report, dir.join("photorec-report.xml"))?;
                }
            }
            reports.push(evaluate(&bin, &manifest, &out, "photorec", elapsed, &dir)?);
        }
    }

    print_summary(&reports);
    fs::write(
        Path::new(WORK).join("summary.json"),
        serde_json::to_string_pretty(&reports)?,
    )?;
    Ok(())
}

/// 最小限のカーバーで復元し、`out` に書き出す。
fn naive(image: &Path, out: &Path) -> Result<()> {
    fs::create_dir_all(out)?;
    let src = ImageFile::open(image)?;
    for f in carver::naive::scan(&src, 64 << 20) {
        let mut buf = vec![0u8; usize::try_from(f.len)?];
        src.read_exact_at(f.offset, &mut buf)?;
        fs::write(
            out.join(format!("f{:010}.{}", f.offset, f.kind.extension())),
            buf,
        )?;
    }
    Ok(())
}

fn evaluate(
    bin: &Path,
    manifest: &Path,
    recovered: &Path,
    tool: &str,
    elapsed: f64,
    dir: &Path,
) -> Result<serde_json::Value> {
    let json = dir.join(format!("{tool}.json"));
    let table = run(Command::new(bin.join("hakk-eval"))
        .arg(manifest)
        .arg(recovered)
        .args(["--tool", tool, "--elapsed", &elapsed.to_string(), "--json"])
        .arg(&json))?;
    print!("{table}");
    Ok(serde_json::from_slice(&fs::read(&json)?)?)
}

fn print_summary(reports: &[serde_json::Value]) {
    println!();
    let header = [
        "シナリオ",
        "ツール",
        "完全復元",
        "断片化",
        "誤検出",
        "MB/秒",
    ];
    let widths: [usize; 6] = [14, 10, 10, 8, 8, 8];
    let row = |cells: &[String]| {
        let mut line = String::new();
        for (i, (c, w)) in cells.iter().zip(widths).enumerate() {
            // 先頭2列は左寄せ、数値は右寄せ。全角は2桁として数える
            let pad = " ".repeat(w.saturating_sub(display_width(c)));
            if i < 2 {
                line.push_str(&format!("{c}{pad} "));
            } else {
                line.push_str(&format!("{pad}{c} "));
            }
        }
        println!("{}", line.trim_end());
    };
    row(&header.map(str::to_owned));
    for r in reports {
        let n = |k: &str| r.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
        let s = |k: &str| {
            r.get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let speed = r
            .get("mb_per_sec")
            .and_then(serde_json::Value::as_f64)
            .map_or("-".to_owned(), |v| format!("{v:.0}"));
        row(&[
            s("scenario"),
            s("tool"),
            format!("{}/{}", n("complete"), n("targets")),
            format!("{}/{}", n("fragmented_complete"), n("fragmented_targets")),
            n("false_positives_or_unknown").to_string(),
            speed,
        ]);
    }
}

/// 端末での表示幅（全角を2とするおおよその値）。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if u32::from(c) >= 0x1100 { 2 } else { 1 })
        .sum()
}

fn workspace_root() -> Result<PathBuf> {
    let out = run(Command::new("cargo").args([
        "locate-project",
        "--workspace",
        "--message-format",
        "plain",
    ]))?;
    Path::new(out.trim())
        .parent()
        .map(Path::to_path_buf)
        .context("ワークスペースのルートが分からない")
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(if cfg!(windows) { ';' } else { ':' })
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("{cmd:?} を起動できない"))?;
    if !out.status.success() {
        bail!(
            "{cmd:?} が失敗した（{}）: {}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
