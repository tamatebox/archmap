//! What a scan is asked to do, apart from the crate root so that the
//! modules the root calls can take it without depending on the root.

/// Options controlling a scan.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Skip source parsing and only extract manifest-level facts.
    pub manifests_only: bool,
}
