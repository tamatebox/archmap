//! Which files the analyzer reads, and the language of each.

use std::path::Path;

pub const LANGUAGE: &str = "typescript";
/// The language of `.js`, `.jsx`, `.mjs` and `.cjs` files, which this
/// analyzer reads too.
pub const JAVASCRIPT: &str = "javascript";

/// `typescript` or `javascript` for a TS/JS file, `None` for any other.
pub(crate) fn language_of(file: &Path) -> Option<&'static str> {
    crate::languages::language_of(file).filter(|l| *l == LANGUAGE || *l == JAVASCRIPT)
}

pub(crate) fn is_code(file: &Path) -> bool {
    language_of(file).is_some()
}
