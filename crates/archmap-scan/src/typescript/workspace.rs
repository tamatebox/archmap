//! The packages a monorepo links by name, as an install links them under
//! `node_modules`: the members its workspaces name (`package.json`
//! `workspaces`, `pnpm-workspace.yaml`) and the directories that `file:`,
//! `link:` and `portal:` dependencies point at.

use std::collections::BTreeMap;
use std::path::{Component as PathComponent, Path, PathBuf};

use globset::{GlobSet, GlobSetBuilder};

use super::package::PackageJson;

/// Package name to its directory, relative to the root. The first member
/// of a name by path wins; a package that no workspace names and no path
/// dependency points at is never linked, so a copied example cannot take
/// over a dependency's name.
pub(crate) fn links(
    manifests: &BTreeMap<PathBuf, PackageJson>,
    pnpm: &BTreeMap<PathBuf, Vec<String>>,
) -> BTreeMap<String, PathBuf> {
    let roots = manifests
        .iter()
        .filter(|(_, m)| !m.workspace_patterns.is_empty())
        .map(|(dir, m)| (dir, &m.workspace_patterns))
        .chain(pnpm.iter());
    let mut links: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (root, patterns) in roots {
        let (include, exclude) = globs(patterns);
        for (dir, manifest) in manifests {
            let (Some(name), Ok(below)) = (&manifest.name, dir.strip_prefix(root)) else {
                continue;
            };
            if below.as_os_str().is_empty() || !include.is_match(below) || exclude.is_match(below) {
                continue;
            }
            links.entry(name.clone()).or_insert_with(|| dir.clone());
        }
    }
    for (dir, manifest) in manifests {
        for declaration in &manifest.declarations {
            let Some(path) = &declaration.path else {
                continue;
            };
            let target = normalize(&dir.join(path));
            if manifests.contains_key(&target) {
                links.entry(declaration.name.clone()).or_insert(target);
            }
        }
    }
    links
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

/// The `packages:` list of a `pnpm-workspace.yaml`, read line by line.
pub(crate) fn pnpm_patterns(text: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t', '-']) {
            inside = trimmed == "packages:";
            continue;
        }
        if let (true, Some(item)) = (inside, trimmed.strip_prefix('-')) {
            let item = item.trim().trim_matches(|c| c == '\'' || c == '"');
            if !item.is_empty() {
                patterns.push(item.to_owned());
            }
        }
    }
    patterns
}

/// `path` with `.` and `..` resolved lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            PathComponent::ParentDir => {
                out.pop();
            }
            PathComponent::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
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
        let found: Vec<(&str, &str)> = links
            .iter()
            .map(|(name, dir)| (name.as_str(), dir.to_str().unwrap()))
            .collect();
        assert_eq!(
            found,
            [
                ("@acme/ui", "packages/ui"),
                ("local-lib", "libs/local"),
                ("site", "apps/web/site"),
                ("web", "apps/web"),
            ]
        );
    }

    #[test]
    fn pnpm_lists_its_packages() {
        let text = "packages:\n  - 'packages/*'\n  - \"apps/*\"\n  # tests\n  - '!**/test/**'\n\
                    catalog:\n  react: ^19\n";
        assert_eq!(pnpm_patterns(text), ["packages/*", "apps/*", "!**/test/**"]);
        let manifests: BTreeMap<PathBuf, PackageJson> = [("packages/a", named("a"))]
            .into_iter()
            .map(|(dir, m)| (PathBuf::from(dir), m))
            .collect();
        let pnpm = BTreeMap::from([(PathBuf::new(), pnpm_patterns(text))]);
        assert_eq!(
            links(&manifests, &pnpm),
            BTreeMap::from([("a".to_owned(), PathBuf::from("packages/a"))])
        );
    }
}
