use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::ScanError;

/// Directories that are never source and would only slow the scan down,
/// wherever they are and even when not git-ignored.
const ALWAYS_SKIP: &[&str] = &[".git", "node_modules", "__pycache__"];

/// Build output, by the manifests of the tools that write it: skipped only
/// beside one of them, so a package named `build` or a subpackage
/// `operations/build/` stays source. Elsewhere ignore rules decide.
const BUILD_OUTPUT: &[(&str, &[&str])] = &[
    (
        "dist",
        &["package.json", "pyproject.toml", "setup.py", "setup.cfg"],
    ),
    (
        "build",
        &[
            "package.json",
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "build.sbt",
        ],
    ),
    (
        "target",
        &[
            "Cargo.toml",
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "build.sbt",
        ],
    ),
];

/// Whether `dir`, a directory named `name`, is build output: beside a
/// manifest of a tool that writes it, and no Python package (what setuptools
/// writes to `build/` holds none directly).
fn is_build_output(dir: &Path, name: &str) -> bool {
    let Some((_, manifests)) = BUILD_OUTPUT.iter().find(|(n, _)| *n == name) else {
        return false;
    };
    let Some(parent) = dir.parent() else {
        return false;
    };
    !dir.join("__init__.py").is_file() && manifests.iter().any(|m| parent.join(m).is_file())
}

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
            // the root is scanned whatever its name
            if entry.depth() == 0 || !entry.file_type().is_some_and(|t| t.is_dir()) {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !(ALWAYS_SKIP.contains(&name.as_ref()) || is_build_output(entry.path(), &name))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_is_skipped_only_beside_the_manifest_that_makes_it() {
        let root = std::env::temp_dir().join(format!("archmap-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for file in [
            "package.json",
            "dist/bundle.js",
            "packages/build/package.json",
            "packages/build/src/a.ts",
            "src/pip/operations/build/wheel.py",
            "py/pyproject.toml",
            "py/build/lib/x.py",
            "flat/pyproject.toml",
            "flat/build/__init__.py",
            "rs/Cargo.toml",
            "rs/target/debug/x.rs",
            "lib/target/mod.py",
            "jvm/pom.xml",
            "jvm/target/classes/x.js",
            "web/node_modules/x/index.js",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let files = collect_files(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let found: Vec<String> = files
            .iter()
            .map(|f| f.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            found,
            [
                // a Python package named `build` is source
                "flat/build/__init__.py",
                "flat/pyproject.toml",
                "jvm/pom.xml",
                "lib/target/mod.py",
                "package.json",
                // a package named `build`
                "packages/build/package.json",
                "packages/build/src/a.ts",
                "py/pyproject.toml",
                "rs/Cargo.toml",
                // a subpackage named `build`
                "src/pip/operations/build/wheel.py",
            ]
        );
    }
}
