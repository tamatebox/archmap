//! Where an imported name is defined when a file re-exports it from
//! another: each file's export table, and the walk through re-exports that
//! ECMAScript's ResolveExport does. A file's own exports come before its
//! `export *` sources, which must agree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Re-export hops a walk follows before it gives up.
pub(crate) const MAX_HOPS: usize = 32;

/// What a file exports, by the name an import asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExportTable {
    pub names: BTreeMap<String, Export>,
    /// `export * from 'm'`: the statement's index among the file's import
    /// statements, and its line.
    pub stars: Vec<(usize, u32)>,
}

/// Where an exported name comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Export {
    /// Declared in the file.
    Local,
    /// The export `name` of the module that import statement `import`
    /// loads, re-exported at `line`.
    Reexport {
        import: usize,
        name: String,
        line: u32,
    },
    /// The whole module that import statement `import` loads, as a
    /// namespace, re-exported at `line`.
    Namespace { import: usize, line: u32 },
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
        let found = self
            .index
            .walk(file, name, 0, &mut BTreeSet::new())
            .and_then(|f| f.via.map(|via| Definition { file: f.file, via }));
        self.walked.insert(key, found.clone());
        found
    }
}

/// A name found: the file that declares it, and the first re-export on the
/// way, if any.
struct Found {
    file: PathBuf,
    via: Option<(PathBuf, u32)>,
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
            for (star, (import, _)) in module.exports.stars.iter().enumerate() {
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

    fn walk<'x>(
        &'x self,
        file: &'x Path,
        name: &'x str,
        hops: usize,
        seen: &mut BTreeSet<(&'x Path, &'x str)>,
    ) -> Option<Found> {
        // The pairs borrow from the modules and the caller, so a visit does
        // not allocate.
        if hops > MAX_HOPS || !seen.insert((file, name)) {
            return None;
        }
        let module = self.modules.get(file)?;
        let loaded = |import: usize| module.loads.get(import).and_then(Option::as_deref);
        let via = |line: u32| Some((file.to_path_buf(), line));
        match module.exports.names.get(name) {
            Some(Export::Local) => {
                return Some(Found {
                    file: file.to_path_buf(),
                    via: None,
                });
            }
            Some(Export::Reexport {
                import,
                name: inner,
                line,
            }) => {
                let next = loaded(*import)?;
                let found = self.walk(next, inner, hops + 1, seen)?;
                return Some(Found {
                    file: found.file,
                    via: via(*line),
                });
            }
            Some(Export::Namespace { import, line }) => {
                let next = loaded(*import)?;
                return Some(Found {
                    file: next.to_path_buf(),
                    via: via(*line),
                });
            }
            None => {}
        }
        // `export *` never re-exports a default export.
        if name == "default" {
            return None;
        }
        let candidates = self.providers.get(file).and_then(|by| by.get(name));
        let mut found: Option<Found> = None;
        for &star in candidates.into_iter().flatten() {
            let (import, line) = module.exports.stars[star];
            let Some(next) = loaded(import) else {
                continue;
            };
            let Some(this) = self.walk(next, name, hops + 1, seen) else {
                continue;
            };
            match &found {
                None => {
                    found = Some(Found {
                        file: this.file,
                        via: via(line),
                    })
                }
                Some(first) if first.file == this.file => {}
                // The sources disagree: the name is ambiguous.
                Some(_) => return None,
            }
        }
        found
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
            for (import, _) in &module.exports.stars {
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
                stars: stars.to_vec(),
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
                    &[("money", Export::Namespace { import: 0, line: 4 })],
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
