//! Where a symbol is used: the identifiers in code that resolve to it, read
//! on demand for one symbol. A use is a fact of its own kind, kept out of the
//! [`ArchitectureGraph`](crate::ArchitectureGraph): never an edge, so rules,
//! cycles, signals and roll-up never see it.

use serde::{Deserialize, Serialize};

use crate::Evidence;

/// What a use does with the symbol, by its place in the syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UseRole {
    /// Called: `f()`, `ns.f()`, a tagged template.
    Call,
    /// Constructed with `new`.
    New,
    /// Rendered as a JSX element: `<Button />`.
    Jsx,
    /// Named in a type, which never runs.
    Type,
    /// Any other use: passed as a value, assigned, compared.
    Read,
}

impl UseRole {
    /// The serialized name (`call`, `new`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            UseRole::Call => "call",
            UseRole::New => "new",
            UseRole::Jsx => "jsx",
            UseRole::Type => "type",
            UseRole::Read => "read",
        }
    }
}

/// An import statement, by the file and line its evidence names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ImportPlace {
    pub file: String,
    pub line: u32,
}

/// A place in code that uses a symbol. Uses sort by file, line, column
/// and role.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolUse {
    /// The file and line, `test` in test code, `type_only` in a type.
    pub evidence: Evidence,
    /// 1-based column, so that two uses on one line stay two.
    pub column: u32,
    pub role: UseRole,
    /// The name the code writes, when it is not the symbol's own: `fp` for
    /// `import { formatPrice as fp }`, `m.formatPrice`, `this.pay`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    /// The import statement whose binding the use goes through; none in the
    /// file that defines the symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<ImportPlace>,
}

impl Ord for SymbolUse {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let key = |u: &'_ SymbolUse| {
            (
                u.evidence.file.clone(),
                u.evidence.line,
                u.column,
                u.role,
                u.binding.clone(),
                u.statement.clone(),
            )
        };
        key(self)
            .cmp(&key(other))
            .then_with(|| self.evidence.cmp(&other.evidence))
    }
}

impl PartialOrd for SymbolUse {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A statement that passes the symbol on under another name: what takes
/// that name is not among the uses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Renamed {
    pub evidence: Evidence,
    /// The name it passes the symbol on as.
    pub name: String,
}

/// Why a file the uses pass meant to read was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnreadReason {
    /// No uses pass reads the file's language yet.
    LanguageNotRead,
    /// The parser gave up on the file.
    ParseError,
    /// The statement's line holds more than one statement that could bind
    /// the symbol.
    AmbiguousStatement,
    /// The statement's line no longer holds it: the file changed since the
    /// scan.
    StatementNotFound,
    /// What the statement loads offers no path to the symbol that the pass
    /// can follow.
    NoPath,
    /// The file's text differs from what the scan read, so what it holds
    /// cannot be placed in what the scan resolved.
    Changed,
    /// The file is gone or cannot be read.
    FileGone,
    /// Its scope binds the statement's name again (Python: an import and a
    /// later `def` of the name, two imports in `try` and `except
    /// ImportError`): which binding code reads depends on run order.
    Rebound,
    /// Code in the file may reach the binding by a computed name
    /// (`globals()`, `sys.modules`, `exec`), or a star import may bind it
    /// by an `__all__` that code builds.
    DynamicAccess,
    /// The file is too large to parse on demand.
    TooLarge,
}

impl UnreadReason {
    /// How the text names the reason.
    pub fn as_str(self) -> &'static str {
        match self {
            UnreadReason::LanguageNotRead => "language not read yet",
            UnreadReason::ParseError => "parse error",
            UnreadReason::AmbiguousStatement => "ambiguous statement",
            UnreadReason::StatementNotFound => "statement not found",
            UnreadReason::NoPath => "no path to the symbol",
            UnreadReason::Changed => "changed since the scan",
            UnreadReason::FileGone => "file gone",
            UnreadReason::Rebound => "name bound again",
            UnreadReason::DynamicAccess => "names reached dynamically",
            UnreadReason::TooLarge => "file too large",
        }
    }
}

/// A file, or one statement in it, that the uses pass did not read.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Unread {
    pub file: String,
    /// The statement's line, when only that statement was not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub reason: UnreadReason,
}

/// Where the code reads and writes an environment variable, read on demand
/// for one name from the files whose text names it or `process.env`.
/// Nothing of it enters the graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvUses {
    /// Reads of it: `process.env.X`, `process.env["X"]`, a destructuring
    /// of `process.env` that takes it, `import.meta.env.X`; the note names
    /// the object read.
    pub reads: Vec<Evidence>,
    /// Writes of it: an assignment, `delete`, a test's `vi.stubEnv("X")`.
    #[serde(default)]
    pub writes: Vec<Evidence>,
    /// `process.env[key]` with a key no literal writes: it may be this one.
    #[serde(default)]
    pub computed: Vec<Evidence>,
    /// `process.env` used whole (passed, spread, kept in a variable, a
    /// destructuring with a rest): what takes it may read this one.
    #[serde(default)]
    pub whole: Vec<Evidence>,
    /// Files that name it but could not be read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<Unread>,
    /// How many files were read.
    pub files_read: usize,
}

