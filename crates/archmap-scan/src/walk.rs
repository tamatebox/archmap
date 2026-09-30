use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::ScanError;

/// Directories that are never interesting for architecture and would only
/// slow the scan down, even when not git-ignored.
const ALWAYS_SKIP: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
];

/// Collect every file under `root`, honoring `.gitignore` and skipping
/// hidden files. Results are relative to `root` and sorted.
pub fn collect_files(root: &Path) -> Result<Vec<PathBuf>, ScanError> {
    let mut files = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !(entry.file_type().is_some_and(|t| t.is_dir()) && ALWAYS_SKIP.contains(&name.as_ref()))
        })
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();

    for entry in walker {
        let entry = entry?;
        // Directories are not followed through symlinks (cycle safety), but a
        // symlinked file is still a file worth reading (Homebrew site-packages
        // and vendored trees do this).
        let is_file = match entry.file_type() {
            Some(t) if t.is_file() => true,
            Some(t) if t.is_symlink() => entry.path().metadata().is_ok_and(|m| m.is_file()),
            _ => false,
        };
        if !is_file {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            files.push(rel.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}
