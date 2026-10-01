//! Resolving import specifiers with `oxc_resolver` over the view of the
//! repository, and telling what a specifier is when it resolves to no
//! file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use oxc_resolver::{ResolveError, ResolveOptions, ResolverGeneric, TsconfigDiscovery};

use super::fs::ViewFs;
use crate::context::display_path;
use crate::RepoContext;

/// Where a specifier leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A scanned file, relative to the root.
    File(PathBuf),
    /// A Node built-in module (`node:fs`, `fs`, `crypto`).
    Builtin,
    /// No scanned file.
    NotFound,
}

pub(crate) struct ImportResolver {
    root: PathBuf,
    /// Applies each file's tsconfig (`paths`, `baseUrl`, `references`).
    with_tsconfig: ResolverGeneric<ViewFs>,
    /// For files whose tsconfig the resolver cannot use.
    without_tsconfig: ResolverGeneric<ViewFs>,
}

impl ImportResolver {
    pub(crate) fn new(root: &Path, view: ViewFs) -> Self {
        Self {
            root: root.to_path_buf(),
            with_tsconfig: ResolverGeneric::new_with_file_system(
                view.clone(),
                options(Some(TsconfigDiscovery::Auto)),
            ),
            without_tsconfig: ResolverGeneric::new_with_file_system(view, options(None)),
        }
    }

    /// Resolve `specifier` as `file` (relative to the root) writes it. A
    /// tsconfig the resolver cannot use is reported once in `problems`, and
    /// the file is resolved without one.
    pub(crate) fn resolve(
        &self,
        file: &Path,
        specifier: &str,
        problems: &mut BTreeSet<String>,
    ) -> Resolved {
        let absolute = self.root.join(file);
        let result = match self.with_tsconfig.resolve_file(&absolute, specifier) {
            Err(err) if is_tsconfig_problem(&err) => {
                problems.insert(self.tsconfig_problem(&err));
                self.without_tsconfig.resolve_file(&absolute, specifier)
            }
            other => other,
        };
        match result {
            Ok(found) => found
                .path()
                .strip_prefix(&self.root)
                .map_or(Resolved::NotFound, |rel| Resolved::File(rel.to_path_buf())),
            Err(ResolveError::Builtin { .. }) => Resolved::Builtin,
            Err(_) => Resolved::NotFound,
        }
    }

    /// The warning for a tsconfig the resolver cannot use, with paths
    /// relative to the root.
    fn tsconfig_problem(&self, err: &ResolveError) -> String {
        let what = match err {
            ResolveError::TsconfigLoadFailed { path, source } => match source.as_ref() {
                ResolveError::Json(json) => format!("{}: {}", path.display(), json.message),
                other => format!("{}: {other}", path.display()),
            },
            other => other.to_string(),
        };
        let root = format!("{}/", self.root.display());
        format!(
            "{}; its files are resolved without a tsconfig",
            what.replace(&root, "")
        )
    }
}

fn options(tsconfig: Option<TsconfigDiscovery>) -> ResolveOptions {
    let strings = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    ResolveOptions {
        tsconfig,
        // Code before declarations: `foo.js` beside `foo.d.ts` is what runs,
        // so it is what an import of `./foo` depends on.
        extensions: strings(&[
            ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".d.ts", ".json",
        ]),
        // ES module TypeScript writes `./foo.js` for `foo.ts`.
        extension_alias: vec![
            (".js".to_owned(), strings(&[".ts", ".tsx", ".js"])),
            (".jsx".to_owned(), strings(&[".tsx", ".jsx"])),
            (".mjs".to_owned(), strings(&[".mts", ".mjs"])),
            (".cjs".to_owned(), strings(&[".cts", ".cjs"])),
        ],
        condition_names: strings(&["types", "import", "require", "node", "default"]),
        builtin_modules: true,
        // NODE_PATH would make the graph depend on the environment.
        node_path: false,
        // The view has no links, and its root is canonical.
        symlinks: false,
        ..ResolveOptions::default()
    }
}

fn is_tsconfig_problem(err: &ResolveError) -> bool {
    matches!(
        err,
        ResolveError::TsconfigNotFound(_)
            | ResolveError::TsconfigLoadFailed { .. }
            | ResolveError::TsconfigCircularExtend(_)
            | ResolveError::TsconfigSelfReference(_)
    )
}

/// The `paths` aliases that the tsconfig and jsconfig files of the scan
/// declare, with the file of each. An import that matches one and resolves
/// to no file is an alias that leads nowhere here (a jsconfig the resolver
/// does not read, or a file outside the tsconfig), never an undeclared
/// package.
#[derive(Debug, Default)]
pub(crate) struct Aliases(Vec<(String, String)>);