impl EnvUses {
    /// Every list in file and line order.
    pub fn normalize(&mut self) {
        for list in [
            &mut self.reads,
            &mut self.writes,
            &mut self.computed,
            &mut self.whole,
        ] {
            list.sort();
            list.dedup();
        }
        self.unread.sort();
        self.unread.dedup();
    }
}

/// What the uses pass found for one symbol. Every statement it reads ends
/// in one of its lists: a use through it, `unused`, an escape in its file,
/// `renamed`, `passed_on`, `values`, `mocked` or `unread`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolUses {
    pub uses: Vec<SymbolUse>,
    /// Import statements that bind the symbol and never use the binding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unused: Vec<Evidence>,
    /// Places where a binding of the symbol's module whole is used other than
    /// by a static name (passed as a value, `ns[key]`): the module escapes
    /// there, so that code may use the symbol. Noted `class` where a static
    /// member's class escapes instead (`make(Wallet)`), and `string` where
    /// a string names the symbol (Python).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escapes: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renamed: Vec<Renamed>,
    /// Statements that only pass the symbol on, by a re-export, without
    /// using it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passed_on: Vec<Evidence>,
    /// For a member that is not static, or whose class a statement's file
    /// extends: statements that bind its class (or the class's module) and
    /// use the member no way the pass reads. Values or subclasses of the
    /// class may reach it there unseen, so they are no negative fact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Evidence>,
    /// For a class member: the places that extend its class
    /// (`class Rich extends Wallet`), through whose subclasses the member
    /// may be reached unseen (`Rich.open()`, `super.open()`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subclasses: Vec<Evidence>,
    /// Where a test's mock stands in for the symbol: the key of the object
    /// its factory returns that names it, or its class for a member
    /// (`vi.mock('./money', () => ({ formatPrice: vi.fn() }))`), and the call
    /// of a mock that replaces the module when no key can be read to name
    /// it. No use, but a place to edit when the symbol is renamed or removed.
    /// `names` holds the key when it names the symbol otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mocked: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<Unread>,
}

impl SymbolUses {
    /// Sort every list and drop repeats, so the same files always give the
    /// same answer.
    pub fn normalize(&mut self) {
        fn tidy<T: Ord>(list: &mut Vec<T>) {
            list.sort();
            list.dedup();
        }
        tidy(&mut self.uses);
        tidy(&mut self.unused);
        tidy(&mut self.escapes);
        tidy(&mut self.renamed);
        tidy(&mut self.passed_on);
        tidy(&mut self.values);
        tidy(&mut self.subclasses);
        tidy(&mut self.mocked);
        tidy(&mut self.unread);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn use_at(line: u32, column: u32, role: UseRole) -> SymbolUse {
        SymbolUse {
            evidence: Evidence::new("src/a.ts").at_line(line),
            column,
            role,
            binding: None,
            statement: None,
        }
    }

    #[test]
    fn uses_sort_by_place_and_two_on_one_line_stay_two() {
        let mut typed = use_at(3, 1, UseRole::Type);
        typed.evidence.type_only = true;
        let mut found = SymbolUses {
            uses: vec![
                use_at(3, 9, UseRole::Read),
                use_at(3, 1, UseRole::Call),
                use_at(1, 5, UseRole::Call),
                use_at(3, 1, UseRole::Call),
                // a type use comes by its column, not after every other
                typed.clone(),
            ],
            ..Default::default()
        };
        found.normalize();
        let places: Vec<(Option<u32>, u32, UseRole)> = found
            .uses
            .iter()
            .map(|u| (u.evidence.line, u.column, u.role))
            .collect();
        assert_eq!(
            places,
            [
                (Some(1), 5, UseRole::Call),
                (Some(3), 1, UseRole::Call),
                (Some(3), 1, UseRole::Type),
                (Some(3), 9, UseRole::Read)
            ]
        );
    }

    #[test]
    fn json_names_roles_and_reasons_in_snake_case_and_leaves_out_empty_lists() {
        let mut call = use_at(2, 4, UseRole::Call);
        call.binding = Some("fp".into());
        call.statement = Some(ImportPlace {
            file: "src/b.ts".into(),
            line: 1,
        });
        let found = SymbolUses {
            uses: vec![call],
            unread: vec![Unread {
                file: "src/c.ts".into(),
                line: Some(4),
                reason: UnreadReason::StatementNotFound,
            }],
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&found).unwrap(),
            serde_json::json!({
                "uses": [{
                    "evidence": {"file": "src/a.ts", "line": 2},
                    "column": 4,
                    "role": "call",
                    "binding": "fp",
                    "statement": {"file": "src/b.ts", "line": 1}
                }],
                "unread": [{"file": "src/c.ts", "line": 4, "reason": "statement_not_found"}]
            })
        );
    }
}
