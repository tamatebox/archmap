use std::path::PathBuf;

/// Fatal scan errors. Per-file problems are reported as warnings instead.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("repository root does not exist or is not a directory: {0}")]
    InvalidRoot(PathBuf),
    #[error("failed to walk repository: {0}")]
    Walk(#[from] ignore::Error),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
