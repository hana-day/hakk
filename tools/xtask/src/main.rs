//! Development tasks.
//!
//! `cargo xtask bench [scenario.toml ...]`
//!   Generates images from scenarios, recovers them with each tool, and scores the results.
//!   Without arguments, every `scenarios/*.toml` is used.
//!   Results go to `work/bench/<scenario>/`, with a summary in `work/bench/summary.json`.
//!
//! Recovery tools:
//! - `naive`: our minimal baseline carver (`carver::naive`)
//! - `hakk`: the structure-aware carver (`carver::scan`)
//! - `fs`: file system analysis (`crates/fs`; see `fs_tool`)
//! - `photorec`: run as an external command if `photorec` is on PATH

// A development tool handling small values; truncation lints are noise here.
#![allow(clippy::cast_possible_truncation)]

mod fs_tool;

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
        _ => bail!("usage: cargo xtask bench [scenario.toml ...]"),
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
    // Device profiles live next to the scenarios but are not scenarios themselves.
    let scenarios: Vec<PathBuf> = scenarios
        .into_iter()
        .filter(|p| p.file_name().is_none_or(|n| n != "profiles.toml"))
        .collect();

    run(Command::new("cargo").args([
        "build",
        "-q",
        "--release",
        "-p",
        "hakk-scenario",
        "-p",
        "hakk-eval",
    ]))?;
    // Where `cargo build` put them (`CARGO_TARGET_DIR` moves the target directory).
    let bin = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), |d| root.join(d))
        .join("release");
    let photorec = which(&exe("photorec"));
    if photorec.is_none() {
        eprintln!("photorec not found, skipping it (brew install testdisk / apt install testdisk)");
    }

    let mut reports = Vec::new();
    for sc in &scenarios {
        let name = sc
            .file_stem()
            .context("invalid scenario file name")?
            .to_string_lossy()
            .into_owned();
        if cfg!(target_os = "macos") && scenario_fs(sc)?.as_deref() == Some("ntfs") {
            eprintln!("== {name}: skipped (macOS cannot write NTFS)");
            continue;
        }
        let dir = Path::new(WORK).join(&name);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        eprintln!("== {name}: generating image");
        run(Command::new(bin.join(exe("hakk-scenario")))
            .arg(sc)
            .arg(&dir))?;
        let image = dir.join("image.img");
        let manifest = dir.join("manifest.json");

        // Our minimal baseline carver.
        let out = dir.join("naive");
        let started = Instant::now();
        naive(&image, &out)?;
        let elapsed = started.elapsed().as_secs_f64();
        reports.push(evaluate(&bin, &manifest, &out, "naive", elapsed, &dir)?);

        // The structure-aware carver.
        let out = dir.join("hakk");
        let started = Instant::now();
        hakk(&image, &out)?;
        let elapsed = started.elapsed().as_secs_f64();
        reports.push(evaluate(&bin, &manifest, &out, "hakk", elapsed, &dir)?);

        reports.push(fs_tool::run(&bin, &image, &manifest, &dir)?);

        if let Some(photorec) = &photorec {
            let out = dir.join("photorec");
            // PhotoRec silently writes nothing if the output's parent directory is missing.
            fs::create_dir_all(&out)?;
            let started = Instant::now();
            // Output dirs get a numeric suffix (recup.1). The log lands in the cwd; move it.
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
            // The run report written next to the output is not a recovered file; move it away.
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

/// The `fs = "..."` value of a scenario file.
fn scenario_fs(path: &Path) -> Result<Option<String>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(text.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("fs")?
            .trim_start()
            .strip_prefix('=')?;
        Some(value.trim().trim_matches('"').to_owned())
    }))
}

/// Recovers with the minimal carver, writing files to `out`.
/// Recovers with the structure-aware carver, writing complete files and the readable part
/// of broken ones to `out`.
fn hakk(image: &Path, out: &Path) -> Result<()> {
    fs::create_dir_all(out)?;
    let src = ImageFile::open(image)?;
    for f in carver::scan::scan(&src, &carver::scan::Options::default()) {
        if f.len == 0 {
            continue;
        }
        let tag = match f.status {
            carver::Status::Complete => "",
            carver::Status::Corrupt { .. } => "-broken",
            carver::Status::Truncated => "-truncated",
        };
        fs::write(
            out.join(format!("f{:010}{tag}.{}", f.offset, f.kind.extension())),
            f.read(&src),
        )?;
    }
    Ok(())
}

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
    let table = run(Command::new(bin.join(exe("hakk-eval")))
        .arg(manifest)
        .arg(recovered)
        .args(["--tool", tool, "--elapsed", &elapsed.to_string(), "--json"])
        .arg(&json))?;
    print!("{table}");
    Ok(serde_json::from_slice(&fs::read(&json)?)?)
}

fn print_summary(reports: &[serde_json::Value]) {
    println!();
    println!(
        "{:<14} {:<10} {:>10} {:>10} {:>8} {:>8}",
        "scenario", "tool", "complete", "fragment", "false+", "MB/s"
    );
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
        println!(
            "{:<14} {:<10} {:>10} {:>10} {:>8} {:>8}",
            s("scenario"),
            s("tool"),
            format!("{}/{}", n("complete"), n("targets")),
            format!("{}/{}", n("fragmented_complete"), n("fragmented_targets")),
            n("false_positives_or_unknown"),
            speed,
        );
    }
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
        .context("cannot find the workspace root")
}

/// Appends the platform's executable suffix (`.exe` on Windows).
fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
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
        .with_context(|| format!("cannot start {cmd:?}"))?;
    if !out.status.success() {
        bail!(
            "{cmd:?} failed ({}): {}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
