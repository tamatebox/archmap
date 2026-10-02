//! Where an imported name is defined when a file re-exports it from
//! another: each file's export table, and the walk through re-exports that
//! ECMAScript's ResolveExport does. A file's own exports come before its
//! `export *` sources, which must agree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::WHOLE_MODULE;

/// Re-export hops a walk follows before it gives up.
pub(crate) const MAX_HOPS: usize = 32;

/// What a file exports, by the name an import asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExportTable {
    pub names: BTreeMap<String, Export>,
    /// `export * from 'm'`: the statement's index among the file's import
    /// statements, its line, and whether it is `export type *`.
    pub stars: Vec<(usize, u32, bool)>,
    /// The name the file's own default export declares, which its symbol
    /// carries: `limitOf` for `export default function limitOf`. `None` for
    /// an anonymous or a re-exported default, or none at all.
    pub default_name: Option<String>,
}

impl ExportTable {
    /// What each import statement's names are exported as where that is
    /// another name, by the statement's index: a taken name (as the loaded
    /// file exports it) to the names this file exports it under, and
    /// [`WHOLE_MODULE`] to a namespace's names.
    pub(crate) fn renames(&self) -> BTreeMap<usize, BTreeMap<String, BTreeSet<String>>> {
        let mut found: BTreeMap<usize, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        for (exported, export) in &self.names {
            let (import, taken) = match export {
                Export::Reexport { import, name, .. } if name != exported => (*import, name),
                Export::Namespace { import, .. } => (*import, &WHOLE_MODULE.to_owned()),
                _ => continue,
            };
            found
                .entry(import)
                .or_default()
                .entry(taken.clone())
                .or_default()
                .insert(exported.clone());
        }
        found
    }
}

/// Where an exported name comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Export {
    /// Declared in the file.
    Local,
    /// The export `name` of the module that import statement `import`
    /// loads, re-exported at `line`, as a type only when `type_only`
    /// (`export type { a } from`, or a binding imported as a type).
    Reexport {
        import: usize,
        name: String,
        line: u32,
        type_only: bool,
    },
    /// Declared in the file under another name, exported at `line` as
    /// this one: `export { formatPrice as fp }`.
    Alias { local: String, line: u32 },
    /// The whole module that import statement `import` loads, as a
    /// namespace, re-exported at `line`.
    Namespace {
        import: usize,
        line: u32,
        type_only: bool,
    },
}

/// What a walk sees of one parsed file.
#[derive(Debug, Clone, Default)]
pub(crate) struct Module {
    pub exports: ExportTable,
    /// The code file each import statement loads, by index; `None` when it
    /// loads no scanned code file (a package, a stylesheet, nothing).
    pub loads: Vec<Option<PathBuf>>,
}

/// The file that defines an imported name, reached through re-exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Definition {
    pub file: PathBuf,
    /// The first re-export on the way: its file and line.
    pub via: (PathBuf, u32),
    /// The name in `file`: a default export by the name its declaration
    /// gives (`default` without one), [`WHOLE_MODULE`] when a namespace
    /// re-export leads to the whole file.
    pub name: String,
    /// A re-export on the way passes the name on as a type only.
    pub type_only: bool,
}

/// Definitions of names as files export them. A (file, name) asked about
/// again is answered from memory; a walk may still pass the same inner
/// files for different names.
pub(crate) struct Definitions<'a> {
    index: Index<'a>,
    walked: BTreeMap<(PathBuf, String), Option<Definition>>,
}

impl<'a> Definitions<'a> {
    pub(crate) fn new(modules: &'a BTreeMap<PathBuf, Module>) -> Self {
        Definitions {
            index: Index::new(modules),
            walked: BTreeMap::new(),
        }
    }

