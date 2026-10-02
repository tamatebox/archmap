//! Where a name imported from a Python file is defined when that file
//! binds it by importing it from another, as `pkg/__init__.py` does with
//! `from .charge import pay`: each file's bindings, and the walk through
//! them that `from pkg import pay` makes when it runs.

use std::collections::{BTreeMap, BTreeSet};

/// What a walk sees of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Table {
    /// The names the file defines at module level when it runs, as the scan
    /// reads them: definitions and assignments.
    pub defined: BTreeSet<String>,
    /// The names its module-level `from` imports bind, by the name bound.
    pub bound: BTreeMap<String, Vec<Binding>>,
    /// The names it binds in a way the walk does not follow, `import a.b
    /// as c` or a `from` import of a module outside the scan or of a file
    /// beside it, each with whether every such binding is a type only.
    pub unfollowed: BTreeMap<String, bool>,
    /// Its module-level star imports, by the file each takes whole.
    pub stars: Vec<Binding>,
    /// A module-level statement binds names the walk cannot see, a name
    /// list that could not be read or a star import of a module outside the
    /// scan: a name the file shows no other way may come from it.
    pub opaque: bool,
    /// What a star import of the file takes.
    pub exported: Exported,
}

/// What a star import of a file takes: the names of its `__all__`, else
/// every name without a leading `_`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Exported {
    #[default]
    Public,
    Listed(BTreeSet<String>),
    /// An `__all__` built at runtime: nothing can be said.
    Built,
}

impl Exported {
    /// Whether a star import takes `name`; `None` when nothing can be said.
    fn takes(&self, name: &str) -> Option<bool> {
        match self {
            Exported::Public => Some(!name.starts_with('_')),
            Exported::Listed(names) => Some(names.contains(name)),
            Exported::Built => None,
        }
    }
}

/// Where a name a file binds comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    /// The file the import takes it from.
    pub file: String,
    /// Its name there; [`archmap_core::WHOLE_MODULE`] for a submodule, or
    /// for the file a star import takes.
    pub name: String,
    pub line: u32,
    /// Under `if TYPE_CHECKING:`.
    pub type_only: bool,
}

/// The file that defines an imported name, reached through bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Definition {
    pub file: String,
    /// The first binding on the way: its file and line.
    pub via: (String, u32),
    /// The name in `file`.
    pub name: String,
    /// A binding on the way passes the name on as a type only.
    pub type_only: bool,
}

/// Bindings a walk follows before it gives up, as for TS/JS re-exports.
const MAX_HOPS: usize = 32;

/// What a file shows of a name.
#[derive(Debug, Clone)]
enum Walk {
    /// Nothing.
    Absent,
    /// It defines the name.
    Here,
    /// It passes the name on from the definition.
    Through(Definition),
    /// Nothing can be said: bindings that disagree, a binding the walk
    /// does not follow, names it cannot see, a cycle, too many bindings.
    Unknown,
}

/// Definitions of names as files bind them. What a walk found from a
/// (file, name) is kept for the next walk that meets it with enough
/// bindings left.
pub(crate) struct Definitions<'a> {
    tables: &'a BTreeMap<String, Table>,
    /// What each (file, name) shows, with the bindings its walk followed
    /// below it, unless the hop limit cut that walk.
    walked: BTreeMap<(String, String), (Walk, usize)>,
    /// The walk under way met the hop limit.
    cut: bool,
}

impl<'a> Definitions<'a> {
    pub fn new(tables: &'a BTreeMap<String, Table>) -> Self {
        Definitions {
            tables,
            walked: BTreeMap::new(),
            cut: false,
        }
    }

    /// Where `name`, taken from `file`, is defined, when `file` binds it
    /// from another file. `None` when `file` defines it or shows nothing of
    /// it, and when nothing can be said.
    pub fn of(&mut self, file: &str, name: &str) -> Option<Definition> {
        self.cut = false;
        match self.walk(file, name, 0, &mut BTreeSet::new()).0 {
            Walk::Through(definition) => Some(definition),
            Walk::Absent | Walk::Here | Walk::Unknown => None,
        }
    }

