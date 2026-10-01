//! Which source files are test code, for Python and TS/JS alike: what test
//! runners and scanners agree on. Rust marks test code by `#[cfg(test)]`
//! and `#[test]` instead, never by path.

use std::path::Path;

/// A file named like a test (`*.test.*`, `*.spec.*`, Vitest's type tests
/// `*.test-d.*` and `*.spec-d.*`, `test_*.py`, `*_test.py`, `conftest.py`),
/// or a file below a directory named `test`, `tests`, `__tests__` or
/// `__mocks__`.
pub fn is_test_code(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let named = [".test.", ".spec.", ".test-d.", ".spec-d."]
        .iter()
        .any(|marker| name.contains(marker))
        || (name.ends_with(".py")
            && (name.starts_with("test_") || name.ends_with("_test.py") || name == "conftest.py"));
    let below = path.parent().is_some_and(|dir| {
        dir.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some("test" | "tests" | "__tests__" | "__mocks__")
            )
        })
    });
    named || below
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_files_and_test_directories_are_test_code() {
        for path in [
            "src/lib/money.test.ts",
            "e2e/login.spec.ts",
            "pkg/test_core.py",
            "pkg/core_test.py",
            "conftest.py",
            "tests/helpers.ts",
            "pkg/tests/fixtures/data.py",
            "test/run.js",
            "src/__tests__/setup.ts",
            "src/lib/__mocks__/money.ts",
            // Vitest's type tests
            "src/lib/money.test-d.ts",
            "src/lib/money.spec-d.ts",
        ] {
            assert!(is_test_code(Path::new(path)), "{path}");
        }
        for path in [
            "src/testing.ts",
            "src/latest/page.tsx",
            "src/contest.py",
            "src/button.stories.tsx",
            "e2e/helpers.ts",
            "spec/support.js",
            "src/attest.py",
        ] {
            assert!(!is_test_code(Path::new(path)), "{path}");
        }
    }
}
