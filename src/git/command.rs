//! Thin wrappers around the system `git` CLI. Git is the source of truth for
//! the MVP (no git2/libgit2), so all repo data flows through these helpers.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Run `git <args>` in `repo_root`, returning stdout. Errors on any non-zero
/// exit. Use for plumbing where success is expected (status, rev-parse).
pub fn capture(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Run a `git diff` variant, tolerating exit code 1 (which `git diff
/// --no-index` / `--exit-code` uses to mean "differences found"). Only treats
/// codes ≥ 2 as real failures.
pub fn capture_diff(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn `git {}`", args.join(" ")))?;
    match out.status.code() {
        Some(0) | Some(1) => Ok(out.stdout),
        _ => bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// Whether a `git` command in `repo_root` exits successfully (no stdout needed).
pub fn succeeds(repo_root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
