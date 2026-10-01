use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The name evidence records for an import that takes a whole module: a
/// namespace import, a glob, a module imported by itself.
pub const WHOLE_MODULE: &str = "*";

/// Why a node or edge exists in the graph.
///
/// Every fact we extract should point back to the place it was derived from,
/// so that a human or an agent can verify it and jump to the source. For
/// dependencies on source code, evidence also keeps the file the statement
/// points at: the fine detail that roll-up hides and that impact and cycle
/// checks recover when they need it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Evidence {
    /// Path relative to the scanned repository root.
    pub file: String,
    /// 1-based line number, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Free-form note explaining the derivation (for example `use` or
    /// `Cargo.toml [dependencies]`). A note of one word names only the kind
    /// of statement (`import`, `use`); longer notes say how a name was
    /// resolved or where it is declared, and views may show only those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// For a dependency on a file in the repository: that file, relative to
    /// the root. `None` when the target is external or not resolved to a
    /// file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Where the statement sits, for languages where it matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    /// For a dependency on a file in the repository: the names the
    /// statement takes from `target`, as `target` exports them, and
    /// [`WHOLE_MODULE`] for the whole module. Empty with a `target`: the
    /// statement loads the file without taking a name.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub names: BTreeSet<String>,
    /// The statement is test code: it runs only for tests.
    #[serde(default, skip_serializing_if = "is_false")]
    pub test: bool,
    /// The statement takes types only, which the compiler erases: it never
    /// runs (`import type`, `export type ... from`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub type_only: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Where an import statement sits in its file. Both scopes run, so both
/// close cycles; a statement that never runs is `Evidence::type_only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Outside any function body. In Python it runs when the file is loaded.
    Module,
    /// Inside a function body. In Python it runs only when the function is
    /// called.
    Local,
}

impl Evidence {
    pub fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line: None,
            note: None,
            target: None,
            scope: None,
            names: BTreeSet::new(),
            test: false,
            type_only: false,
        }
    }

    pub fn at_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn pointing_at(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn in_scope(mut self, scope: Scope) -> Self {
        self.scope = Some(scope);
        self
    }

    pub fn in_test(mut self, test: bool) -> Self {
        self.test = test;
        self
    }

    pub fn type_only(mut self, type_only: bool) -> Self {
        self.type_only = type_only;
        self
    }

    pub fn taking<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.names.extend(names.into_iter().map(Into::into));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sorted_once_and_left_out_of_json_when_empty() {
        let evidence =
            Evidence::new("a.ts")
                .pointing_at("b.ts")
                .taking(["b", WHOLE_MODULE, "a", "b"]);
        let names: Vec<&str> = evidence.names.iter().map(String::as_str).collect();
        assert_eq!(names, ["*", "a", "b"]);
        let json = serde_json::to_string(&evidence).unwrap();
        assert!(json.ends_with(r#""names":["*","a","b"]}"#), "{json}");

        let bare = serde_json::to_string(&Evidence::new("a.ts")).unwrap();
        assert!(!bare.contains("names"), "{bare}");
        // evidence written before names existed still reads
        let old: Evidence = serde_json::from_str(r#"{"file":"a.ts","target":"b.ts"}"#).unwrap();
        assert!(old.names.is_empty());
    }

    #[test]
    fn imports_of_types_only_are_marked_only_when_they_are() {
        let marked = serde_json::to_string(&Evidence::new("a.ts").type_only(true)).unwrap();
        assert!(marked.ends_with(r#""type_only":true}"#), "{marked}");
        let plain = serde_json::to_string(&Evidence::new("a.ts").type_only(false)).unwrap();
        assert_eq!(plain, r#"{"file":"a.ts"}"#);
        let old: Evidence = serde_json::from_str(r#"{"file":"a.ts"}"#).unwrap();
        assert!(!old.type_only);
    }

    #[test]
    fn test_code_is_marked_only_when_it_is() {
        let marked = serde_json::to_string(&Evidence::new("tests/a.py").in_test(true)).unwrap();
        assert!(marked.ends_with(r#""test":true}"#), "{marked}");
        let plain = serde_json::to_string(&Evidence::new("src/a.py").in_test(false)).unwrap();
        assert_eq!(plain, r#"{"file":"src/a.py"}"#);
        let old: Evidence = serde_json::from_str(r#"{"file":"a.py"}"#).unwrap();
        assert!(!old.test);
    }
}
