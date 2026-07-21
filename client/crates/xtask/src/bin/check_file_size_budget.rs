//! Enforces a maximum line-count budget on Rust source files under `client/`.
//!
//! Why: nothing currently stops a single file from growing without bound.
//! `client/crates/daemon/src/discovery.rs` reached ~3.2k lines before being
//! split (see task-49) purely because no check ever flagged it. This is the
//! "at minimum" floor called out in task-50: a cheap, dependency-free
//! line-count budget that catches the same problem early, before a file
//! becomes painful to split.
//!
//! Threshold rationale (see docs/developers/complexity-budgets.md for the
//! full writeup): the budget was 2500 lines (set after the discovery.rs
//! split) but was tightened to 1000 lines — 2500 let several files grow past
//! 2k before anyone noticed. 1000 lines is a real signal that a file is
//! doing too much, well before it becomes painful to split.
//!
//! Usage:
//!   cargo run -p portzero-xtask --bin check-file-size-budget            # all client/*.rs files
//!   cargo run -p portzero-xtask --bin check-file-size-budget -- --changed  # staged/changed only
//!
//! Exit status: 0 if every file is within budget, 1 otherwise (with a report
//! of the offending files).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let max_lines: usize = std::env::var("PORTZERO_FILE_SIZE_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);

    let repo_root = repo_root()?;
    let changed = std::env::args().nth(1).as_deref() == Some("--changed");

    let files = if changed {
        let files = changed_client_rs_files(&repo_root)?;
        if files.is_empty() {
            println!(
                "check-file-size-budget: no changed client/*.rs files staged, nothing to check."
            );
            return Ok(());
        }
        files
    } else {
        all_client_rs_files(&repo_root)?
    };

    let mut failed = false;
    for file in &files {
        let path = repo_root.join(file);
        if !path.is_file() {
            continue;
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let lines = contents.lines().count();
        if lines > max_lines {
            println!("FAIL: {} has {lines} lines (budget: {max_lines})", file.display());
            failed = true;
        }
    }

    if failed {
        println!();
        println!("One or more files under client/ exceed the {max_lines}-line budget.");
        println!(
            "Consider splitting the file into smaller modules (see docs/developers/complexity-budgets.md)."
        );
        std::process::exit(1);
    }

    println!("check-file-size-budget: all client/*.rs files within {max_lines}-line budget.");
    Ok(())
}

fn repo_root() -> Result<PathBuf> {
    // CARGO_MANIFEST_DIR is client/crates/xtask; the repo root is three
    // levels up, independent of the caller's current directory.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .context("resolving repo root from CARGO_MANIFEST_DIR")
}

fn all_client_rs_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rs_files(&repo_root.join("client"), repo_root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_rs_files(dir: &Path, repo_root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, repo_root, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path.strip_prefix(repo_root)?.to_path_buf());
        }
    }
    Ok(())
}

fn changed_client_rs_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args([
            "diff",
            "--cached",
            "--name-only",
            "--diff-filter=ACMR",
            "--",
            "client/*.rs",
        ])
        .current_dir(repo_root)
        .output();

    // Falls back to an empty set (nothing to check) if there is no git repo
    // (e.g. running from a tarball) — the file-size check then no-ops.
    let output = match output {
        Ok(output) if output.status.success() => output,
        _ => return Ok(Vec::new()),
    };

    let files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    Ok(files)
}