impl Aliases {
    pub(crate) fn collect(ctx: &RepoContext) -> Self {
        let mut found = Vec::new();
        for rel in ctx.files().iter().filter(|f| is_config(f)) {
            let Ok(text) = ctx.read_to_string(rel) else {
                continue;
            };
            let mut text = text.trim_start_matches('\u{feff}').to_owned();
            if json_strip_comments::strip(&mut text).is_err() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let Some(paths) = value
                .pointer("/compilerOptions/paths")
                .and_then(|v| v.as_object())
            else {
                continue;
            };
            // `*` alone matches every bare name and would hide every
            // undeclared package.
            for pattern in paths
                .keys()
                .filter(|p| !p.is_empty() && !p.starts_with('*'))
            {
                found.push((pattern.clone(), display_path(rel)));
            }
        }
        Aliases(found)
    }

    /// The alias pattern `specifier` matches and the file that declares it.
    pub(crate) fn matching(&self, specifier: &str) -> Option<(&str, &str)> {
        self.0
            .iter()
            .find(|(pattern, _)| match pattern.split_once('*') {
                Some((prefix, suffix)) => {
                    specifier.len() >= prefix.len() + suffix.len()
                        && specifier.starts_with(prefix)
                        && specifier.ends_with(suffix)
                }
                None => specifier == pattern,
            })
            .map(|(pattern, file)| (pattern.as_str(), file.as_str()))
    }
}

/// `tsconfig.json`, `jsconfig.json`, `tsconfig.app.json` ...
fn is_config(file: &Path) -> bool {
    file.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        (n.starts_with("tsconfig") || n.starts_with("jsconfig")) && n.ends_with(".json")
    })
}

/// A relative or absolute path rather than a bare specifier.
pub(crate) fn is_path(specifier: &str) -> bool {
    specifier.starts_with('.') || specifier.starts_with('/')
}