    /// Where `name`, as `file` exports it, is defined, when `file`
    /// re-exports it. `None` when `file` declares it itself, when no export
    /// matches, when `export *` sources disagree, and at a cycle or past
    /// [`MAX_HOPS`].
    pub(crate) fn of(&mut self, file: &Path, name: &str) -> Option<Definition> {
        let key = (file.to_path_buf(), name.to_owned());
        if let Some(known) = self.walked.get(&key) {
            return known.clone();
        }
        let found = match self.index.walk(file, name, 0, &mut BTreeMap::new()) {
            Walk::Found(found) => Some(found),
            Walk::Missing | Walk::Unknown => None,
        }
        .and_then(|f| {
            let via = f.via?;
            let name = self.index.declared(&f.file, f.name);
            Some(Definition {
                file: f.file,
                via,
                name,
                type_only: f.type_only,
            })
        });
        self.walked.insert(key, found.clone());
        found
    }

    /// The name a statement that takes `name` from `file` records: `name`
    /// itself, except that a default export goes by the name its
    /// declaration in `file` gives. A default that `file` re-exports, whole
    /// or not, stays `default`: `file` declares no name for it, and the
    /// statement's `via` evidence names it as the defining file does.
    pub(crate) fn recorded(&self, file: &Path, name: &str) -> String {
        self.index.declared(file, name.to_owned())
    }
}

/// What a walk for a name ends with.
#[derive(Clone)]
enum Walk {
    Found(Found),
    /// The file exports no such name, or the name leads back to where the
    /// walk is.
    Missing,
    /// The file exports the name, but where it is defined is not known:
    /// `export *` sources disagree, the name leads outside the scan, or the
    /// walk gives up past [`MAX_HOPS`]. A sibling `export *` then answers
    /// nothing either.
    Unknown,
}

/// A name found: the file that declares it, the first re-export on the
/// way, if any, and the name in that file.
#[derive(Clone)]
struct Found {
    file: PathBuf,
    via: Option<(PathBuf, u32)>,
    name: String,
    /// A re-export on the way is a type only.
    type_only: bool,
}

/// The modules, and for each one with `export *` the star entries whose
/// source can export a name, by name. A walk enters only those, so a barrel
/// of many stars costs a visit per source that has the name, not per star.
struct Index<'a> {
    modules: &'a BTreeMap<PathBuf, Module>,
    providers: BTreeMap<&'a Path, BTreeMap<&'a str, Vec<usize>>>,
}

impl<'a> Index<'a> {
    fn new(modules: &'a BTreeMap<PathBuf, Module>) -> Self {
        let exportable = exportable(modules);
        let mut providers: BTreeMap<&Path, BTreeMap<&str, Vec<usize>>> = BTreeMap::new();
        for (file, module) in modules {
            for (star, (import, _, _)) in module.exports.stars.iter().enumerate() {
                let Some(source) = module.loads.get(*import).and_then(Option::as_deref) else {
                    continue;
                };
                for name in exportable.get(source).into_iter().flatten() {
                    if *name != "default" {
                        providers
                            .entry(file.as_path())
                            .or_default()
                            .entry(name)
                            .or_default()
                            .push(star);
                    }
                }
            }
        }
        Index { modules, providers }
    }

    /// `name` as `file` declares it: a default export by the name its
    /// declaration gives, when it gives one.
    fn declared(&self, file: &Path, name: String) -> String {
        if name != "default" {
            return name;
        }
        self.modules
            .get(file)
            .and_then(|m| m.exports.default_name.clone())
            .unwrap_or(name)
    }

