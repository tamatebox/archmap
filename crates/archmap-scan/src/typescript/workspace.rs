//! The packages a monorepo links by name, as an install links them under
//! `node_modules`: the members its workspaces name (`package.json`
//! `workspaces`, `pnpm-workspace.yaml`) and the directories that `file:`,
//! `link:` and `portal:` dependencies point at.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component as PathComponent, Path, PathBuf};

use globset::{GlobSet, GlobSetBuilder};

use super::package::PackageJson;

/// The packages an install links by name, by the directory whose
/// `node_modules` holds the links: a workspace root links the members its
/// patterns name, the first of a name by path, and a package the
/// directories its `file:` dependencies point at. Code reaches the nearest
/// link of a name above it, as Node looks, so two workspaces in one
/// checkout keep their members apart. A package that no workspace names and
/// no path dependency points at is never linked, so a copied example cannot
/// take over a dependency's name. Paths are relative to the root.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Links(BTreeMap<PathBuf, BTreeMap<String, PathBuf>>);

impl Links {
    /// The directory of the package `name` that code in `dir` reaches.
    pub(crate) fn find(&self, dir: &Path, name: &str) -> Option<&Path> {
        dir.ancestors()
            .find_map(|d| self.0.get(d)?.get(name))
            .map(PathBuf::as_path)
    }

    /// Every link: the directory whose `node_modules` holds it, the name and
    /// the package's directory.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Path, &str, &Path)> {
        self.0.iter().flat_map(|(at, names)| {
            names
                .iter()
                .map(move |(name, dir)| (at.as_path(), name.as_str(), dir.as_path()))
        })
    }

    /// The directories of every linked package.
    pub(crate) fn members(&self) -> BTreeSet<PathBuf> {
        self.iter().map(|(_, _, dir)| dir.to_path_buf()).collect()
    }
}

/// Links at the root, by name, as a single workspace makes them.
#[cfg(test)]
impl From<BTreeMap<String, PathBuf>> for Links {
    fn from(names: BTreeMap<String, PathBuf>) -> Self {
        Links(BTreeMap::from([(PathBuf::new(), names)]))
    }
}

pub(crate) fn links(
    manifests: &BTreeMap<PathBuf, PackageJson>,
    pnpm: &BTreeMap<PathBuf, Vec<String>>,
) -> Links {
    let roots = manifests
        .iter()
        .filter(|(_, m)| !m.workspace_patterns.is_empty())
        .map(|(dir, m)| (dir, &m.workspace_patterns))
        .chain(pnpm.iter());
    let mut links: BTreeMap<PathBuf, BTreeMap<String, PathBuf>> = BTreeMap::new();
    for (root, patterns) in roots {
        let (include, exclude) = globs(patterns);
        for (dir, manifest) in manifests {
            let (Some(name), Ok(below)) = (&manifest.name, dir.strip_prefix(root)) else {
                continue;
            };
            if below.as_os_str().is_empty() || !include.is_match(below) || exclude.is_match(below) {
                continue;
            }
            links
                .entry(root.clone())
                .or_default()
                .entry(name.clone())
                .or_insert_with(|| dir.clone());
        }
    }
    for (dir, manifest) in manifests {
        for declaration in &manifest.declarations {
            let Some(path) = &declaration.path else {
                continue;
            };
            let Some(target) = normalize(&dir.join(path)) else {
                continue;
            };
            if manifests.contains_key(&target) {
                links
                    .entry(dir.clone())
                    .or_default()
                    .entry(declaration.name.clone())
                    .or_insert(target);
            }
        }
    }
    Links(links)
}

/// The patterns that pick members and those (`!`) that leave them out.
fn globs(patterns: &[String]) -> (GlobSet, GlobSet) {
    let (mut include, mut exclude) = (GlobSetBuilder::new(), GlobSetBuilder::new());
    for pattern in patterns {
        let (set, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (&mut exclude, rest),
            None => (&mut include, pattern.as_str()),
        };
        let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
        // `*` stays within one directory, as workspaces read it
        if let Ok(glob) = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
        {
            set.add(glob);
        }
    }
    let build = |set: GlobSetBuilder| set.build().unwrap_or_else(|_| GlobSet::empty());
    (build(include), build(exclude))
}

/// The `packages:` list of a `pnpm-workspace.yaml`, none when it has none;
/// `Err` with the parser's message for a file that is no YAML.
pub(crate) fn pnpm_patterns(text: &str) -> Result<Vec<String>, String> {
    let documents = yaml_rust2::YamlLoader::load_from_str(text).map_err(|e| e.to_string())?;
    Ok(documents
        .first()
        .and_then(|document| document["packages"].as_vec())
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect())
}

