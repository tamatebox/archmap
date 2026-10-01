//! Where TS/JS code sits: which `package.json` makes a package, the source
//! root of each package, the directory and file components below it, and
//! the component that owns each code file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{Component, ComponentId, ComponentKind, Evidence};

use super::language::{is_code, language_of, JAVASCRIPT, LANGUAGE};
use super::package::PackageJson;
use crate::context::display_path;

/// A package, or the root component of code that no package owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Package {
    pub id: ComponentId,
    /// Relative directory; empty for the repository root.
    pub dir: PathBuf,
    /// `<dir>/src` when the package has one, otherwise `dir`.
    pub source_root: PathBuf,
    /// The directory of its `package.json`. The root component of code
    /// that no package owns has the root's `package.json` without a name,
    /// when there is one.
    pub manifest: Option<PathBuf>,
    pub language: &'static str,
    /// The names a bare specifier uses for the package's own code when an
    /// alias the scan does not read resolves it (see [`local_names`]).
    pub local_names: BTreeSet<String>,
    /// Those of `local_names` at the top of the source root, the ones a
    /// bundler alias such as `@components/` reaches.
    pub source_names: BTreeSet<String>,
}

/// The component that owns a code file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Owner {
    pub component: ComponentId,
    /// Index into [`Layout::packages`].
    pub package: usize,
    /// For a file of the package itself other than an `index.*`: its file
    /// name, which goes between the package id and a symbol name.
    pub symbol_scope: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Layout {
    pub packages: Vec<Package>,
    /// Directory and file components.
    pub modules: Vec<Component>,
    /// Every code file and its owner.
    pub owners: BTreeMap<PathBuf, Owner>,
    /// Packages that share a name with an earlier one by path: the name,
    /// the directory that keeps it, and the one that took `<name>+<dir>`.
    pub renamed: Vec<(String, PathBuf, PathBuf)>,
}

