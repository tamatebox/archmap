//! One scanned workspace per root, kept between calls and scanned again
//! when a stamp of the root says its files changed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use archmap_app::{ScanMode, Stamp, Workspace};

/// Roots kept at once; the least recently used goes first.
const MAX_ROOTS: usize = 8;

#[derive(Default)]
pub(crate) struct Cache {
    entries: Vec<Entry>,
    /// Ticks once per lookup, to find the least recently used entry.
    clock: u64,
    scans: usize,
}

struct Entry {
    root: PathBuf,
    /// Taken before the scan that made `workspace`, so an edit made during
    /// the scan shows up as a change on the next call.
    stamp: Stamp,
    workspace: Arc<Workspace>,
    used: u64,
}

impl Cache {
    /// The workspace of `root` (canonical): the one kept when the files are
    /// unchanged, else a new scan. A failed scan keeps nothing, so the next
    /// call tries again.
    pub(crate) fn workspace(&mut self, root: &Path) -> Result<Arc<Workspace>> {
        self.clock += 1;
        let stamp = archmap_app::stamp(root)?;
        if let Some(entry) = self.entries.iter_mut().find(|e| e.root == root) {
            if entry.stamp == stamp {
                entry.used = self.clock;
                return Ok(entry.workspace.clone());
            }
        }
        self.entries.retain(|e| e.root != root);
        let workspace = Arc::new(Workspace::scan(root, ScanMode::Full)?);
        self.scans += 1;
        if self.entries.len() >= MAX_ROOTS {
            if let Some(oldest) = (0..self.entries.len()).min_by_key(|&i| self.entries[i].used) {
                self.entries.remove(oldest);
            }
        }
        self.entries.push(Entry {
            root: root.to_path_buf(),
            stamp,
            workspace: workspace.clone(),
            used: self.clock,
        });
        Ok(workspace)
    }

    /// How many scans the cache has made.
    pub(crate) fn scans(&self) -> usize {
        self.scans
    }
}
