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
    /// A TS/JS import whose every name the loaded file only re-exports: it
    /// counts for the files that define them, through its `via` evidence.
    Through,
}

pub(crate) use archmap_core::via_place;

/// A statement: its file and line.
type Statement<'g> = (&'g str, Option<u32>);

/// What the rule needs to know of the whole graph.
pub(crate) struct Pairs<'g> {
    full: &'g ArchitectureGraph,
    /// The names each TS/JS `import` statement takes from the file it loads,
    /// its values and its types together.
    taken: BTreeMap<Statement<'g>, BTreeSet<&'g str>>,
    /// The names each statement reaches through re-exports, with the file
    /// that defines each: one per name taken, the names as those files
    /// declare them.
    walked: BTreeMap<Statement<'g>, BTreeSet<(&'g str, &'g str)>>,
}

impl<'g> Pairs<'g> {
    pub(crate) fn new(full: &'g ArchitectureGraph) -> Self {
        let mut taken: BTreeMap<Statement, BTreeSet<&str>> = BTreeMap::new();
        let mut walked: BTreeMap<Statement, BTreeSet<(&str, &str)>> = BTreeMap::new();
        for e in full.edges.iter().flat_map(|edge| &edge.evidence) {
            let at = (e.file.as_str(), e.line);
            let names = e.names.iter().map(String::as_str);
            match (e.note.as_deref(), e.target.as_deref()) {
                (Some("import"), Some(_)) => taken.entry(at).or_default().extend(names),
                (Some(note), Some(target)) if via_place(note).is_some() => walked
                    .entry(at)
                    .or_default()
                    .extend(names.map(|name| (target, name))),
                _ => {}
            }
        }
        Pairs {
            full,
            taken,
            walked,
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
        // the evidence for the file a TS/JS statement loads (Rust notes
        // `use`, and its re-exports never make such evidence): it passes
        // through when re-exports lead each of its names to the file that
        // defines it. A name the loaded file declares itself, an anonymous
        // default included, one that leads outside the scan and one found
        // nowhere have no `via` evidence, so the counts differ and the
        // statement counts for the loaded file; two names of one definition
        // count for it too.
        let at = (e.file.as_str(), e.line);
        let through = e.note.as_deref() == Some("import")
            && !e.names.contains(WHOLE_MODULE)
            && match (self.pairs.taken.get(&at), self.pairs.walked.get(&at)) {
                (Some(taken), Some(walked)) => taken.len() == walked.len(),
                _ => false,
            };
        if through {
            return Counted::Through;
        }
        if e.test {
            Counted::Test
        } else {
            Counted::Production
        }
    }
}