    /// What `file` shows of `name`, and how many bindings below it the
    /// answer rests on. `path` holds the (file, name) pairs the walk is
    /// inside of: meeting one again is a cycle, while two star sources may
    /// meet in one file.
    fn walk(
        &mut self,
        file: &str,
        name: &str,
        hops: usize,
        path: &mut BTreeSet<(String, String)>,
    ) -> (Walk, usize) {
        let key = (file.to_owned(), name.to_owned());
        if let Some((walk, below)) = self.walked.get(&key) {
            if hops + below <= MAX_HOPS {
                return (walk.clone(), *below);
            }
            self.cut = true;
            return (Walk::Unknown, 0);
        }
        if hops > MAX_HOPS {
            self.cut = true;
            return (Walk::Unknown, 0);
        }
        // what a cycle leaves depends on where the walk entered it, so it
        // is kept no more than what the hop limit cuts
        if !path.insert(key.clone()) {
            self.cut = true;
            return (Walk::Unknown, 0);
        }
        let outer = std::mem::replace(&mut self.cut, false);
        let (walk, below) = self.step(file, name, hops, path);
        path.remove(&key);
        if !self.cut {
            self.walked.insert(key, (walk.clone(), below));
        }
        self.cut |= outer;
        (walk, below)
    }

    /// What `file` shows of `name`, from every kind of statement that shows
    /// it: a definition, the bindings, by where they lead, and each star
    /// import (see [`agree`]).
    fn step(
        &mut self,
        file: &str,
        name: &str,
        hops: usize,
        path: &mut BTreeSet<(String, String)>,
    ) -> (Walk, usize) {
        let tables = self.tables;
        let Some(table) = tables.get(file) else {
            return (Walk::Unknown, 0);
        };
        // each with whether it passes a type only
        let mut found: Vec<(Walk, bool)> = Vec::new();
        let mut below = 0;
        if table.defined.contains(name) {
            found.push((Walk::Here, false));
        }
        if let Some(&type_only) = table.unfollowed.get(name) {
            found.push((Walk::Unknown, type_only));
        }
        // the bindings that lead to one place: the first that runs gives
        // the line, and they pass a type only when every one does
        let mut leads: Vec<(&Binding, bool)> = Vec::new();
        for binding in table.bound.get(name).into_iter().flatten() {
            let same = |(b, _): &&mut (&Binding, bool)| {
                (&b.file, &b.name) == (&binding.file, &binding.name)
            };
            match leads.iter_mut().find(same) {
                Some((first, type_only)) => {
                    if first.type_only && !binding.type_only {
                        *first = binding;
                    }
                    *type_only &= binding.type_only;
                }
                None => leads.push((binding, binding.type_only)),
            }
        }
        for (binding, type_only) in leads {
            let (walk, depth) = self.pass_on(file, binding, type_only, hops, path);
            below = below.max(depth);
            let type_only = match &walk {
                Walk::Through(definition) => definition.type_only,
                _ => type_only,
            };
            found.push((walk, type_only));
        }
        if found.is_empty() && table.opaque {
            return (Walk::Unknown, below);
        }
        for star in &table.stars {
            let takes = tables
                .get(&star.file)
                .and_then(|source| source.exported.takes(name));
            match takes {
                None => {
                    found.push((Walk::Unknown, star.type_only));
                    continue;
                }
                Some(false) => continue,
                Some(true) => {}
            }
            let (walk, depth) = self.walk(&star.file, name, hops + 1, path);
            below = below.max(depth + 1);
            let mut reached = match walk {
                Walk::Absent => continue,
                Walk::Unknown => {
                    found.push((Walk::Unknown, star.type_only));
                    continue;
                }
                Walk::Here => Definition {
                    file: star.file.clone(),
                    via: (String::new(), 0),
                    name: name.to_owned(),
                    type_only: false,
                },
                Walk::Through(definition) => definition,
            };
            reached.via = (file.to_owned(), star.line);
            reached.type_only |= star.type_only;
            let type_only = reached.type_only;
            found.push((Walk::Through(reached), type_only));
        }
        (agree(found), below)
    }

    /// The definition behind `binding`, which `file` passes on: where the
    /// file it names leads, or that file when it shows nothing more.
    fn pass_on(
        &mut self,
        file: &str,
        binding: &Binding,
        type_only: bool,
        hops: usize,
        path: &mut BTreeSet<(String, String)>,
    ) -> (Walk, usize) {
        let via = (file.to_owned(), binding.line);
        if binding.name == archmap_core::WHOLE_MODULE {
            let definition = Definition {
                file: binding.file.clone(),
                via,
                name: binding.name.clone(),
                type_only,
            };
            return (Walk::Through(definition), 0);
        }
        let (walk, below) = self.walk(&binding.file, &binding.name, hops + 1, path);
        let walk = match walk {
            Walk::Unknown => Walk::Unknown,
            Walk::Through(definition) => Walk::Through(Definition {
                via,
                type_only: definition.type_only || type_only,
                ..definition
            }),
            Walk::Absent | Walk::Here => Walk::Through(Definition {
                file: binding.file.clone(),
                via,
                name: binding.name.clone(),
                type_only,
            }),
        };
        (walk, below + 1)
    }
}

