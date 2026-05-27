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
