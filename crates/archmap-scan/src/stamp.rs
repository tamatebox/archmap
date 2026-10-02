//! A cheap fingerprint of what a scan reads: every walked file with its
//! size and times, the installed packages the Python analyzer reads
//! outside the walk, and the git HEAD the history starts from. A long-running caller compares stamps to tell whether
//! a graph it keeps is still current, without scanning again.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{walk, ScanError};

/// What [`stamp`] saw. Two stamps are equal when a scan would read the same
/// files, as far as sizes and times tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    files: Vec<Entry>,
    /// `site-packages` directories of the virtualenvs the Python analyzer
    /// reads, with their modification times: installing or removing a
    /// distribution changes them.
    installed: Vec<(PathBuf, Option<SystemTime>)>,
    /// HEAD and whether the clone is shallow, which the history a command
    /// reads starts from (see [`crate::history`]).
    head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    /// `None` when the file went away between the walk and the stat.
    len: Option<u64>,
    modified: Option<SystemTime>,
    /// The inode change time on Unix, which `cp -p` and `rsync -t` move
    /// although they keep the modification time.
    changed: Option<(i64, i64)>,
}

/// Stamp the repository at `root` the way [`crate::scan`] walks it.
pub fn stamp(root: &Path) -> Result<Stamp, ScanError> {
    if !root.is_dir() {
        return Err(ScanError::InvalidRoot(root.to_path_buf()));
    }
    let root = root.canonicalize().map_err(|source| ScanError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let walked = walk::collect_files(&root)?;
    let files = walked
        .iter()
        .map(|relative| {
            // through a symlink to its target, as the walk reads it
            let meta = std::fs::metadata(root.join(relative)).ok();
            Entry {
                path: relative.clone(),
                len: meta.as_ref().map(std::fs::Metadata::len),
                modified: meta.as_ref().and_then(|m| m.modified().ok()),
                changed: meta.as_ref().and_then(changed),
            }
        })
        .collect();
    let installed = crate::python::installed_dirs(&root, &walked)
        .into_iter()
        .map(|dir| {
            let modified = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
            (dir, modified)
        })
        .collect();
    let head = crate::history::head_stamp(&root);
    Ok(Stamp {
        files,
        installed,
        head,
    })
}

#[cfg(unix)]
fn changed(meta: &std::fs::Metadata) -> Option<(i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.ctime(), meta.ctime_nsec()))
}

#[cfg(not(unix))]
fn changed(_meta: &std::fs::Metadata) -> Option<(i64, i64)> {
    None
}
