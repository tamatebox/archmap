//! Which source files are test code, for Python and TS/JS alike: what test
//! runners and scanners agree on. Rust marks test code by `#[cfg(test)]`
//! and `#[test]` instead, never by path. And what a file of test code is to
//! a test runner: a test it runs, or test code it runs no test of.

use std::path::Path;

use crate::rust::TargetKind;

/// What a file of test code is to a test runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    /// A file the runner collects and runs.
    Test,
    /// pytest's `conftest.py`, which it loads without an import for every
    /// test in its directory and below.
    Conftest,
    /// Test code no runner collects: what the tests import.
    Helper,
    /// A Cargo example, or a module of one.
    Example,
    /// A Cargo bench, or a module of one.
    Bench,
}

/// A Rust file by the targets that hold it, a test's root first.
pub(crate) fn rust_kind(targets: &[(TargetKind, bool)]) -> TestKind {
    let has = |kind: TargetKind, root: Option<bool>| {
        targets
            .iter()
            .any(|(k, r)| *k == kind && root.is_none_or(|root| root == *r))
    };
    if has(TargetKind::Test, Some(true)) {
        TestKind::Test
    } else if has(TargetKind::Example, None) {
        TestKind::Example
    } else if has(TargetKind::Bench, None) {
        TestKind::Bench
    } else if has(TargetKind::Test, Some(false)) {
        TestKind::Helper
    } else {
        TestKind::Test
    }
}

/// A Python or TS/JS file of test code by its name and directory.
pub(crate) fn kind_by_name(path: &Path) -> TestKind {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let below_tests = path.parent().is_some_and(|dir| {
        dir.components()
            .any(|c| c.as_os_str().to_str() == Some("__tests__"))
    });
    if name == "conftest.py" {
        TestKind::Conftest
    } else if is_test_named(path) || below_tests {
        TestKind::Test
    } else {
        TestKind::Helper
    }
}

/// A file named like a test (`*.test.*`, `*.spec.*`, Vitest's type tests
/// `*.test-d.*` and `*.spec-d.*`, `test_*.py`, `*_test.py`, `conftest.py`,
/// and Django's and unittest's `tests.py`),
/// or a file below a directory named `test`, `tests`, `__tests__` or
/// `__mocks__`.
pub fn is_test_code(path: &Path) -> bool {
    let below = path.parent().is_some_and(|dir| {
        dir.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some("test" | "tests" | "__tests__" | "__mocks__")
            )
        })
    });
    is_test_named(path) || below
}

/// A file named like a test, whatever its directory: the half of
/// [`is_test_code`] that also tells which Python files give no symbols.
pub fn is_test_named(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    [".test.", ".spec.", ".test-d.", ".spec-d."]
        .iter()
        .any(|marker| name.contains(marker))
        || (name.ends_with(".py")
            && (name.starts_with("test_")
                || name.ends_with("_test.py")
                || name == "conftest.py"
                || name == "tests.py"))
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
            // an app's test module, which unittest and Django collect
            "shop/orders/tests.py",
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
            "shop/testing.py",
            "shop/tests_helper.py",
        ] {
            assert!(!is_test_code(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn a_runner_runs_test_files_and_the_rest_of_test_code_is_helpers() {
        for (path, kind) in [
            ("tests/test_billing.py", TestKind::Test),
            ("shop/orders/tests.py", TestKind::Test),
            ("tests/unit/conftest.py", TestKind::Conftest),
            ("tests/unit/factories.py", TestKind::Helper),
            ("src/lib/money.test.ts", TestKind::Test),
            ("src/__tests__/setup.ts", TestKind::Test),
            ("tests/helpers.ts", TestKind::Helper),
            ("src/lib/__mocks__/money.ts", TestKind::Helper),
        ] {
            assert_eq!(kind_by_name(Path::new(path)), kind, "{path}");
        }
        // a test's root, its modules, examples and benches
        let test = |root| (TargetKind::Test, root);
        assert_eq!(rust_kind(&[test(true)]), TestKind::Test);
        assert_eq!(rust_kind(&[test(false)]), TestKind::Helper);
        assert_eq!(rust_kind(&[test(false), test(true)]), TestKind::Test);
        assert_eq!(rust_kind(&[(TargetKind::Example, true)]), TestKind::Example);
        assert_eq!(rust_kind(&[(TargetKind::Bench, false)]), TestKind::Bench);
    }
}