/// One answer from what each kind of statement of a file says of a name,
/// with whether it passes a type only: only those that run count, when any
/// does. One that cannot tell, or two that lead to different definitions,
/// leave nothing to say; the first gives the line.
fn agree(found: Vec<(Walk, bool)>) -> Walk {
    let running = found.iter().any(|(_, type_only)| !type_only);
    let mut counted = found
        .into_iter()
        .filter(|(_, type_only)| !running || !type_only)
        .map(|(walk, _)| walk);
    let Some(mut answer) = counted.next() else {
        return Walk::Absent;
    };
    for walk in counted {
        answer = match (answer, walk) {
            (Walk::Here, Walk::Here) => Walk::Here,
            (Walk::Through(a), Walk::Through(b)) if (&a.file, &a.name) == (&b.file, &b.name) => {
                Walk::Through(a)
            }
            _ => return Walk::Unknown,
        };
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use archmap_core::WHOLE_MODULE;

    fn binding(file: &str, name: &str, line: u32) -> Binding {
        Binding {
            file: file.into(),
            name: name.into(),
            line,
            type_only: false,
        }
    }

    fn defining(names: &[&str]) -> Table {
        Table {
            defined: names.iter().map(|n| n.to_string()).collect(),
            ..Table::default()
        }
    }

    fn binding_table(bound: &[(&str, Binding)]) -> Table {
        let mut table = Table::default();
        for (name, b) in bound {
            table
                .bound
                .entry(name.to_string())
                .or_default()
                .push(b.clone());
        }
        table
    }

    fn repo(files: Vec<(&str, Table)>) -> BTreeMap<String, Table> {
        files.into_iter().map(|(f, t)| (f.to_owned(), t)).collect()
    }

    fn definition(file: &str, via: (&str, u32), name: &str) -> Definition {
        Definition {
            file: file.into(),
            via: (via.0.into(), via.1),
            name: name.into(),
            type_only: false,
        }
    }

    #[test]
    fn a_name_the_file_defines_needs_no_walk() {
        let tables = repo(vec![("pkg/charge.py", defining(&["pay"]))]);
        assert_eq!(Definitions::new(&tables).of("pkg/charge.py", "pay"), None);
    }

    #[test]
    fn a_bound_name_leads_to_the_file_that_defines_it() {
        let tables = repo(vec![
            (
                "pkg/__init__.py",
                binding_table(&[("pay", binding("pkg/charge.py", "pay", 1))]),
            ),
            ("pkg/charge.py", defining(&["pay"])),
        ]);
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "pay"),
            Some(definition("pkg/charge.py", ("pkg/__init__.py", 1), "pay"))
        );
    }

    #[test]
    fn a_walk_follows_aliases_and_chains_from_the_first_binding() {
        let tables = repo(vec![
            (
                "app/__init__.py",
                binding_table(&[("checkout", binding("pkg/__init__.py", "pay", 3))]),
            ),
            (
                "pkg/__init__.py",
                binding_table(&[("pay", binding("pkg/charge.py", "charge", 1))]),
            ),
            ("pkg/charge.py", defining(&["charge"])),
        ]);
        assert_eq!(
            Definitions::new(&tables).of("app/__init__.py", "checkout"),
            Some(definition(
                "pkg/charge.py",
                ("app/__init__.py", 3),
                "charge"
            ))
        );
    }

    #[test]
    fn a_name_the_bound_file_does_not_show_stays_there() {
        // `pay = make_pay()` in charge.py is no definition the scan reads,
        // but the binding still says where the name comes from
        let tables = repo(vec![
            (
                "pkg/__init__.py",
                binding_table(&[("pay", binding("pkg/charge.py", "pay", 1))]),
            ),
            ("pkg/charge.py", Table::default()),
        ]);
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "pay"),
            Some(definition("pkg/charge.py", ("pkg/__init__.py", 1), "pay"))
        );
    }

    #[test]
    fn bindings_that_disagree_say_nothing_even_midway() {
        // try: from .fast import pay / except ImportError: from .slow import pay
        let disagreeing = binding_table(&[
            ("pay", binding("pkg/fast.py", "pay", 2)),
            ("pay", binding("pkg/slow.py", "pay", 4)),
        ]);
        let tables = repo(vec![
            ("pkg/__init__.py", disagreeing),
            (
                "mid/__init__.py",
                binding_table(&[("pay", binding("pkg/__init__.py", "pay", 1))]),
            ),
        ]);
        let mut definitions = Definitions::new(&tables);
        assert_eq!(definitions.of("pkg/__init__.py", "pay"), None);
        assert_eq!(definitions.of("mid/__init__.py", "pay"), None);
    }

    #[test]
    fn star_sources_must_agree() {
        let star = |file: &str, line| binding(file, WHOLE_MODULE, line);
        let mut one = Table::default();
        one.stars.push(star("pkg/ledger.py", 1));
        one.stars.push(star("pkg/empty.py", 2));
        let mut two = Table::default();
        two.stars.push(star("pkg/ledger.py", 1));
        two.stars.push(star("pkg/other.py", 2));
        let tables = repo(vec![
            ("pkg/one.py", one),
            ("pkg/two.py", two),
            ("pkg/ledger.py", defining(&["post"])),
            ("pkg/other.py", defining(&["post"])),
            ("pkg/empty.py", Table::default()),
        ]);
        let mut definitions = Definitions::new(&tables);
        assert_eq!(
            definitions.of("pkg/one.py", "post"),
            Some(definition("pkg/ledger.py", ("pkg/one.py", 1), "post"))
        );
        assert_eq!(definitions.of("pkg/two.py", "post"), None);
        // a name no star source shows is the file's own
        assert_eq!(definitions.of("pkg/one.py", "other"), None);
    }

    #[test]
    fn star_sources_that_meet_again_are_no_cycle() {
        let star = |file: &str, line| binding(file, WHOLE_MODULE, line);
        let mut init = Table::default();
        init.stars.push(star("pkg/a.py", 1));
        init.stars.push(star("pkg/b.py", 2));
        let mut a = Table::default();
        a.stars.push(star("pkg/common.py", 1));
        let mut b = Table::default();
        b.stars.push(star("pkg/common.py", 1));
        let tables = repo(vec![
            ("pkg/__init__.py", init),
            ("pkg/a.py", a),
            ("pkg/b.py", b),
            ("pkg/common.py", defining(&["x"])),
        ]);
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "x"),
            Some(definition("pkg/common.py", ("pkg/__init__.py", 1), "x"))
        );
    }

    #[test]
    fn a_star_takes_what_its_source_exports() {
        let through = |exported: Exported, name: &str| {
            let mut init = Table::default();
            init.stars.push(binding("pkg/ledger.py", WHOLE_MODULE, 1));
            let mut ledger = defining(&["post", "draft", "_hidden"]);
            ledger.exported = exported;
            let tables = repo(vec![("pkg/__init__.py", init), ("pkg/ledger.py", ledger)]);
            Definitions::new(&tables)
                .of("pkg/__init__.py", name)
                .map(|d| d.file)
        };
        let ledger = Some("pkg/ledger.py".to_owned());
        // without `__all__`, every name without a leading `_`
        assert_eq!(through(Exported::Public, "draft"), ledger);
        assert_eq!(through(Exported::Public, "_hidden"), None);
        // with one, its names only
        let listed = Exported::Listed(BTreeSet::from(["post".to_owned()]));
        assert_eq!(through(listed.clone(), "post"), ledger);
        assert_eq!(through(listed, "draft"), None);
        // one built at runtime says nothing
        assert_eq!(through(Exported::Built, "post"), None);
    }

    #[test]
    fn bindings_the_walk_cannot_see_stop_it_before_star_sources() {
        // a name list that could not be read, or a star from outside the scan
        let mut init = binding_table(&[("pay", binding("pkg/charge.py", "pay", 2))]);
        init.opaque = true;
        init.stars.push(binding("pkg/ledger.py", WHOLE_MODULE, 1));
        let tables = repo(vec![
            ("pkg/__init__.py", init),
            ("pkg/ledger.py", defining(&["post"])),
            ("pkg/charge.py", defining(&["pay"])),
        ]);
        let mut definitions = Definitions::new(&tables);
        assert_eq!(definitions.of("pkg/__init__.py", "post"), None);
        // a binding it shows still answers
        assert_eq!(
            definitions.of("pkg/__init__.py", "pay"),
            Some(definition("pkg/charge.py", ("pkg/__init__.py", 2), "pay"))
        );
    }

    #[test]
    fn a_binding_and_a_star_source_must_agree() {
        // the later statement wins when the program runs, whichever it is
        let init = |other: Table| {
            let mut init = binding_table(&[("post", binding("pkg/ledger.py", "post", 2))]);
            init.stars.push(binding("pkg/other.py", WHOLE_MODULE, 1));
            repo(vec![
                ("pkg/__init__.py", init),
                ("pkg/ledger.py", defining(&["post"])),
                ("pkg/other.py", other),
            ])
        };
        let tables = init(defining(&["post"]));
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "post"),
            None
        );
        let tables = init(binding_table(&[(
            "post",
            binding("pkg/ledger.py", "post", 1),
        )]));
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "post"),
            Some(definition("pkg/ledger.py", ("pkg/__init__.py", 2), "post"))
        );
    }

    #[test]
    fn a_definition_and_a_binding_of_one_name_say_nothing() {
        // from .x import pay / pay = wrap(pay)
        let mut init = binding_table(&[("pay", binding("pkg/x.py", "pay", 1))]);
        init.defined.insert("pay".into());
        let tables = repo(vec![
            ("pkg/__init__.py", init),
            ("pkg/x.py", defining(&["pay"])),
        ]);
        let mut definitions = Definitions::new(&tables);
        assert_eq!(definitions.of("pkg/__init__.py", "pay"), None);

        // if TYPE_CHECKING: from .a import Foo / else: Foo = Any; only the
        // assignment runs, so the file defines Foo
        let mut lazy = binding("pkg/a.py", "Foo", 2);
        lazy.type_only = true;
        let mut init = binding_table(&[("Foo", lazy)]);
        init.defined.insert("Foo".into());
        let tables = repo(vec![
            ("pkg/__init__.py", init),
            ("pkg/a.py", defining(&["Foo"])),
            (
                "mid/__init__.py",
                binding_table(&[("Foo", binding("pkg/__init__.py", "Foo", 1))]),
            ),
        ]);
        let mut definitions = Definitions::new(&tables);
        assert_eq!(definitions.of("pkg/__init__.py", "Foo"), None);
        assert_eq!(
            definitions.of("mid/__init__.py", "Foo"),
            Some(definition("pkg/__init__.py", ("mid/__init__.py", 1), "Foo"))
        );
    }

    #[test]
    fn a_name_bound_by_what_the_walk_does_not_follow_says_nothing() {
        // import shop.charge as pay, or from requests import pay
        let mut init = Table::default();
        init.unfollowed.insert("pay".into(), false);
        let tables = repo(vec![
            ("pkg/__init__.py", init),
            (
                "mid/__init__.py",
                binding_table(&[("pay", binding("pkg/__init__.py", "pay", 1))]),
            ),
        ]);
        let mut definitions = Definitions::new(&tables);
        assert_eq!(definitions.of("pkg/__init__.py", "pay"), None);
        assert_eq!(definitions.of("mid/__init__.py", "pay"), None);
    }

    #[test]
    fn a_star_source_that_cannot_tell_stops_the_walk() {
        // from .b import * / from .a import *, where b.py defines x
        let tables = |a: Option<Table>| {
            let mut init = Table::default();
            init.stars.push(binding("pkg/b.py", WHOLE_MODULE, 1));
            init.stars.push(binding("pkg/a.py", WHOLE_MODULE, 2));
            let mut files = vec![
                ("pkg/__init__.py", init),
                ("pkg/b.py", defining(&["x"])),
                ("pkg/fast.py", defining(&["x"])),
                ("pkg/slow.py", defining(&["x"])),
            ];
            files.extend(a.map(|a| ("pkg/a.py", a)));
            repo(files)
        };
        // a.py binds x two ways
        let disagreeing = binding_table(&[
            ("x", binding("pkg/fast.py", "x", 2)),
            ("x", binding("pkg/slow.py", "x", 4)),
        ]);
        // a.py takes a module outside the scan whole
        let opaque = Table {
            opaque: true,
            ..Table::default()
        };
        // a.py could not be read
        for a in [Some(disagreeing), Some(opaque), None] {
            let tables = tables(a);
            assert_eq!(Definitions::new(&tables).of("pkg/__init__.py", "x"), None);
        }
    }

    #[test]
    fn star_sources_pass_a_type_only_only_together() {
        // if TYPE_CHECKING: from .a import * / from .b import *, where a.py
        // and b.py both take x from common.py
        let mut init = Table::default();
        let mut lazy = binding("pkg/a.py", WHOLE_MODULE, 3);
        lazy.type_only = true;
        init.stars.push(lazy);
        init.stars.push(binding("pkg/b.py", WHOLE_MODULE, 4));
        let common = || binding_table(&[("x", binding("pkg/common.py", "x", 1))]);
        let tables = repo(vec![
            ("pkg/__init__.py", init),
            ("pkg/a.py", common()),
            ("pkg/b.py", common()),
            ("pkg/common.py", defining(&["x"])),
        ]);
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "x"),
            Some(definition("pkg/common.py", ("pkg/__init__.py", 4), "x"))
        );
    }

    #[test]
    fn a_submodule_bound_whole_ends_the_walk() {
        let tables = repo(vec![
            (
                "pkg/__init__.py",
                binding_table(&[(
                    "billing",
                    binding("pkg/services/billing.py", WHOLE_MODULE, 1),
                )]),
            ),
            ("pkg/services/billing.py", defining(&["billing"])),
        ]);
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "billing"),
            Some(definition(
                "pkg/services/billing.py",
                ("pkg/__init__.py", 1),
                WHOLE_MODULE
            ))
        );
    }

    #[test]
    fn cycles_and_long_chains_give_up() {
        let tables = repo(vec![
            ("a.py", binding_table(&[("x", binding("b.py", "x", 1))])),
            ("b.py", binding_table(&[("x", binding("a.py", "x", 1))])),
        ]);
        assert_eq!(Definitions::new(&tables).of("a.py", "x"), None);

        let files: Vec<String> = (0..=MAX_HOPS + 1).map(|i| format!("m{i}.py")).collect();
        let mut chain: Vec<(&str, Table)> = files
            .windows(2)
            .map(|w| {
                (
                    w[0].as_str(),
                    binding_table(&[("x", binding(&w[1], "x", 1))]),
                )
            })
            .collect();
        chain.push((files.last().unwrap(), defining(&["x"])));
        assert_eq!(Definitions::new(&repo(chain)).of("m0.py", "x"), None);
    }

    #[test]
    fn a_binding_under_type_checking_passes_a_type_only() {
        let mut lazy = binding("pkg/charge.py", "Charge", 4);
        lazy.type_only = true;
        let tables = repo(vec![
            ("pkg/__init__.py", binding_table(&[("Charge", lazy)])),
            ("pkg/charge.py", defining(&["Charge"])),
        ]);
        let found = Definitions::new(&tables).of("pkg/__init__.py", "Charge");
        assert_eq!(found.map(|d| d.type_only), Some(true));

        // one binding that runs is enough for the name to run, and its line
        // is the one the evidence names
        let mut lazy = binding("pkg/charge.py", "Charge", 4);
        lazy.type_only = true;
        let mut runs = binding_table(&[("Charge", lazy)]);
        runs.bound
            .get_mut("Charge")
            .unwrap()
            .push(binding("pkg/charge.py", "Charge", 6));
        let tables = repo(vec![
            ("pkg/__init__.py", runs),
            ("pkg/charge.py", defining(&["Charge"])),
        ]);
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "Charge"),
            Some(definition(
                "pkg/charge.py",
                ("pkg/__init__.py", 6),
                "Charge"
            ))
        );
    }

    #[test]
    fn deep_diamonds_of_star_imports_are_walked_once() {
        // each level has two files that both take both files of the next
        // level whole: every path is walked only once its end is known
        let levels = 30;
        let file = |level: usize, i: usize| format!("m{level}_{i}.py");
        let mut files: Vec<(String, Table)> = Vec::new();
        for level in 0..levels {
            for i in 0..2 {
                let mut table = Table::default();
                for j in 0..2 {
                    table
                        .stars
                        .push(binding(&file(level + 1, j), WHOLE_MODULE, 1 + j as u32));
                }
                files.push((file(level, i), table));
            }
        }
        for i in 0..2 {
            files.push((
                file(levels, i),
                binding_table(&[("x", binding("end.py", "x", 1))]),
            ));
        }
        files.push(("end.py".to_owned(), defining(&["x"])));
        let tables: BTreeMap<String, Table> = files.into_iter().collect();
        assert_eq!(
            Definitions::new(&tables)
                .of(&file(0, 0), "x")
                .map(|d| d.file),
            Some("end.py".to_owned())
        );
    }

    #[test]
    fn only_what_runs_decides_when_anything_does() {
        let lazy = |file: &str, name: &str, line| Binding {
            type_only: true,
            ..binding(file, name, line)
        };
        // if TYPE_CHECKING: from ._types import Foo / else: from ._runtime import Foo
        let mut init = binding_table(&[("Foo", lazy("pkg/_types.py", "Foo", 2))]);
        init.bound
            .get_mut("Foo")
            .unwrap()
            .push(binding("pkg/_runtime.py", "Foo", 4));
        let tables = repo(vec![
            ("pkg/__init__.py", init),
            ("pkg/_types.py", defining(&["Foo"])),
            ("pkg/_runtime.py", defining(&["Foo"])),
        ]);
        assert_eq!(
            Definitions::new(&tables).of("pkg/__init__.py", "Foo"),
            Some(definition("pkg/_runtime.py", ("pkg/__init__.py", 4), "Foo"))
        );

        // if TYPE_CHECKING: from requests import Session / else: Session = None
        let mut init = Table::default();
        init.unfollowed.insert("Session".into(), true);
        init.defined.insert("Session".into());
        let tables = repo(vec![
            ("pkg/__init__.py", init),
            (
                "app/__init__.py",
                binding_table(&[("Session", binding("pkg/__init__.py", "Session", 1))]),
            ),
        ]);
        let mut definitions = Definitions::new(&tables);
        assert_eq!(definitions.of("pkg/__init__.py", "Session"), None);
        assert_eq!(
            definitions.of("app/__init__.py", "Session"),
            Some(definition(
                "pkg/__init__.py",
                ("app/__init__.py", 1),
                "Session"
            ))
        );
    }

    #[test]
    fn the_hop_limit_holds_whichever_walk_comes_first() {
        // m0 -> ... -> m20, which defines x, and p0 -> ... -> p25 -> m10
        let chain = |prefix: &str, len: usize, end: &str| -> Vec<(String, Table)> {
            (0..len)
                .map(|i| {
                    let next = if i + 1 == len {
                        end.to_owned()
                    } else {
                        format!("{prefix}{}.py", i + 1)
                    };
                    (
                        format!("{prefix}{i}.py"),
                        binding_table(&[("x", binding(&next, "x", 1))]),
                    )
                })
                .collect()
        };
        let mut files = chain("m", 20, "m20.py");
        files.push(("m20.py".to_owned(), defining(&["x"])));
        files.extend(chain("p", 26, "m10.py"));
        let tables: BTreeMap<String, Table> = files.into_iter().collect();
        // 36 bindings from p0, past the limit, even after a walk from m10
        // has found x ten bindings away
        let fresh = Definitions::new(&tables).of("p0.py", "x");
        let mut definitions = Definitions::new(&tables);
        assert!(definitions.of("m10.py", "x").is_some());
        assert_eq!(definitions.of("p0.py", "x"), fresh);
        assert_eq!(fresh, None);
    }

    #[test]
    fn what_a_cycle_leaves_is_walked_again() {
        // p.py: if TYPE_CHECKING: from pkg.n import X / from pkg.r import X;
        // n.py: from pkg.p import X; r.py defines X
        let mut p = binding_table(&[(
            "X",
            Binding {
                type_only: true,
                ..binding("pkg/n.py", "X", 2)
            },
        )]);
        p.bound
            .get_mut("X")
            .unwrap()
            .push(binding("pkg/r.py", "X", 3));
        let tables = repo(vec![
            ("pkg/p.py", p),
            (
                "pkg/n.py",
                binding_table(&[("X", binding("pkg/p.py", "X", 1))]),
            ),
            ("pkg/r.py", defining(&["X"])),
        ]);
        let fresh = Definitions::new(&tables).of("pkg/n.py", "X");
        assert_eq!(fresh, Some(definition("pkg/r.py", ("pkg/n.py", 1), "X")));
        // a walk from p.py first meets n.py inside the cycle
        let mut definitions = Definitions::new(&tables);
        assert!(definitions.of("pkg/p.py", "X").is_some());
        assert_eq!(definitions.of("pkg/n.py", "X"), fresh);
    }
}
