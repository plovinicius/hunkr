//! Repository discovery.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Resolve the working-tree root containing `start` (worktree-aware).
pub fn discover(start: &Path) -> Result<PathBuf> {
    let out = super::command::capture(start, &["rev-parse", "--show-toplevel"])
        .with_context(|| format!("`{}` is not inside a git repository", start.display()))?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    if s.is_empty() {
        bail!(
            "could not determine git repository root for {}",
            start.display()
        );
    }
    Ok(PathBuf::from(s))
}

/// The absolute git directory for `repo_root`. Uses `--absolute-git-dir` so it
/// resolves correctly for linked worktrees (where `.git` is a file pointing
/// elsewhere), giving a stable home for hunkr's per-repo state.
pub fn git_dir(repo_root: &Path) -> Result<PathBuf> {
    let out = super::command::capture(repo_root, &["rev-parse", "--absolute-git-dir"])
        .context("could not resolve the git directory")?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    Ok(PathBuf::from(s))
}