    /// Where `name`, as `file` exports it, is defined. `walked` holds what
    /// each file and name led to, `None` while the walk is inside it, so a
    /// walk visits each once and a cycle ends.
    fn walk<'x>(
        &'x self,
        file: &'x Path,
        name: &'x str,
        hops: usize,
        walked: &mut BTreeMap<(&'x Path, &'x str), Option<Walk>>,
    ) -> Walk {
        if hops > MAX_HOPS {
            return Walk::Unknown;
        }
        // The keys borrow from the modules and the caller, so a visit does
        // not allocate.
        match walked.get(&(file, name)) {
            Some(Some(known)) => return known.clone(),
            Some(None) => return Walk::Missing,
            None => {}
        }
        walked.insert((file, name), None);
        let result = self.visit(file, name, hops, walked);
        walked.insert((file, name), Some(result.clone()));
        result
    }

    fn visit<'x>(
        &'x self,
        file: &'x Path,
        name: &'x str,
        hops: usize,
        walked: &mut BTreeMap<(&'x Path, &'x str), Option<Walk>>,
    ) -> Walk {
        let Some(module) = self.modules.get(file) else {
            return Walk::Missing;
        };
        let loaded = |import: usize| module.loads.get(import).and_then(Option::as_deref);
        let via = |line: u32| Some((file.to_path_buf(), line));
        match module.exports.names.get(name) {
            Some(Export::Local) => {
                return Walk::Found(Found {
                    file: file.to_path_buf(),
                    via: None,
                    name: name.to_owned(),
                    type_only: false,
                });
            }
            // the file's own declaration, by the name it declares, reached
            // through the specifier that renames it
            Some(Export::Alias { local, line }) => {
                return Walk::Found(Found {
                    file: file.to_path_buf(),
                    via: via(*line),
                    name: local.clone(),
                    type_only: false,
                });
            }
            Some(Export::Reexport {
                import,
                name: inner,
                line,
                type_only,
            }) => {
                let Some(next) = loaded(*import) else {
                    return Walk::Unknown;
                };
                return match self.walk(next, inner, hops + 1, walked) {
                    Walk::Found(found) => Walk::Found(Found {
                        file: found.file,
                        via: via(*line),
                        name: found.name,
                        type_only: *type_only || found.type_only,
                    }),
                    // the file says it exports the name all the same
                    Walk::Missing | Walk::Unknown => Walk::Unknown,
                };
            }
            Some(Export::Namespace {
                import,
                line,
                type_only,
            }) => {
                let Some(next) = loaded(*import) else {
                    return Walk::Unknown;
                };
                return Walk::Found(Found {
                    file: next.to_path_buf(),
                    via: via(*line),
                    name: WHOLE_MODULE.to_owned(),
                    type_only: *type_only,
                });
            }
            None => {}
        }
        // `export *` never re-exports a default export.
        if name == "default" {
            return Walk::Missing;
        }
        let candidates = self.providers.get(file).and_then(|by| by.get(name));
        let mut found: Option<Found> = None;
        for &star in candidates.into_iter().flatten() {
            let (import, line, star_type) = module.exports.stars[star];
            let Some(next) = loaded(import) else {
                continue;
            };
            let this = match self.walk(next, name, hops + 1, walked) {
                Walk::Found(this) => this,
                Walk::Missing => continue,
                Walk::Unknown => return Walk::Unknown,
            };
            match &found {
                None => {
                    found = Some(Found {
                        file: this.file,
                        via: via(line),
                        name: this.name,
                        type_only: star_type || this.type_only,
                    })
                }
                Some(first) if first.file == this.file => {}
                // The sources disagree: the name is ambiguous.
                Some(_) => return Walk::Unknown,
            }
        }
        found.map_or(Walk::Missing, Walk::Found)
    }
}

