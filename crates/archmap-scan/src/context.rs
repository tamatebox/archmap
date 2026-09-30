use std::path::{Path, PathBuf};

use crate::{walk, ScanError, ScanOptions};

/// Everything analyzers may look at: the repository root and the list of
/// files that survived ignore rules.
///
/// Paths in `files` are relative to `root` and use `/` separators so that
/// evidence is portable across machines.
#[derive(Debug, Clone)]
pub struct RepoContext {
    root: PathBuf,
    files: Vec<PathBuf>,
    options: ScanOptions,
}

impl RepoContext {
    pub fn load(root: &Path, options: ScanOptions) -> Result<Self, ScanError> {
        if !root.is_dir() {
            return Err(ScanError::InvalidRoot(root.to_path_buf()));
        }
        let root = root.canonicalize().map_err(|source| ScanError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let files = walk::collect_files(&root)?;
        Ok(Self {
            root,
            files,
            options,
        })
    }

    /// Build a context from an explicit file list (useful in tests).
    pub fn from_files(root: impl Into<PathBuf>, files: Vec<PathBuf>, options: ScanOptions) -> Self {
        Self {
            root: root.into(),
            files,
            options,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn root_display(&self) -> String {
        self.root.display().to_string()
    }

    pub fn options(&self) -> &ScanOptions {
        &self.options
    }

    /// All files, relative to root, sorted.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Files whose file name equals `name` (for manifests such as `Cargo.toml`).
    pub fn files_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Path> + 'a {
        self.files
            .iter()
            .filter(move |p| p.file_name().is_some_and(|f| f == name))
            .map(PathBuf::as_path)
    }

    /// Files with the given extension (without the dot).
    pub fn files_with_extension<'a>(&'a self, ext: &'a str) -> impl Iterator<Item = &'a Path> + 'a {
        self.files
            .iter()
            .filter(move |p| p.extension().is_some_and(|e| e == ext))
            .map(PathBuf::as_path)
    }

    pub fn absolute(&self, relative: &Path) -> PathBuf {
        self.root.join(relative)
    }

    pub fn read_to_string(&self, relative: &Path) -> Result<String, ScanError> {
        let path = self.absolute(relative);
        std::fs::read_to_string(&path).map_err(|source| ScanError::Io { path, source })
    }
}

/// Render a relative path with `/` separators for evidence and ids.
pub fn display_path(path: &Path) -> String {
    let s = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if s.is_empty() {
        ".".to_owned()
    } else {
        s
    }
}
