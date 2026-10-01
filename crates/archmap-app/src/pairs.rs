//! How an import statement counts for the pair of components it connects:
//! one rule, so that `summary` and `query` give a pair the same counts.

use std::collections::{BTreeMap, BTreeSet};

use archmap_core::{ArchitectureGraph, ComponentId, Evidence, WHOLE_MODULE};

/// What a statement of an import edge counts as for its pair, best first: a
/// statement with several pieces of evidence counts as the best of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Counted {
    /// Production code that depends on the target.
    Production,
    /// Test code that depends on the target.
    Test,
    /// A component's own entry file (an `index.*`, an `__init__.py`) that
    /// imports one of its submodules: what the component holds, not what it
    /// depends on.
    Entry,
    /// A TS/JS import of names that the loaded file only re-exports: it
    /// counts for the files that define them, through its `via` evidence.
    Through,
}

/// The re-export a statement went through, `src/index.ts:2` of
/// `import via src/index.ts:2`: the analyzers note a walk as one word (the
/// statement's own note: `import`, `require`, `vi.mock`, `use`), ` via `
/// and the place. A note that only contains ` via `, as a specifier written
/// with it does (`import ./a via b: no file matches`), is none.
pub(crate) fn via_place(note: &str) -> Option<&str> {
    let (word, place) = note.split_once(" via ")?;
    let line = place.rsplit_once(':')?.1;
    let one_word = !word.is_empty() && !word.contains(char::is_whitespace);
    let numbered = !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit());
    (one_word && numbered).then_some(place)
}

/// What the rule needs to know of the whole graph.
pub(crate) struct Pairs<'g> {
    full: &'g ArchitectureGraph,
    /// Statements that reach a name through a re-export.
    walked: BTreeSet<(&'g str, Option<u32>)>,
    /// The names each file declares, by its symbols.
    defined: BTreeMap<&'g str, BTreeSet<&'g str>>,
}

impl<'g> Pairs<'g> {
    pub(crate) fn new(full: &'g ArchitectureGraph) -> Self {
        let walked = full
            .edges
            .iter()
            .flat_map(|edge| &edge.evidence)
            .filter(|e| e.note.as_deref().and_then(via_place).is_some())
            .map(|e| (e.file.as_str(), e.line))
            .collect();
        let mut defined: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for symbol in full.symbols.values() {
            if let Some(at) = symbol.location() {
                defined
                    .entry(at.file.as_str())
                    .or_default()
                    .insert(symbol.name.as_str());
            }
        }
        Pairs {
            full,
            walked,
            defined,
        }
    }

    /// The rule for the import statements from `from` to `to`, components
    /// of `rolled`.
    pub(crate) fn pair(
        &self,
        rolled: &ArchitectureGraph,
        from: &ComponentId,
        to: &ComponentId,
    ) -> Pair<'_, 'g> {
        // the entry files `from` names as its own, when `to` is inside it
        let entry = if from != to && rolled.containment_path(to).contains(from) {
            self.full
                .component(from)
                .into_iter()
                .flat_map(|c| &c.evidence)
                .filter(|e| matches!(e.note.as_deref(), Some("index" | "package")))
                .map(|e| e.file.as_str())
                .collect()
        } else {
            BTreeSet::new()
        };
        Pair { pairs: self, entry }
    }
}

/// The rule for one pair of components.
pub(crate) struct Pair<'p, 'g> {
    pairs: &'p Pairs<'g>,
    entry: BTreeSet<&'g str>,
}

impl Pair<'_, '_> {
    /// How one piece of evidence of an import edge of the pair counts.
    pub(crate) fn counted(&self, e: &Evidence) -> Counted {
        if self.entry.contains(e.file.as_str()) {
            return Counted::Entry;
        }
        // the evidence for the file a TS/JS statement loads; Rust notes
        // `use`, and its re-exports never make such evidence
        let own = |name: &String| {
            e.target
                .as_deref()
                .and_then(|t| self.pairs.defined.get(t))
                .is_some_and(|names| names.contains(name.as_str()))
        };
        if e.note.as_deref() == Some("import")
            && self.pairs.walked.contains(&(e.file.as_str(), e.line))
            && !e.names.contains(WHOLE_MODULE)
            && !e.names.iter().any(own)
        {
            return Counted::Through;
        }
        if e.test {
            Counted::Test
        } else {
            Counted::Production
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_walked_note_names_a_re_export() {
        for (note, place) in [
            ("import via src/index.ts:2", Some("src/index.ts:2")),
            (
                "import() via src/a b/index.ts:12",
                Some("src/a b/index.ts:12"),
            ),
            ("vi.mock via src/index.ts:3", Some("src/index.ts:3")),
            ("use via src/shapes/mod.rs:2", Some("src/shapes/mod.rs:2")),
            ("import ./a via b: no file matches", None),
            ("import pkg/c via d", None),
            ("import via src/index.ts", None),
            (" via src/index.ts:2", None),
        ] {
            assert_eq!(via_place(note), place, "{note}");
        }
    }
}
