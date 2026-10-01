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

/// What a file's tsconfig says about whether TypeScript reads the file as a
/// module rather than a script.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModuleOptions {
    /// `module` is `node16`, `node18`, `node20` or `nodenext`, under which
    /// `"type": "module"` in the closest `package.json` makes a module.
    pub node: bool,
    /// `jsx` is `react-jsx` or `react-jsxdev`, which imports a runtime into
    /// every file with JSX.
    pub jsx_runtime: bool,
    /// `moduleDetection` is `force`: every file but a declaration file is a
    /// module.
    pub force: bool,
}

pub(crate) struct ImportResolver {
    root: PathBuf,
    view: ViewFs,
    /// Applies each file's tsconfig (`paths`, `baseUrl`, `references`).
    with_tsconfig: ResolverGeneric<ViewFs>,
    /// For files whose tsconfig the resolver cannot use.
    without_tsconfig: ResolverGeneric<ViewFs>,
    /// The same without the `types` condition, for a package whose types
    /// lead outside the scan.
    untyped_with_tsconfig: ResolverGeneric<ViewFs>,
    untyped_without_tsconfig: ResolverGeneric<ViewFs>,
}

impl ImportResolver {
    /// A resolver over `view` that also matches the `exports` conditions
    /// the tsconfigs turn on (`customConditions`).
    pub(crate) fn new(root: &Path, view: ViewFs, conditions: &[String]) -> Self {
        let resolver = |tsconfig: Option<TsconfigDiscovery>, types: bool| {
            ResolverGeneric::new_with_file_system(
                view.clone(),
                options(tsconfig, types, conditions),
            )
        };
        Self {
            root: root.to_path_buf(),
            with_tsconfig: resolver(Some(TsconfigDiscovery::Auto), true),
            without_tsconfig: resolver(None, true),
            untyped_with_tsconfig: resolver(Some(TsconfigDiscovery::Auto), false),
            untyped_without_tsconfig: resolver(None, false),
            view,
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
        let attempt = |with: &ResolverGeneric<ViewFs>,
                       without: &ResolverGeneric<ViewFs>,
                       problems: &mut BTreeSet<String>| {
            match with.resolve_file(&absolute, specifier) {
                Err(err) if is_tsconfig_problem(&err) => {
                    problems.insert(self.tsconfig_problem(&err));
                    without.resolve_file(&absolute, specifier)
                }
                other => other,
            }
        };
        let mut result = attempt(&self.with_tsconfig, &self.without_tsconfig, problems);
        // a linked package's `types` condition can lead to built
        // declarations outside the scan, where its source answers to the
        // next condition; other packages are never in the view
        if matches!(&result, Err(e) if !matches!(e, ResolveError::Builtin { .. }))
            && package_name(specifier).is_some_and(|p| self.view.links(p))
        {
            if let ok @ Ok(_) = attempt(
                &self.untyped_with_tsconfig,
                &self.untyped_without_tsconfig,
                problems,
            ) {
                result = ok;
            }
        }
        match result {
            Ok(found) => found
                .path()
                .strip_prefix(&self.root)
                .map_or(Resolved::NotFound, |rel| Resolved::File(rel.to_path_buf())),
            Err(ResolveError::Builtin { .. }) => Resolved::Builtin,
            Err(_) => Resolved::NotFound,
        }
    }

    /// What the tsconfig of `file` (relative to the root) says about module
    /// detection; nothing for a file without one, or with one the resolver
    /// cannot use.
    pub(crate) fn module_options(&self, file: &Path) -> ModuleOptions {
        let Ok(Some(tsconfig)) = self.with_tsconfig.find_tsconfig(self.root.join(file)) else {
            return ModuleOptions::default();
        };
        let options = &tsconfig.compiler_options;
        let one_of = |value: &Option<String>, names: &[&str]| {
            value
                .as_deref()
                .is_some_and(|v| names.iter().any(|n| v.eq_ignore_ascii_case(n)))
        };
        ModuleOptions {
            node: one_of(&options.module, &["node16", "node18", "node20", "nodenext"]),
            jsx_runtime: one_of(&options.jsx, &["react-jsx", "react-jsxdev"]),
            force: self.view.module_detection_forced(tsconfig.path()),
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

fn options(tsconfig: Option<TsconfigDiscovery>, types: bool, custom: &[String]) -> ResolveOptions {
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
        condition_names: custom
            .iter()
            .cloned()
            .chain(strings(if types {
                &["types", "import", "require", "node", "default"]
            } else {
                &["import", "require", "node", "default"]
            }))
            .collect(),
        builtin_modules: true,
        // NODE_PATH would make the graph depend on the environment.
        node_path: false,
        // The links of the view lead to the files of workspace members.
        symlinks: true,
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
pub(crate) struct Aliases(Vec<(String, String, PathBuf)>);

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
            let dir = rel.parent().unwrap_or(Path::new("")).to_path_buf();
            for pattern in paths
                .keys()
                .filter(|p| !p.is_empty() && !p.starts_with('*'))
            {
                found.push((pattern.clone(), display_path(rel), dir.clone()));
            }
        }
        Aliases(found)
    }

    /// The alias pattern `specifier` matches, written in `file`, and the
    /// config that declares it: one whose directory holds `file`, as a
    /// config covers the files below it, so another package's alias never
    /// hides an undeclared import.
    pub(crate) fn matching(&self, specifier: &str, file: &Path) -> Option<(&str, &str)> {
        self.0
            .iter()
            .filter(|(.., dir)| file.starts_with(dir))
            .find(|(pattern, ..)| match pattern.split_once('*') {
                Some((prefix, suffix)) => {
                    specifier.len() >= prefix.len() + suffix.len()
                        && specifier.starts_with(prefix)
                        && specifier.ends_with(suffix)
                }
                None => specifier == pattern,
            })
            .map(|(pattern, config, _)| (pattern.as_str(), config.as_str()))
    }
}

/// The `customConditions` that the scanned tsconfig and jsconfig files turn
/// on, sorted. A condition counts for every file, not only those its config
/// covers: oxc_resolver reads no `customConditions`, and one resolver serves
/// the whole scan.
pub(crate) fn custom_conditions(ctx: &RepoContext) -> Vec<String> {
    let mut found = BTreeSet::new();
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
        let conditions = value
            .pointer("/compilerOptions/customConditions")
            .and_then(|v| v.as_array());
        found.extend(
            conditions
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(str::to_owned)),
        );
    }
    found.into_iter().collect()
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
/// `react/jsx-runtime`, `@babel/core` for `@babel/core/lib/config`), or
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
    use std::collections::BTreeMap;