/// The npm package a bare specifier names (`react` for
/// `react/jsx-runtime`, `@supabase/ssr` for `@supabase/ssr/server`), or
/// `None` when it cannot be a package name: a path, an alias such as `@/x`
/// or `~/x`, a subpath import `#x`, a URL or `virtual:x`.
pub(crate) fn package_name(specifier: &str) -> Option<&str> {
    let valid = |segment: &str| {
        !segment.is_empty()
            && !segment.starts_with('.')
            && !segment.starts_with('_')
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    };
    let mut parts = specifier.splitn(3, '/');
    let first = parts.next()?;
    match first.strip_prefix('@') {
        Some(scope) => {
            let name = parts.next()?;
            (valid(scope) && valid(name)).then(|| &specifier[..first.len() + 1 + name.len()])
        }
        None => valid(first).then_some(first),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::display_path;
    use crate::typescript::fs::tests::repo;
    use crate::{RepoContext, ScanOptions};

    #[test]
    fn package_names_of_bare_specifiers() {
        assert_eq!(package_name("react"), Some("react"));
        assert_eq!(package_name("next/cache"), Some("next"));
        assert_eq!(package_name("@supabase/ssr"), Some("@supabase/ssr"));
        assert_eq!(package_name("@supabase/ssr/dist/x"), Some("@supabase/ssr"));
        assert_eq!(package_name("lodash.debounce"), Some("lodash.debounce"));
        for not_a_package in [
            "./a",
            "../a",
            "/abs",
            "@/lib",
            "~/x",
            "#internal",
            "virtual:pwa",
            "https://esm.sh/react",
            "@scope",
            "",
        ] {
            assert_eq!(package_name(not_a_package), None, "{not_a_package}");
        }
    }

    #[test]
    fn specifiers_resolve_through_the_view() {
        let root = repo(
            "resolve",
            &[
                ("package.json", "{ \"name\": \"r\" }"),
                (
                    "tsconfig.json",
                    "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@/*\": \
                     [\"./src/*\"] } }, \"include\": [\"src\"] }",
                ),
                ("src/a.ts", "export const a = 1;\n"),
                ("src/lib/b.ts", "export const b = 1;\n"),
                ("src/lib/index.ts", "export * from './b';\n"),
                ("src/styles.css", ".x {}\n"),
                ("src/logo.svg", "<svg/>\n"),
                ("scripts/c.mjs", "export const c = 1;\n"),
            ],
        );
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let mut warnings = Vec::new();
        let resolver = ImportResolver::new(ctx.root(), ViewFs::new(&ctx, &mut warnings));
        let mut problems = BTreeSet::new();
        let mut file = |from: &str, specifier: &str| match resolver.resolve(
            Path::new(from),
            specifier,
            &mut problems,
        ) {
            Resolved::File(path) => display_path(&path),
            Resolved::Builtin => "builtin".to_owned(),
            Resolved::NotFound => "not found".to_owned(),
        };
        assert_eq!(file("src/a.ts", "./lib/b"), "src/lib/b.ts");
        assert_eq!(file("src/a.ts", "@/lib/b"), "src/lib/b.ts");
        assert_eq!(file("src/a.ts", "@/lib"), "src/lib/index.ts");
        assert_eq!(file("src/a.ts", "./lib/b.js"), "src/lib/b.ts");
        assert_eq!(file("src/a.ts", "./styles.css"), "src/styles.css");
        assert_eq!(file("src/a.ts", "./logo.svg?url"), "src/logo.svg");
        assert_eq!(file("src/a.ts", "node:fs"), "builtin");
        assert_eq!(file("src/a.ts", "crypto"), "builtin");
        assert_eq!(file("src/a.ts", "react"), "not found");
        // scripts/ is outside the tsconfig's `include`, so no alias applies
        assert_eq!(file("scripts/c.mjs", "@/lib/b"), "not found");
        assert_eq!(file("scripts/c.mjs", "../src/lib/b.ts"), "src/lib/b.ts");
        assert!(problems.is_empty(), "{problems:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Resolve `specifier` from `src/a.ts` in a repository of `files`, with
    /// the problems reported.
    fn resolve_in(name: &str, files: &[(&str, &str)], specifier: &str) -> (Resolved, Vec<String>) {
        let root = repo(name, files);
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let mut warnings = Vec::new();
        let resolver = ImportResolver::new(ctx.root(), ViewFs::new(&ctx, &mut warnings));
        let mut problems = BTreeSet::new();
        let resolved = resolver.resolve(Path::new("src/a.ts"), specifier, &mut problems);
        std::fs::remove_dir_all(&root).unwrap();
        let root = format!("{}", root.display());
        for problem in &problems {
            assert!(!problem.contains(&root), "absolute path in `{problem}`");
        }
        (resolved, problems.into_iter().collect())
    }

    #[test]
    fn a_tsconfig_that_is_no_json_is_reported_by_its_relative_path() {
        let (resolved, problems) = resolve_in(
            "badtsconfig",
            &[
                ("tsconfig.json", "{ \"compilerOptions\": { \"paths\": "),
                ("src/a.ts", "import { b } from './b';\n"),
                ("src/b.ts", "export const b = 1;\n"),
            ],
            "./b",
        );
        assert_eq!(resolved, Resolved::File(PathBuf::from("src/b.ts")));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("tsconfig.json: "), "{problems:?}");
        assert!(
            problems[0].ends_with("; its files are resolved without a tsconfig"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_js_file_wins_over_its_declaration_file() {
        let (resolved, _) = resolve_in(
            "dts",
            &[
                ("src/a.ts", "import { b } from './b';\n"),
                ("src/b.js", "export const b = 1;\n"),
                ("src/b.d.ts", "export declare const b: number;\n"),
            ],
            "./b",
        );
        assert_eq!(resolved, Resolved::File(PathBuf::from("src/b.js")));
    }

    #[test]
    fn a_solution_tsconfig_applies_the_paths_of_its_references() {
        // Vite's layout: `tsconfig.json` only lists the projects.
        let (resolved, problems) = resolve_in(
            "references",
            &[
                (
                    "tsconfig.json",
                    "{ \"files\": [], \"references\": [{ \"path\": \"./tsconfig.app.json\" }, \
                     { \"path\": \"./tsconfig.node.json\" }] }",
                ),
                (
                    "tsconfig.app.json",
                    "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@/*\": \
                     [\"./src/*\"] } }, \"include\": [\"src\"] }",
                ),
                (
                    "tsconfig.node.json",
                    "{ \"include\": [\"vite.config.ts\"] }",
                ),
                ("src/a.ts", "import { b } from '@/b';\n"),
                ("src/b.ts", "export const b = 1;\n"),
            ],
            "@/b",
        );
        assert_eq!(resolved, Resolved::File(PathBuf::from("src/b.ts")));
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_broken_package_json_does_not_stop_resolution() {
        let (resolved, problems) = resolve_in(
            "badpackage",
            &[
                ("package.json", "{ \"name\": "),
                ("src/a.ts", "import { b } from './b';\n"),
                ("src/b.ts", "export const b = 1;\n"),
            ],
            "./b",
        );
        assert_eq!(resolved, Resolved::File(PathBuf::from("src/b.ts")));
        assert!(problems.is_empty(), "{problems:?}");
    }
}