/// The names each module can export: its own entries and, through
/// `export *`, every name but `default` that its sources can export. Grown
/// to a fixed point, so a cycle of stars ends.
fn exportable(modules: &BTreeMap<PathBuf, Module>) -> BTreeMap<&Path, BTreeSet<&str>> {
    let mut names: BTreeMap<&Path, BTreeSet<&str>> = modules
        .iter()
        .map(|(file, m)| {
            (
                file.as_path(),
                m.exports.names.keys().map(String::as_str).collect(),
            )
        })
        .collect();
    loop {
        let mut changed = false;
        for (file, module) in modules {
            for (import, _, _) in &module.exports.stars {
                let Some(source) = module.loads.get(*import).and_then(Option::as_deref) else {
                    continue;
                };
                let (Some(from), Some(own)) = (names.get(source), names.get(file.as_path())) else {
                    continue;
                };
                let add: Vec<&str> = from
                    .iter()
                    .copied()
                    .filter(|name| *name != "default" && !own.contains(name))
                    .collect();
                if !add.is_empty() {
                    changed = true;
                    names.entry(file.as_path()).or_default().extend(add);
                }
            }
        }
        if !changed {
            return names;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module with `names`, `export *` statements `stars`, and the files
    /// its import statements load.
    fn module(names: &[(&str, Export)], stars: &[(usize, u32)], loads: &[Option<&str>]) -> Module {
        Module {
            exports: ExportTable {
                names: names
                    .iter()
                    .map(|(name, export)| ((*name).to_owned(), export.clone()))
                    .collect(),
                stars: stars.iter().map(|&(i, l)| (i, l, false)).collect(),
                default_name: None,
            },
            loads: loads.iter().map(|l| l.map(PathBuf::from)).collect(),
        }
    }

    fn modules<S: AsRef<str>>(
        list: impl IntoIterator<Item = (S, Module)>,
    ) -> BTreeMap<PathBuf, Module> {
        list.into_iter()
            .map(|(file, m)| (PathBuf::from(file.as_ref()), m))
            .collect()
    }

    fn reexport(import: usize, name: &str, line: u32) -> Export {
        Export::Reexport {
            import,
            name: name.to_owned(),
            line,
            type_only: false,
        }
    }

    /// The defining file, the re-export it was reached through, and that
    /// statement's line.
    fn definition(
        modules: &BTreeMap<PathBuf, Module>,
        file: &str,
        name: &str,
    ) -> Option<(String, String, u32)> {
        let shown = |p: &Path| p.to_string_lossy().into_owned();
        Definitions::new(modules)
            .of(Path::new(file), name)
            .map(|d| (shown(&d.file), shown(&d.via.0), d.via.1))
    }

    fn found(file: &str, via: &str, line: u32) -> Option<(String, String, u32)> {
        Some((file.to_owned(), via.to_owned(), line))
    }

    /// The name the defining file gives what `file` exports as `name`.
    fn defined_name(modules: &BTreeMap<PathBuf, Module>, file: &str, name: &str) -> Option<String> {
        Definitions::new(modules)
            .of(Path::new(file), name)
            .map(|d| d.name)
    }

    fn with_default(mut m: Module, name: &str) -> Module {
        m.exports.default_name = Some(name.to_owned());
        m
    }

    #[test]
    fn the_walk_returns_the_name_the_defining_file_declares() {
        let modules = modules([
            (
                "index.ts",
                module(
                    &[
                        ("b", reexport(0, "a", 3)),
                        ("limitOf", reexport(1, "default", 4)),
                        ("anon", reexport(2, "default", 5)),
                        (
                            "ns",
                            Export::Namespace {
                                import: 0,
                                line: 6,
                                type_only: false,
                            },
                        ),
                    ],
                    &[],
                    &[Some("mid.ts"), Some("limits.ts"), Some("anon.ts")],
                ),
            ),
            (
                "mid.ts",
                module(&[("a", reexport(0, "x", 7))], &[], &[Some("leaf.ts")]),
            ),
            ("leaf.ts", module(&[("x", Export::Local)], &[], &[])),
            (
                "limits.ts",
                with_default(module(&[("default", Export::Local)], &[], &[]), "limitOf"),
            ),
            ("anon.ts", module(&[("default", Export::Local)], &[], &[])),
        ]);
        assert_eq!(
            defined_name(&modules, "index.ts", "b").as_deref(),
            Some("x")
        );
        assert_eq!(
            defined_name(&modules, "index.ts", "limitOf").as_deref(),
            Some("limitOf")
        );
        assert_eq!(
            defined_name(&modules, "index.ts", "anon").as_deref(),
            Some("default")
        );
        assert_eq!(
            defined_name(&modules, "index.ts", "ns").as_deref(),
            Some("*")
        );
    }

    #[test]
    fn a_re_export_of_types_only_passes_its_names_on_as_types() {
        let typed = |import, name: &str, line| Export::Reexport {
            import,
            name: name.to_owned(),
            line,
            type_only: true,
        };
        let mut index = module(
            &[
                ("A", typed(0, "A", 2)),
                ("B", reexport(1, "B", 3)),
                ("D", reexport(3, "D", 4)),
                (
                    "ns",
                    Export::Namespace {
                        import: 1,
                        line: 5,
                        type_only: true,
                    },
                ),
            ],
            &[],
            &[
                Some("mid.ts"),
                Some("b.ts"),
                Some("star.ts"),
                Some("typed.ts"),
            ],
        );
        // `export type * from './star'`
        index.exports.stars = vec![(2, 6, true)];
        let modules = modules([
            ("index.ts", index),
            (
                "mid.ts",
                module(&[("A", reexport(0, "A", 1))], &[], &[Some("a.ts")]),
            ),
            (
                "typed.ts",
                module(&[("D", typed(0, "D", 1))], &[], &[Some("d.ts")]),
            ),
            ("a.ts", module(&[("A", Export::Local)], &[], &[])),
            ("b.ts", module(&[("B", Export::Local)], &[], &[])),
            ("d.ts", module(&[("D", Export::Local)], &[], &[])),
            ("star.ts", module(&[("C", Export::Local)], &[], &[])),
        ]);
        let mut definitions = Definitions::new(&modules);
        let mut type_only = |name: &str| {
            definitions
                .of(Path::new("index.ts"), name)
                .map(|d| d.type_only)
        };
        // a type at the first hop or a later one
        assert_eq!(type_only("A"), Some(true));
        assert_eq!(type_only("D"), Some(true));
        assert_eq!(type_only("ns"), Some(true));
        assert_eq!(type_only("C"), Some(true));
        assert_eq!(type_only("B"), Some(false));
    }

    #[test]
    fn stars_that_reach_one_file_agree_whatever_the_name() {
        // b.ts and c.ts both lead to leaf.ts under different names: two
        // names of one binding in valid code (`export { f as g }`), so the
        // first star's name stands
        let modules = modules([
            (
                "index.ts",
                module(&[], &[(0, 1), (1, 2)], &[Some("b.ts"), Some("c.ts")]),
            ),
            (
                "b.ts",
                module(&[("X", reexport(0, "a", 1))], &[], &[Some("leaf.ts")]),
            ),
            (
                "c.ts",
                module(&[("X", reexport(0, "b", 1))], &[], &[Some("leaf.ts")]),
            ),
            (
                "leaf.ts",
                module(&[("a", Export::Local), ("b", Export::Local)], &[], &[]),
            ),
        ]);
        assert_eq!(
            definition(&modules, "index.ts", "X"),
            found("leaf.ts", "index.ts", 1)
        );
        assert_eq!(
            defined_name(&modules, "index.ts", "X").as_deref(),
            Some("a")
        );
    }

    #[test]
    fn recorded_names_go_by_the_declaration() {
        let modules = modules([
            (
                "index.ts",
                module(
                    &[("default", reexport(0, "default", 1))],
                    &[],
                    &[Some("limits.ts")],
                ),
            ),
            (
                "limits.ts",
                with_default(module(&[("default", Export::Local)], &[], &[]), "limitOf"),
            ),
            ("anon.ts", module(&[("default", Export::Local)], &[], &[])),
            (
                "ns.ts",
                module(
                    &[(
                        "default",
                        Export::Namespace {
                            import: 0,
                            line: 2,
                            type_only: false,
                        },
                    )],
                    &[],
                    &[Some("limits.ts")],
                ),
            ),
        ]);
        let definitions = Definitions::new(&modules);
        let recorded = |file: &str, name: &str| definitions.recorded(Path::new(file), name);
        // the file's own default by its declared name; a default the file
        // re-exports, whole or not, and an anonymous one stay `default`:
        // the file declares no name for them
        assert_eq!(recorded("limits.ts", "default"), "limitOf");
        assert_eq!(recorded("index.ts", "default"), "default");
        assert_eq!(recorded("ns.ts", "default"), "default");
        assert_eq!(recorded("anon.ts", "default"), "default");
        // other names, and files that are no scanned code, stay as written
        assert_eq!(recorded("index.ts", "other"), "other");
        assert_eq!(recorded("data.json", "default"), "default");
    }

    #[test]
    fn a_name_the_file_declares_needs_no_walk() {
        let modules = modules([("a.ts", module(&[("a", Export::Local)], &[], &[]))]);
        assert_eq!(definition(&modules, "a.ts", "a"), None);
    }

    #[test]
    fn a_named_re_export_leads_to_the_declaring_file_through_the_first_hop() {
        let modules = modules([
            (
                "index.ts",
                module(&[("b", reexport(0, "a", 3))], &[], &[Some("mid.ts")]),
            ),
            (
                "mid.ts",
                module(&[("a", reexport(0, "x", 7))], &[], &[Some("leaf.ts")]),
            ),
            ("leaf.ts", module(&[("x", Export::Local)], &[], &[])),
        ]);
        assert_eq!(
            definition(&modules, "index.ts", "b"),
            found("leaf.ts", "index.ts", 3)
        );
    }

    #[test]
    fn a_default_export_travels_by_name_but_never_through_a_star() {
        let modules = modules([
            (
                "index.ts",
                module(
                    &[("limitOf", reexport(0, "default", 3))],
                    &[(1, 4)],
                    &[Some("limits.ts"), Some("other.ts")],
                ),
            ),
            ("limits.ts", module(&[("default", Export::Local)], &[], &[])),
            ("other.ts", module(&[("default", Export::Local)], &[], &[])),
        ]);
        assert_eq!(
            definition(&modules, "index.ts", "limitOf"),
            found("limits.ts", "index.ts", 3)
        );
        assert_eq!(definition(&modules, "index.ts", "default"), None);
    }

    #[test]
    fn a_star_source_that_cannot_answer_stops_the_walk() {
        let modules = modules([
            (
                "index.ts",
                module(&[], &[(0, 1), (1, 2)], &[Some("a.ts"), Some("f.ts")]),
            ),
            // `X` is ambiguous in a.ts, and `Y` leads outside the scan
            (
                "a.ts",
                module(
                    &[("Y", reexport(2, "Y", 3))],
                    &[(0, 1), (1, 2)],
                    &[Some("d.ts"), Some("e.ts"), None],
                ),
            ),
            ("d.ts", module(&[("X", Export::Local)], &[], &[])),
            ("e.ts", module(&[("X", Export::Local)], &[], &[])),
            (
                "f.ts",
                module(&[("X", Export::Local), ("Y", Export::Local)], &[], &[]),
            ),
        ]);
        // f.ts is no answer while a.ts may export the name too
        assert_eq!(definition(&modules, "index.ts", "X"), None);
        assert_eq!(definition(&modules, "index.ts", "Y"), None);
    }

    #[test]
    fn stars_are_searched_and_must_agree() {
        let modules = modules([
            (
                "index.ts",
                module(
                    &[],
                    &[(0, 1), (1, 2), (2, 3)],
                    &[Some("a.ts"), Some("b.ts"), Some("c.ts")],
                ),
            ),
            (
                "a.ts",
                module(&[("A", Export::Local), ("Both", Export::Local)], &[], &[]),
            ),
            ("b.ts", module(&[("Both", Export::Local)], &[], &[])),
            (
                "c.ts",
                module(&[("A", reexport(0, "A", 1))], &[], &[Some("a.ts")]),
            ),
        ]);
        // a.ts, directly and through c.ts: one declaring file
        assert_eq!(
            definition(&modules, "index.ts", "A"),
            found("a.ts", "index.ts", 1)
        );
        // a.ts and b.ts both declare it: ambiguous
        assert_eq!(definition(&modules, "index.ts", "Both"), None);
        assert_eq!(definition(&modules, "index.ts", "Missing"), None);
    }

    #[test]
    fn a_name_the_file_declares_shadows_its_stars() {
        // A Playwright fixture file: `export * from '@playwright/test'`
        // beside its own `export const test`. The package is no scanned
        // file, and `test` is the file's own.
        let modules = modules([(
            "e2e/fixtures.ts",
            module(&[("test", Export::Local)], &[(0, 1)], &[None]),
        )]);
        assert_eq!(definition(&modules, "e2e/fixtures.ts", "test"), None);
        assert_eq!(definition(&modules, "e2e/fixtures.ts", "expect"), None);
    }

    #[test]
    fn a_namespace_re_export_leads_to_the_whole_module() {
        let modules = modules([
            (
                "index.ts",
                module(
                    &[(
                        "money",
                        Export::Namespace {
                            import: 0,
                            line: 4,
                            type_only: false,
                        },
                    )],
                    &[],
                    &[Some("money.ts")],
                ),
            ),
            ("money.ts", module(&[], &[], &[])),
        ]);
        assert_eq!(
            definition(&modules, "index.ts", "money"),
            found("money.ts", "index.ts", 4)
        );
    }

    #[test]
    fn imports_of_files_that_are_no_scanned_code_lead_nowhere() {
        let modules = modules([(
            "index.ts",
            module(&[("a", reexport(0, "a", 1))], &[(1, 2)], &[None, None]),
        )]);
        assert_eq!(definition(&modules, "index.ts", "a"), None);
        assert_eq!(definition(&modules, "index.ts", "b"), None);
    }

    #[test]
    fn cycles_stop() {
        let modules = modules([
            ("a.ts", module(&[], &[(0, 1)], &[Some("b.ts")])),
            ("b.ts", module(&[], &[(0, 1)], &[Some("a.ts")])),
        ]);
        assert_eq!(definition(&modules, "a.ts", "x"), None);
    }

    /// `f0.ts` to `f<files - 1>.ts`, each re-exporting `x` from the next;
    /// the last one declares it.
    fn chain(files: usize) -> BTreeMap<PathBuf, Module> {
        modules((0..files).map(|i| {
            let next = format!("f{}.ts", i + 1);
            let m = if i + 1 == files {
                module(&[("x", Export::Local)], &[], &[])
            } else {
                module(&[("x", reexport(0, "x", 1))], &[], &[Some(next.as_str())])
            };
            (format!("f{i}.ts"), m)
        }))
    }

    #[test]
    fn a_walk_gives_up_after_the_hop_limit() {
        // f0 to f32 is 32 hops; f0 to f33 is one too many
        assert_eq!(
            definition(&chain(MAX_HOPS + 1), "f0.ts", "x"),
            found("f32.ts", "f0.ts", 1)
        );
        assert_eq!(definition(&chain(MAX_HOPS + 2), "f0.ts", "x"), None);
    }

    #[test]
    fn each_name_is_walked_once() {
        let modules = chain(3);
        let mut definitions = Definitions::new(&modules);
        let first = definitions.of(Path::new("f0.ts"), "x");
        assert!(first.is_some());
        assert_eq!(definitions.of(Path::new("f0.ts"), "x"), first);
        assert_eq!(definitions.walked.len(), 1);
    }

    #[test]
    fn a_wide_star_barrel_is_searched_only_where_a_name_can_be() {
        // 1,500 `export *` sources of one name each, every name asked for:
        // a walk that tried every source per name would make 2.25 million
        // visits (generated API clients have barrels like this).
        let n = 1_500;
        let names: Vec<String> = (0..n).map(|i| format!("C{i}")).collect();
        let files: Vec<String> = (0..n).map(|i| format!("c{i}.ts")).collect();
        let mut list: Vec<(String, Module)> = (0..n)
            .map(|i| {
                (
                    files[i].clone(),
                    module(&[(names[i].as_str(), Export::Local)], &[], &[]),
                )
            })
            .collect();
        let stars: Vec<(usize, u32)> = (0..n).map(|i| (i, i as u32 + 1)).collect();
        let loads: Vec<Option<&str>> = files.iter().map(|f| Some(f.as_str())).collect();
        list.push(("index.ts".to_owned(), module(&[], &stars, &loads)));
        let modules = modules(list);
        let started = std::time::Instant::now();
        let mut definitions = Definitions::new(&modules);
        for (i, name) in names.iter().enumerate() {
            let definition = definitions.of(Path::new("index.ts"), name).unwrap();
            assert_eq!(definition.file, PathBuf::from(&files[i]));
            assert_eq!(definition.via, (PathBuf::from("index.ts"), i as u32 + 1));
        }
        let elapsed = started.elapsed();
        assert!(elapsed < std::time::Duration::from_secs(1), "{elapsed:?}");
    }
}