    #[test]
    fn package_names_of_bare_specifiers() {
        assert_eq!(package_name("react"), Some("react"));
        assert_eq!(package_name("next/cache"), Some("next"));
        assert_eq!(package_name("@babel/core"), Some("@babel/core"));
        assert_eq!(package_name("@babel/core/lib/x"), Some("@babel/core"));
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
        let resolver = ImportResolver::new(ctx.root(), ViewFs::new(&ctx, &mut warnings), &[]);
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

    #[test]
    fn tsconfig_options_say_how_typescript_reads_a_file() {
        let root = repo(
            "module-options",
            &[
                // one project: `module` and `jsx` come through `extends`,
                // `moduleDetection` from the file itself
                (
                    "app/tsconfig.json",
                    "{ \"extends\": \"./tsconfig.base.json\", \"compilerOptions\": \
                     { \"moduleDetection\": \"force\" } }",
                ),
                (
                    "app/tsconfig.base.json",
                    "{ \"compilerOptions\": { \"module\": \"NodeNext\", \"jsx\": \"react-jsx\" } }",
                ),
                ("app/a.ts", "const a = 1;\n"),
                // another: `moduleDetection` only through `extends`
                (
                    "web/tsconfig.json",
                    "{ \"extends\": [\"./other.json\", \"./base\"] }",
                ),
                (
                    "web/other.json",
                    "{ \"compilerOptions\": { \"moduleDetection\": \"auto\" } }",
                ),
                (
                    "web/base.json",
                    "{ \"compilerOptions\": { \"moduleDetection\": \"force\" } }",
                ),
                ("web/b.ts", "const b = 1;\n"),
                ("plain/c.js", "var c = 1;\n"),
            ],
        );
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let mut warnings = Vec::new();
        let resolver = ImportResolver::new(ctx.root(), ViewFs::new(&ctx, &mut warnings), &[]);
        assert_eq!(
            resolver.module_options(Path::new("app/a.ts")),
            ModuleOptions {
                node: true,
                jsx_runtime: true,
                force: true
            }
        );
        // the last entry of `extends` wins
        assert!(resolver.module_options(Path::new("web/b.ts")).force);
        assert_eq!(
            resolver.module_options(Path::new("plain/c.js")),
            ModuleOptions::default()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn linked_packages_resolve_by_name_to_their_files() {
        let root = repo(
            "linked",
            &[
                (
                    "package.json",
                    r#"{ "name": "mono", "workspaces": ["packages/*", "apps/*"] }"#,
                ),
                (
                    "packages/ui/package.json",
                    r#"{ "name": "@acme/ui", "exports": { ".": "./src/index.ts" } }"#,
                ),
                ("packages/ui/src/index.ts", "export const a = 1;\n"),
                (
                    "packages/types/package.json",
                    r#"{ "name": "@acme/types", "exports": { ".": { "types": "./dist/index.d.ts", "import": "./src/index.ts" } } }"#,
                ),
                ("packages/types/src/index.ts", "export const t = 1;\n"),
                (
                    "packages/core/package.json",
                    r#"{ "name": "@acme/core", "main": "./dist/index.js" }"#,
                ),
                ("packages/core/src/index.ts", "export const c = 1;\n"),
                ("apps/web/package.json", r#"{ "name": "web" }"#),
                ("apps/web/src/a.ts", "import { a } from '@acme/ui';\n"),
            ],
        );
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let links: BTreeMap<String, PathBuf> = [
            ("@acme/ui", "packages/ui"),
            ("@acme/types", "packages/types"),
            ("@acme/core", "packages/core"),
        ]
        .into_iter()
        .map(|(name, dir)| (name.to_owned(), PathBuf::from(dir)))
        .collect();
        let mut warnings = Vec::new();
        let resolver = ImportResolver::new(
            ctx.root(),
            ViewFs::new_linked(&ctx, &links, &mut warnings),
            &[],
        );
        let mut problems = BTreeSet::new();
        let mut file =
            |spec: &str| resolver.resolve(Path::new("apps/web/src/a.ts"), spec, &mut problems);
        assert_eq!(
            file("@acme/ui"),
            Resolved::File(PathBuf::from("packages/ui/src/index.ts"))
        );
        // `types` leads into dist/, which the scan does not hold: the next
        // condition answers
        assert_eq!(
            file("@acme/types"),
            Resolved::File(PathBuf::from("packages/types/src/index.ts"))
        );
        // an entry outside the scan resolves to no file
        assert_eq!(file("@acme/core"), Resolved::NotFound);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn custom_conditions_of_the_tsconfigs_choose_exports() {
        // a member points its source at a condition the tsconfig turns on
        let root = repo(
            "conditions",
            &[
                (
                    "tsconfig.json",
                    r#"{ "compilerOptions": { "customConditions": ["@acme/source"] } }"#,
                ),
                (
                    "packages/core/package.json",
                    r#"{ "name": "@acme/core", "exports": { ".": { "@acme/source": "./src/index.ts", "import": "./dist/index.js" } } }"#,
                ),
                ("packages/core/src/index.ts", "export const c = 1;\n"),
                ("apps/web/src/a.ts", "import { c } from '@acme/core';\n"),
            ],
        );
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let links = BTreeMap::from([("@acme/core".to_owned(), PathBuf::from("packages/core"))]);
        let mut warnings = Vec::new();
        let resolver = ImportResolver::new(
            ctx.root(),
            ViewFs::new_linked(&ctx, &links, &mut warnings),
            &custom_conditions(&ctx),
        );
        let mut problems = BTreeSet::new();
        let found = resolver.resolve(Path::new("apps/web/src/a.ts"), "@acme/core", &mut problems);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            found,
            Resolved::File(PathBuf::from("packages/core/src/index.ts"))
        );
    }

    /// Resolve `specifier` from `src/a.ts` in a repository of `files`, with
    /// the problems reported.
    fn resolve_in(name: &str, files: &[(&str, &str)], specifier: &str) -> (Resolved, Vec<String>) {
        let root = repo(name, files);
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let mut warnings = Vec::new();
        let resolver = ImportResolver::new(ctx.root(), ViewFs::new(&ctx, &mut warnings), &[]);
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