/// Lay out `code` (TS/JS files, relative to the root) by the `package.json`
/// files in `manifests`, keyed by directory. `files` are all scanned files,
/// sorted as [`RepoContext::files`](crate::RepoContext::files) gives them,
/// for the names of each package's own directories and files.
pub(crate) fn discover(
    code: &[&Path],
    manifests: &BTreeMap<PathBuf, PackageJson>,
    members: &BTreeSet<PathBuf>,
    files: &[PathBuf],
    root_name: &str,
) -> Layout {
    // The closest named manifest above each file, if any.
    let mut owned: BTreeMap<Option<&PathBuf>, Vec<&Path>> = BTreeMap::new();
    for file in code {
        let manifest = file.ancestors().skip(1).find_map(|dir| {
            manifests
                .get_key_value(dir)
                .filter(|(_, m)| m.name.is_some())
                .map(|(dir, _)| dir)
        });
        owned.entry(manifest).or_default().push(file);
    }

    // The packages: those with code, workspace roots, and the members
    // (and path dependencies) a dependency names whatever files they hold,
    // a JSON or config package included. Of packages that share a name, the
    // member keeps it, then the first by path; the others become
    // `<name>+<dir>`.
    let made: Vec<(&PathBuf, &String, &[&Path])> = manifests
        .iter()
        .filter_map(|(dir, manifest)| {
            let name = manifest.name.as_ref()?;
            let own = owned.get(&Some(dir)).map(Vec::as_slice).unwrap_or_default();
            (!own.is_empty() || manifest.workspaces || members.contains(dir))
                .then_some((dir, name, own))
        })
        .collect();
    let mut named: BTreeMap<&str, &PathBuf> = BTreeMap::new();
    let mut ids: BTreeMap<&PathBuf, ComponentId> = BTreeMap::new();
    let mut renamed = Vec::new();
    let (first, rest): (Vec<_>, Vec<_>) = made.iter().partition(|(dir, ..)| members.contains(*dir));
    for (dir, name, _) in first.into_iter().chain(rest) {
        let id = match named.get(name.as_str()) {
            Some(kept) => {
                renamed.push(((*name).clone(), (*kept).clone(), (*dir).clone()));
                ComponentId::new(format!("{name}+{}", display_path(dir)))
            }
            None => {
                named.insert(name, dir);
                ComponentId::new(name.as_str())
            }
        };
        ids.insert(dir, id);
    }
    renamed.sort();
    let mut packages = Vec::new();
    let mut index: BTreeMap<Option<&PathBuf>, usize> = BTreeMap::new();
    for (dir, _, own) in &made {
        index.insert(Some(dir), packages.len());
        packages.push(package(
            ids[dir].clone(),
            dir,
            Some((*dir).clone()),
            own,
            files,
        ));
    }
    if let Some(own) = owned.get(&None) {
        index.insert(None, packages.len());
        packages.push(package(
            ComponentId::new(root_name),
            Path::new(""),
            manifests.contains_key(Path::new("")).then(PathBuf::new),
            own,
            files,
        ));
    }

    let mut layout = Layout {
        packages,
        renamed,
        ..Layout::default()
    };
    let mut dirs: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut indexes: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let mut module_files: Vec<(&Path, usize)> = Vec::new();
    for (manifest, own) in &owned {
        let p = index[manifest];
        let package = &layout.packages[p];
        for file in own {
            let dir = file.parent().unwrap_or(Path::new(""));
            let owner = if package.source_root != package.dir && dir == package.dir.as_path() {
                Owner {
                    component: package.id.clone(),
                    package: p,
                    symbol_scope: Some(file_name(file)),
                }
            } else if is_index(file) && dir == package.source_root.as_path() {
                Owner {
                    component: package.id.clone(),
                    package: p,
                    symbol_scope: None,
                }
            } else if is_index(file) {
                indexes
                    .entry(dir.to_path_buf())
                    .or_insert_with(|| file.to_path_buf());
                add_dirs(dir, package, p, &mut dirs);
                Owner {
                    component: module_id(package, dir),
                    package: p,
                    symbol_scope: None,
                }
            } else {
                add_dirs(dir, package, p, &mut dirs);
                module_files.push((file, p));
                Owner {
                    component: module_id(package, file),
                    package: p,
                    symbol_scope: None,
                }
            };
            layout.owners.insert(file.to_path_buf(), owner);
        }
    }

    // Directories with a TS file anywhere below them in their package.
    let mut ts_dirs: BTreeSet<&Path> = BTreeSet::new();
    for (manifest, own) in &owned {
        let package = &layout.packages[index[manifest]];
        for file in own.iter().filter(|f| language_of(f) == Some(LANGUAGE)) {
            for dir in file.ancestors().skip(1) {
                if dir == package.dir.as_path()
                    || !dir.starts_with(&package.dir)
                    || !ts_dirs.insert(dir)
                {
                    break;
                }
            }
        }
    }
    let known: BTreeSet<PathBuf> = dirs.keys().cloned().collect();
    for (dir, p) in &dirs {
        let package = &layout.packages[*p];
        let mut c = Component::new(
            module_id(package, dir),
            module_name(package, dir),
            ComponentKind::Module,
        );
        c.path = Some(display_path(dir));
        c.parent = Some(parent_of(package, dir, &known));
        let language = match indexes.get(dir) {
            Some(index) => language_of(index).unwrap_or(LANGUAGE),
            None if ts_dirs.contains(dir.as_path()) => LANGUAGE,
            None => JAVASCRIPT,
        };
        c.language = Some(language.to_owned());
        c.evidence.push(match indexes.get(dir) {
            Some(index) => Evidence::new(display_path(index)).with_note("index"),
            None => Evidence::new(display_path(dir)).with_note("directory"),
        });
        layout.modules.push(c);
    }
    for (file, p) in module_files {
        let package = &layout.packages[p];
        let mut c = Component::new(
            module_id(package, file),
            module_name(package, file),
            ComponentKind::Module,
        );
        c.path = Some(display_path(file));
        c.parent = Some(parent_of(package, file, &known));
        c.language = language_of(file).map(str::to_owned);
        c.evidence
            .push(Evidence::new(display_path(file)).with_note("module"));
        layout.modules.push(c);
    }
    layout
}

impl Layout {
    /// The package whose directory holds `file`, the innermost one.
    pub(crate) fn package_holding(&self, file: &Path) -> Option<usize> {
        (0..self.packages.len())
            .filter(|&p| file.starts_with(&self.packages[p].dir))
            .max_by_key(|&p| self.packages[p].dir.components().count())
    }
}

fn package(
    id: ComponentId,
    dir: &Path,
    manifest: Option<PathBuf>,
    own: &[&Path],
    files: &[PathBuf],
) -> Package {
    let src = dir.join("src");
    let source_root = if own.iter().any(|f| f.starts_with(&src)) {
        src
    } else {
        dir.to_path_buf()
    };
    let language = if own.iter().any(|f| language_of(f) == Some(LANGUAGE)) {
        LANGUAGE
    } else {
        JAVASCRIPT
    };
    let (local_names, source_names) = local_names(dir, &source_root, files);
    Package {
        id,
        dir: dir.to_path_buf(),
        local_names,
        source_names,
        source_root,
        manifest,
        language,
    }
}

