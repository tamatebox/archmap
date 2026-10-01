//! Source languages by file extension, so a scan can report how many files
//! of each language it saw, including languages no analyzer reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Language names and their file extensions. A language with an analyzer
/// uses the analyzer's language name (`python`, `rust`). Configuration and
/// data formats (YAML, JSON, CSV) are not source code and are not counted.
const LANGUAGES: &[(&str, &[&str])] = &[
    ("c", &["c", "h"]),
    ("cpp", &["cc", "cpp", "cxx", "hh", "hpp", "hxx"]),
    ("csharp", &["cs"]),
    ("cython", &["pxd", "pyx"]),
    ("dart", &["dart"]),
    ("elixir", &["ex", "exs"]),
    ("go", &["go"]),
    ("java", &["java"]),
    ("javascript", &["cjs", "js", "jsx", "mjs"]),
    ("kotlin", &["kt", "kts"]),
    ("lua", &["lua"]),
    ("notebook", &["ipynb"]),
    ("php", &["php"]),
    ("python", &["py"]),
    ("r", &["R", "r"]),
    ("ruby", &["rb"]),
    ("rust", &["rs"]),
    ("scala", &["scala"]),
    ("shell", &["bash", "sh", "zsh"]),
    ("sql", &["sql"]),
    ("svelte", &["svelte"]),
    ("swift", &["swift"]),
    ("terraform", &["hcl", "tf"]),
    ("typescript", &["cts", "mts", "ts", "tsx"]),
    ("vue", &["vue"]),
];

/// The language of a file, judged by its extension.
pub fn language_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    LANGUAGES
        .iter()
        .find(|(_, exts)| exts.contains(&ext))
        .map(|(name, _)| *name)
}

/// How many of `files` belong to each recognized language.
pub fn count_files(files: &[PathBuf]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for language in files.iter().filter_map(|f| language_of(f)) {
        *counts.entry(language).or_default() += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzer_languages_and_others_are_recognized() {
        assert_eq!(language_of(Path::new("src/a.py")), Some("python"));
        assert_eq!(language_of(Path::new("crates/x/src/lib.rs")), Some("rust"));
        assert_eq!(language_of(Path::new("sql/daily.sql")), Some("sql"));
        assert_eq!(
            language_of(Path::new("notebooks/eda.ipynb")),
            Some("notebook")
        );
        assert_eq!(language_of(Path::new("infra/main.tf")), Some("terraform"));
        assert_eq!(language_of(Path::new("run.sh")), Some("shell"));
        assert_eq!(language_of(Path::new("web/app.tsx")), Some("typescript"));
        // configuration, data and documentation are not source code
        for other in [
            "conf.yaml",
            "data.json",
            "rows.csv",
            "README.md",
            "Makefile",
        ] {
            assert_eq!(language_of(Path::new(other)), None, "{other}");
        }
    }

    #[test]
    fn counts_files_per_language() {
        let files: Vec<PathBuf> = ["a.py", "b.py", "c.sql", "d.yaml"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            count_files(&files),
            BTreeMap::from([("python", 2), ("sql", 1)])
        );
    }

    #[test]
    fn analyzer_language_names_match_the_table() {
        for name in [
            crate::python::LANGUAGE,
            crate::rust::LANGUAGE,
            crate::typescript::LANGUAGE,
            crate::typescript::JAVASCRIPT,
        ] {
            assert!(
                LANGUAGES.iter().any(|(n, _)| *n == name),
                "{name} missing from LANGUAGES"
            );
        }
    }
}
