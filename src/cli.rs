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
}