/// `path` with `.` and `..` resolved lexically; `None` when it leaves the
/// root, where a directory of the same name is another one.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            PathComponent::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            PathComponent::CurDir => {}
            PathComponent::Normal(name) => out.push(name),
            PathComponent::RootDir | PathComponent::Prefix(_) => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typescript::package::{Declaration, Section};

    fn named(name: &str) -> PackageJson {
        PackageJson {
            name: Some(name.to_owned()),
            ..PackageJson::default()
        }
    }

    #[test]
    fn members_and_path_dependencies_are_linked_by_name() {
        let mut root = named("mono");
        root.workspaces = true;
        root.workspace_patterns = vec![
            "packages/*".into(),
            "apps/**".into(),
            "!packages/skip".into(),
        ];
        let mut web = named("web");
        web.declarations.push(Declaration {
            name: "local-lib".into(),
            section: Section::Dependencies,
            line: Some(3),
            path: Some("../../libs/local".into()),
            workspace: false,
            version: None,
        });
        let manifests: BTreeMap<PathBuf, PackageJson> = [
            ("", root),
            ("packages/ui", named("@acme/ui")),
            ("packages/skip", named("skip")),
            ("packages/ui/deep", named("deep")),
            ("apps/web", web),
            ("apps/web/site", named("site")),
            ("libs/local", named("local-lib")),
            ("examples/react", named("react")),
        ]
        .into_iter()
        .map(|(dir, m)| (PathBuf::from(dir), m))
        .collect();
        let links = links(&manifests, &BTreeMap::new());
        let found: Vec<(&str, &str, &str)> = links
            .iter()
            .map(|(at, name, dir)| (at.to_str().unwrap(), name, dir.to_str().unwrap()))
            .collect();
        assert_eq!(
            found,
            [
                // the workspace root links its members
                ("", "@acme/ui", "packages/ui"),
                ("", "site", "apps/web/site"),
                ("", "web", "apps/web"),
                // a package links its path dependencies
                ("apps/web", "local-lib", "libs/local"),
            ]
        );
        assert_eq!(
            links.find(Path::new("apps/web/src"), "local-lib"),
            Some(Path::new("libs/local"))
        );
        assert_eq!(links.find(Path::new("packages/ui/src"), "local-lib"), None);
    }

    #[test]
    fn a_path_dependency_outside_the_root_links_nothing() {
        let mut web = named("web");
        web.declarations.push(Declaration {
            name: "shared".into(),
            section: Section::Dependencies,
            line: Some(3),
            // `shared/` beside the checkout, not the one inside it
            path: Some("../../../shared".into()),
            workspace: false,
            version: None,
        });
        let manifests: BTreeMap<PathBuf, PackageJson> =
            [("apps/web", web), ("shared", named("shared"))]
                .into_iter()
                .map(|(dir, m)| (PathBuf::from(dir), m))
                .collect();
        assert_eq!(links(&manifests, &BTreeMap::new()), Links::default());
    }

    #[test]
    fn pnpm_reads_comments_and_flow_lists() {
        let block = "packages:\n  - 'packages/*' # libraries\n  - apps/* # apps\n  - \"a#b/*\"\n";
        assert_eq!(
            pnpm_patterns(block).unwrap(),
            ["packages/*", "apps/*", "a#b/*"]
        );
        let flow = "packages: ['a/*', \"b/{c,d}\"] # all\ncatalog:\n  react: ^19\n";
        assert_eq!(pnpm_patterns(flow).unwrap(), ["a/*", "b/{c,d}"]);
        let lines = "packages: [\n  'a/*', # first\n  b/*\n]\ncatalog: {}\n";
        assert_eq!(pnpm_patterns(lines).unwrap(), ["a/*", "b/*"]);
        // YAML that a reader by lines misses: an anchor, a key indented
        let anchored = "defaults: &all\n  - 'libs/*'\npackages: *all\n";
        assert_eq!(pnpm_patterns(anchored).unwrap(), ["libs/*"]);
        // no list: no members
        assert_eq!(
            pnpm_patterns("catalog:\n  react: ^19\n").unwrap(),
            Vec::<String>::new()
        );
        // a file that is no YAML says why
        assert!(pnpm_patterns("packages: [\n  'a/*'\n").is_err());
    }

    #[test]
    fn pnpm_lists_its_packages() {
        let text = "packages:\n  - 'packages/*'\n  - \"apps/*\"\n  # tests\n  - '!**/test/**'\n\
                    catalog:\n  react: ^19\n";
        assert_eq!(
            pnpm_patterns(text).unwrap(),
            ["packages/*", "apps/*", "!**/test/**"]
        );
        let manifests: BTreeMap<PathBuf, PackageJson> = [("packages/a", named("a"))]
            .into_iter()
            .map(|(dir, m)| (PathBuf::from(dir), m))
            .collect();
        let pnpm = BTreeMap::from([(PathBuf::new(), pnpm_patterns(text).unwrap())]);
        assert_eq!(
            links(&manifests, &pnpm),
            Links::from(BTreeMap::from([(
                "a".to_owned(),
                PathBuf::from("packages/a")
            )]))
        );
    }
}
