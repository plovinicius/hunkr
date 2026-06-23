//! Command-line arguments.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "hunkr",
    version,
    about = "A terminal git diff reviewer for AI coding workflows"
)]
pub struct Args {
    /// Path inside the git repository to review (defaults to the current directory).
    pub path: Option<PathBuf>,

    /// Force the ASCII-safe glyph set ([x] [ ] …) instead of the default
    /// Unicode icons (✓ ● …) — useful on terminals/fonts that mis-render them.
    #[arg(long)]
    pub ascii: bool,

    /// Path to the config file (defaults to ~/.config/hunkr/config.toml, or
    /// $XDG_CONFIG_HOME/hunkr/config.toml when that's set).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}
