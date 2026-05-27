//! An immutable result computed by the git worker thread and handed to the UI
//! thread via `Event::Refreshed`. Kept as its own type so it can grow (e.g.
//! per-file diff hashes in M4) without churning the event signature.

use crate::model::file::ChangedFile;

#[derive(Debug, Clone)]
pub struct GitSnapshot {
    pub files: Vec<ChangedFile>,
}