/// The directories at the top of the package directory and of its source
/// root, and the code files at the top of the source root by stem (`App`
/// for `App.tsx`), as (all of them, those of the source root). Files with
/// more than one extension are left out, and so are the files of a package
/// directory that has `src/`: they are config files, and their stems name
/// packages (`vite.config.ts`).
fn local_names(
    dir: &Path,
    source_root: &Path,
    files: &[PathBuf],
) -> (BTreeSet<String>, BTreeSet<String>) {
    let (mut names, mut source) = (BTreeSet::new(), BTreeSet::new());
    // `files` is sorted, so the files under `dir` are one run of it.
    let start = files.partition_point(|f| f.as_path() < dir);
    for file in files[start..].iter().take_while(|f| f.starts_with(dir)) {
        for base in [dir, source_root] {
            let Ok(rest) = file.strip_prefix(base) else {
                continue;
            };
            let mut parts = rest.components();
            let Some(first) = parts.next() else {
                continue;
            };
            let first = first.as_os_str().to_string_lossy();
            let name = if parts.next().is_some() {
                Some(first.into_owned())
            } else if base == source_root && is_code(file) {
                first
                    .split_once('.')
                    .filter(|(stem, extension)| !stem.is_empty() && !extension.contains('.'))
                    .map(|(stem, _)| stem.to_owned())
            } else {
                None
            };
            if let Some(name) = name {
                if base == source_root {
                    source.insert(name.clone());
                }
                names.insert(name);
            }
        }
    }
    (names, source)
}

/// `dir` and its ancestors below the package directory, except the source
/// root, as directory components of package `p`.
fn add_dirs(dir: &Path, package: &Package, p: usize, dirs: &mut BTreeMap<PathBuf, usize>) {
    for d in dir.ancestors() {
        if d == package.dir.as_path() || !d.starts_with(&package.dir) {
            break;
        }
        if d != package.source_root.as_path() {
            dirs.insert(d.to_path_buf(), p);
        }
    }
}

/// `<package id>::<path relative to the package directory>`.
fn module_id(package: &Package, path: &Path) -> ComponentId {
    let rest = path.strip_prefix(&package.dir).unwrap_or(path);
    ComponentId::new(format!("{}::{}", package.id, display_path(rest)))
}

/// The path from the source root, or from the package directory for code
/// outside the source root (`tests/helpers.ts`).
fn module_name(package: &Package, path: &Path) -> String {
    let base = if package.source_root != package.dir && path.starts_with(&package.source_root) {
        &package.source_root
    } else {
        &package.dir
    };
    display_path(path.strip_prefix(base).unwrap_or(path))
}

/// The closest directory component above `path`, else the package.
fn parent_of(package: &Package, path: &Path, dirs: &BTreeSet<PathBuf>) -> ComponentId {
    path.ancestors()
        .skip(1)
        .take_while(|d| *d != package.dir.as_path() && d.starts_with(&package.dir))
        .find(|d| dirs.contains(*d))
        .map_or_else(|| package.id.clone(), |d| module_id(package, d))
}

