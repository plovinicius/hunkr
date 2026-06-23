//! An immutable result computed by the git worker thread and handed to the UI
//! thread via `Event::Refreshed`.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::model::file::ChangedFile;
use crate::model::review::FileHashes;

#[derive(Debug, Clone)]
pub struct GitSnapshot {
    pub files: Vec<ChangedFile>,
    /// Current diff/chunk hashes for the files the UI asked about (the reviewed
    /// set), used to flip a reviewed file (or one of its chunks) back to
    /// unreviewed when its diff changes on disk. Unreviewed files are omitted.
    pub hashes: HashMap<PathBuf, FileHashes>,
}