fn file_name(file: &Path) -> String {
    file.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `index.ts`, `index.tsx`, `index.d.ts`, `index.js` ...: the file a
/// directory import loads, its directory's own file.
fn is_index(file: &Path) -> bool {
    let name = file_name(file);
    let stem = name
        .strip_suffix(".d.ts")
        .or_else(|| name.rsplit_once('.').map(|(stem, _)| stem));
    stem == Some("index")
}

/// Test, story and mock files: their imports count, their exports are no
/// public interface. Helpers in `tests/` or `__tests__/` keep their
/// symbols.
pub(crate) fn is_test_file(file: &Path) -> bool {
    let name = file_name(file);
    [".test.", ".spec.", ".stories."]
        .iter()
        .any(|marker| name.contains(marker))
        || file.components().any(|c| c.as_os_str() == "__mocks__")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: Option<&str>, workspaces: bool) -> PackageJson {
        PackageJson {
            name: name.map(str::to_owned),
            workspaces,
            ..PackageJson::default()
        }
    }

    fn lay_out(files: &[&str], manifests: &[(&str, PackageJson)]) -> Layout {
        let mut files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        files.sort();
        let code: Vec<&Path> = files
            .iter()
            .map(PathBuf::as_path)
            .filter(|f| language_of(f).is_some())
            .collect();
        let manifests: BTreeMap<PathBuf, PackageJson> = manifests
            .iter()
            .map(|(dir, m)| (PathBuf::from(dir), m.clone()))
            .collect();
        discover(&code, &manifests, &BTreeSet::new(), &files, "repo")
    }

    fn module<'a>(layout: &'a Layout, id: &str) -> &'a Component {
        layout
            .modules
            .iter()
            .find(|c| c.id.as_str() == id)
            .unwrap_or_else(|| panic!("no component {id}"))
    }

    #[test]
    fn packages_that_share_a_name_stay_apart() {
        // a copied example keeps the name of the package it copies
        let layout = lay_out(
            &[
                "examples/dup/package.json",
                "examples/dup/index.ts",
                "packages/dup/package.json",
                "packages/dup/src/index.ts",
                "packages/dup/src/a.ts",
            ],
            &[
                ("examples/dup", manifest(Some("dup"), false)),
                ("packages/dup", manifest(Some("dup"), false)),
            ],
        );
        let ids: Vec<&str> = layout.packages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["dup", "dup+packages/dup"]);
        // the files of each belong to their own package
        assert_eq!(
            layout.owners[Path::new("packages/dup/src/a.ts")]
                .component
                .as_str(),
            "dup+packages/dup::src/a.ts"
        );
        assert_eq!(
            layout.renamed,
            [(
                "dup".to_owned(),
                PathBuf::from("examples/dup"),
                PathBuf::from("packages/dup")
            )]
        );
    }

    #[test]
    fn a_workspace_member_keeps_its_name_over_a_package_that_sorts_first() {
        let mut files: Vec<PathBuf> = [
            "examples/dup/package.json",
            "examples/dup/index.ts",
            "packages/dup/package.json",
            "packages/dup/src/index.ts",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        files.sort();
        let code: Vec<&Path> = files
            .iter()
            .map(PathBuf::as_path)
            .filter(|f| language_of(f).is_some())
            .collect();
        let manifests: BTreeMap<PathBuf, PackageJson> = [
            ("examples/dup", manifest(Some("dup"), false)),
            ("packages/dup", manifest(Some("dup"), false)),
        ]
        .into_iter()
        .map(|(dir, m)| (PathBuf::from(dir), m))
        .collect();
        let members = BTreeSet::from([PathBuf::from("packages/dup")]);
        let layout = discover(&code, &manifests, &members, &files, "repo");
        let ids: Vec<&str> = layout.packages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["dup+examples/dup", "dup"]);
    }

    #[test]
    fn a_package_with_src_has_directories_files_and_files_of_its_own() {
        let layout = lay_out(
            &[
                "package.json",
                "next.config.ts",
                "src/index.ts",
                "src/lib/money.ts",
                "src/components/index.ts",
                "src/components/button.tsx",
                "src/app/(public)/[slug]/page.tsx",
                "src/assets/logo.svg",
                "tests/helpers.ts",
                "scripts/seed.mjs",
            ],
            &[("", manifest(Some("ts-shop"), false))],
        );
        assert_eq!(layout.packages.len(), 1);
        let package = &layout.packages[0];
        assert_eq!(package.id.as_str(), "ts-shop");
        assert_eq!(package.source_root, PathBuf::from("src"));
        assert_eq!(package.language, "typescript");

        let ids: BTreeSet<&str> = layout.modules.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            BTreeSet::from([
                "ts-shop::scripts",
                "ts-shop::scripts/seed.mjs",
                "ts-shop::src/app",
                "ts-shop::src/app/(public)",
                "ts-shop::src/app/(public)/[slug]",
                "ts-shop::src/app/(public)/[slug]/page.tsx",
                "ts-shop::src/components",
                "ts-shop::src/components/button.tsx",
                "ts-shop::src/lib",
                "ts-shop::src/lib/money.ts",
                "ts-shop::tests",
                "ts-shop::tests/helpers.ts",
            ])
        );
        let page = module(&layout, "ts-shop::src/app/(public)/[slug]/page.tsx");
        assert_eq!(page.name, "app/(public)/[slug]/page.tsx");
        assert_eq!(
            page.path.as_deref(),
            Some("src/app/(public)/[slug]/page.tsx")
        );
        assert_eq!(
            page.parent.as_ref().unwrap().as_str(),
            "ts-shop::src/app/(public)/[slug]"
        );
        assert_eq!(page.language.as_deref(), Some("typescript"));
        assert_eq!(
            module(&layout, "ts-shop::tests/helpers.ts").name,
            "tests/helpers.ts"
        );
        assert_eq!(
            module(&layout, "ts-shop::src/lib")
                .parent
                .as_ref()
                .unwrap()
                .as_str(),
            "ts-shop"
        );
        assert_eq!(
            module(&layout, "ts-shop::scripts").language.as_deref(),
            Some("javascript")
        );
        assert_eq!(
            module(&layout, "ts-shop::src/components").evidence[0].file,
            "src/components/index.ts"
        );

        let owner = |file: &str| layout.owners[Path::new(file)].clone();
        assert_eq!(
            owner("next.config.ts"),
            Owner {
                component: "ts-shop".into(),
                package: 0,
                symbol_scope: Some("next.config.ts".to_owned()),
            }
        );
        assert_eq!(
            owner("src/index.ts"),
            Owner {
                component: "ts-shop".into(),
                package: 0,
                symbol_scope: None,
            }
        );
        assert_eq!(
            owner("src/components/index.ts").component.as_str(),
            "ts-shop::src/components"
        );
        assert!(package.local_names.contains("components"));
        assert!(package.local_names.contains("tests"));
    }

    #[test]
    fn nested_packages_markers_and_code_outside_any_package() {
        let layout = lay_out(
            &[
                "a.ts",
                "web/package.json",
                "web/main.ts",
                "web/esm/package.json",
                "web/esm/x.js",
                "tools/package.json",
            ],
            &[
                ("web", manifest(Some("web"), false)),
                ("web/esm", manifest(None, false)),
                ("tools", manifest(Some("tools"), false)),
            ],
        );
        let ids: Vec<&str> = layout.packages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["web", "repo"]);
        assert_eq!(
            layout.owners[Path::new("a.ts")].component.as_str(),
            "repo::a.ts"
        );
        assert_eq!(
            layout.owners[Path::new("web/esm/x.js")].component.as_str(),
            "web::esm/x.js"
        );
        assert_eq!(
            module(&layout, "web::esm").language.as_deref(),
            Some("javascript")
        );
        assert_eq!(module(&layout, "repo::a.ts").name, "a.ts");
    }

    #[test]
    fn a_workspace_root_without_code_is_a_package() {
        let layout = lay_out(
            &[
                "package.json",
                "packages/ui/package.json",
                "packages/ui/src/index.ts",
            ],
            &[
                ("", manifest(Some("mono"), true)),
                ("packages/ui", manifest(Some("@acme/ui"), false)),
            ],
        );
        let ids: Vec<&str> = layout.packages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["mono", "@acme/ui"]);
        assert_eq!(
            layout.owners[Path::new("packages/ui/src/index.ts")]
                .component
                .as_str(),
            "@acme/ui"
        );
    }

    fn local_names(layout: &Layout) -> Vec<&str> {
        layout.packages[0]
            .local_names
            .iter()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn local_names_leave_out_config_files() {
        let layout = lay_out(
            &[
                "package.json",
                "vite.config.ts",
                "vitest.config.ts",
                "eslint.config.mjs",
                "next-env.d.ts",
                "src/App.tsx",
                "src/main.tsx",
                "src/components/button.tsx",
                "src/styles/app.css",
                "tests/helpers.ts",
            ],
            &[("", manifest(Some("web"), false))],
        );
        assert_eq!(
            local_names(&layout),
            ["App", "components", "main", "src", "styles", "tests"]
        );
        // without `src/`, the package directory is the source root
        let layout = lay_out(
            &["package.json", "App.js", "jest.config.js", "lib/x.js"],
            &[("", manifest(Some("cra"), false))],
        );
        assert_eq!(local_names(&layout), ["App", "lib"]);
    }

    #[test]
    fn a_src_directory_without_code_is_no_source_root() {
        let layout = lay_out(
            &[
                "package.json",
                "index.js",
                "lib.js",
                "src/lib.rs",
                "src/assets/logo.svg",
            ],
            &[("", manifest(Some("napi"), false))],
        );
        assert_eq!(layout.packages[0].source_root, PathBuf::from(""));
        assert_eq!(
            layout.owners[Path::new("lib.js")].component.as_str(),
            "napi::lib.js"
        );
    }

    #[test]
    fn test_files_follow_runner_conventions() {
        assert!(is_test_file(Path::new("src/money.test.ts")));
        assert!(is_test_file(Path::new("src/button.stories.tsx")));
        assert!(is_test_file(Path::new("tests/e2e/login.spec.ts")));
        assert!(is_test_file(Path::new("src/lib/__mocks__/money.ts")));
        assert!(!is_test_file(Path::new("tests/e2e/helpers.ts")));
        assert!(!is_test_file(Path::new("src/__tests__/fixtures.ts")));
        assert!(!is_test_file(Path::new("src/testing.ts")));
    }

    #[test]
    fn index_files() {
        assert!(is_index(Path::new("a/index.ts")));
        assert!(is_index(Path::new("a/index.d.ts")));
        assert!(is_index(Path::new("index.jsx")));
        assert!(!is_index(Path::new("a/indexes.ts")));
    }
}
