//! What the resolver may see: the scanned files and their directories, and
//! each tsconfig as it would load from the scan alone. Nothing else exists
//! for it, no `node_modules`, no build output, no ignored file, so the
//! graph of a commit does not depend on whether it was installed or built.
//! A `package.json` that is no JSON is left out too: the resolver reads
//! the closest one for every import below it and would fail them all, and
//! the analyzer reports it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Component as PathComponent, Path, PathBuf};
use std::sync::Arc;

use oxc_resolver::{FileMetadata, FileSystem, ResolveError};

use crate::context::display_path;
use crate::RepoContext;

#[derive(Debug, Clone, Default)]
pub(crate) struct ViewFs(Arc<View>);

#[derive(Debug, Default)]
struct View {
    /// Absolute paths, under the canonical root.
    files: HashSet<PathBuf>,
    dirs: HashSet<PathBuf>,
    /// tsconfig files served without the `extends` entries the view cannot
    /// load, so that their own `paths` still apply.
    served: HashMap<PathBuf, String>,
    /// `<root>/node_modules/<name>` to the directory of the package it
    /// links, as an install links workspace members.
    links: HashMap<PathBuf, PathBuf>,
    root: PathBuf,
}

impl ViewFs {
    /// The view of `ctx` without links.
    #[cfg(test)]
    pub(crate) fn new(ctx: &RepoContext, warnings: &mut Vec<String>) -> Self {
        Self::new_linked(ctx, &BTreeMap::new(), warnings)
    }

    /// The view of `ctx`, with each package of `links`, a name and the
    /// package's directory relative to the root, linked as
    /// `<root>/node_modules/<name>`. A tsconfig `extends` that names no
    /// scanned file is left out and reported in `warnings`, once per tsconfig
    /// and entry.
    pub(crate) fn new_linked(
        ctx: &RepoContext,
        links: &BTreeMap<String, PathBuf>,
        warnings: &mut Vec<String>,
    ) -> Self {
        let root = ctx.root();
        let mut view = View {
            root: root.to_path_buf(),
            ..View::default()
        };
        for (name, dir) in links {
            let link = root.join("node_modules").join(name);
            for parent in link.ancestors().skip(1) {
                if !parent.starts_with(root) || !view.dirs.insert(parent.to_path_buf()) {
                    break;
                }
            }
            view.links.insert(link, root.join(dir));
        }
        view.dirs.insert(root.to_path_buf());
        for rel in ctx.files() {
            let file = root.join(rel);
            for dir in file.ancestors().skip(1) {
                if !dir.starts_with(root) || !view.dirs.insert(dir.to_path_buf()) {
                    break;
                }
            }
            if !is_broken_package_json(ctx, rel) {
                view.files.insert(file);
            }
        }
        for rel in ctx.files().iter().filter(|f| is_tsconfig(f)) {
            let Ok(text) = ctx.read_to_string(rel) else {
                continue;
            };
            let path = root.join(rel);
            if let Some((served, dropped)) = without_unloadable_extends(&text, &path, &view) {
                for entry in dropped {
                    warnings.push(format!(
                        "{}: extends `{entry}` is not in the scanned files; its options are \
                         not applied",
                        display_path(rel)
                    ));
                }
                view.served.insert(path, served);
            }
        }
        ViewFs(Arc::new(view))
    }
}

/// A `package.json` the analyzer cannot read either.
fn is_broken_package_json(ctx: &RepoContext, rel: &Path) -> bool {
    rel.file_name().is_some_and(|n| n == "package.json")
        && ctx
            .read_to_string(rel)
            .map_or(true, |text| super::package::parse(&text).is_err())
}

/// `tsconfig.json`, `tsconfig.app.json`, `tsconfig.base.json` ...
fn is_tsconfig(file: &Path) -> bool {
    file.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("tsconfig") && n.ends_with(".json"))
}

/// `text`, the tsconfig at `path`, without the `extends` entries that name
/// no scanned file, and those entries. `None` when every entry loads, or
/// when the text is no JSON object (the resolver reports that itself).
fn without_unloadable_extends(
    text: &str,
    path: &Path,
    view: &View,
) -> Option<(String, Vec<String>)> {
    let mut stripped = text.trim_start_matches('\u{feff}').to_owned();
    json_strip_comments::strip(&mut stripped).ok()?;
    let mut value: serde_json::Value = serde_json::from_str(&stripped).ok()?;
    let entries = extends_entries(&value)?;
    let object = value.as_object_mut()?;
    let dir = path.parent()?;
    let (kept, dropped): (Vec<String>, Vec<String>) = entries
        .into_iter()
        .partition(|entry| view.extended(dir, entry).is_some());
    if dropped.is_empty() {
        return None;
    }
    match kept.as_slice() {
        [] => {
            object.remove("extends");
        }
        [only] => {
            object.insert(
                "extends".to_owned(),
                serde_json::Value::String(only.clone()),
            );
        }
        _ => {
            let kept = kept.into_iter().map(serde_json::Value::String).collect();
            object.insert("extends".to_owned(), serde_json::Value::Array(kept));
        }
    }
    Some((value.to_string(), dropped))
}

/// The `extends` entries of a tsconfig, in order.
fn extends_entries(value: &serde_json::Value) -> Option<Vec<String>> {
    match value.get("extends")? {
        serde_json::Value::String(entry) => Some(vec![entry.clone()]),
        serde_json::Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
        ),
        _ => None,
    }
}

impl ViewFs {
    /// Whether the tsconfig at `path` sets `moduleDetection` to `force`,
    /// itself or through a tsconfig it extends that the view holds: its own
    /// setting wins, then the last of its `extends` entries.
    pub(crate) fn module_detection_forced(&self, path: &Path) -> bool {
        self.module_detection(path, 0) == Some(true)
    }

    fn module_detection(&self, path: &Path, depth: usize) -> Option<bool> {
        // `extends` cycles end here; the resolver reports them
        if depth > 8 {
            return None;
        }
        let mut text = self.read_to_string(path).ok()?;
        text = text.trim_start_matches('\u{feff}').to_owned();
        json_strip_comments::strip(&mut text).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        if let Some(setting) = value
            .pointer("/compilerOptions/moduleDetection")
            .and_then(|v| v.as_str())
        {
            return Some(setting.eq_ignore_ascii_case("force"));
        }
        let dir = path.parent()?;
        extends_entries(&value)?.iter().rev().find_map(|entry| {
            let target = self.0.extended(dir, entry)?;
            self.module_detection(&target, depth + 1)
        })
    }
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

impl ViewFs {
    /// Whether the view links the package `name` (a workspace member, a
    /// path dependency), the only packages whose files it holds.
    pub(crate) fn links(&self, name: &str) -> bool {
        self.0
            .links
            .contains_key(&self.0.root.join("node_modules").join(name))
    }
}

impl View {
    /// The tsconfig an `extends` entry names, when the view holds it: a
    /// path, or a file of a package the view links (a workspace member that
    /// holds configuration); other packages live in `node_modules`, which
    /// the view never shows.
    fn extended(&self, dir: &Path, entry: &str) -> Option<PathBuf> {
        let target = if entry.starts_with('.') || entry.starts_with('/') {
            normalize(&dir.join(entry))
        } else {
            self.through_link(&self.root.join("node_modules").join(entry))?
        };
        let mut with_json = target.clone().into_os_string();
        with_json.push(".json");
        [
            target.clone(),
            PathBuf::from(with_json),
            target.join("tsconfig.json"),
        ]
        .into_iter()
        .find(|candidate| self.files.contains(candidate))
    }

    /// `path` through the link it lies below, if any: what the linked
    /// directory holds there.
    fn through_link(&self, path: &Path) -> Option<PathBuf> {
        path.ancestors().find_map(|link| {
            let target = self.links.get(link)?;
            let rest = path.strip_prefix(link).ok()?;
            Some(target.join(rest))
        })
    }

    /// `path` as the view holds it: through a link, or as it is.
    fn real(&self, path: &Path) -> PathBuf {
        self.through_link(path)
            .unwrap_or_else(|| path.to_path_buf())
    }
}

fn not_in_view(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("not in the scanned files: {}", path.display()),
    )
}

impl FileSystem for ViewFs {
    fn new() -> Self {
        Self::default()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.read_to_string(path).map(String::into_bytes)
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let path = self.0.real(path);
        if let Some(text) = self.0.served.get(&path) {
            return Ok(text.clone());
        }
        if self.0.files.contains(&path) {
            std::fs::read_to_string(path)
        } else {
            Err(not_in_view(&path))
        }
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        let path = self.0.real(path);
        if self.0.files.contains(&path) {
            Ok(FileMetadata::new(true, false, false))
        } else if self.0.dirs.contains(&path) {
            Ok(FileMetadata::new(false, true, false))
        } else {
            Err(not_in_view(&path))
        }
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        if self.0.links.contains_key(path) {
            Ok(FileMetadata::new(false, false, true))
        } else {
            self.metadata(path)
        }
    }

    fn read_link(&self, path: &Path) -> Result<PathBuf, ResolveError> {
        match self.0.links.get(path) {
            Some(target) => Ok(target.clone()),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not a link: {}", path.display()),
            )
            .into()),
        }
    }

    /// Paths stay as the scan sees them, through the links of the view:
    /// the root is canonical already.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(self.0.real(path))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ScanOptions;

    /// A throwaway repository with `files`, canonicalized.
    pub(crate) fn repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("archmap-ts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (file, text) in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir.canonicalize().unwrap()
    }

    #[test]
    fn the_view_shows_scanned_files_and_their_directories_only() {
        let root = repo(
            "view",
            &[("src/a.ts", "export const a = 1;\n"), (".hidden/b.ts", "")],
        );
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let mut warnings = Vec::new();
        let view = ViewFs::new(&ctx, &mut warnings);
        assert!(view.read_to_string(&root.join("src/a.ts")).is_ok());
        assert!(view.metadata(&root.join("src")).is_ok());
        assert!(view.metadata(&root).is_ok());
        assert!(view.metadata(&root.join(".hidden/b.ts")).is_err());
        assert!(view.metadata(&root.join("node_modules")).is_err());
        assert!(warnings.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_extends_outside_the_scan_is_dropped_and_reported() {
        let root = repo(
            "extends",
            &[
                (
                    "tsconfig.base.json",
                    "{ \"compilerOptions\": { \"strict\": true } }",
                ),
                (
                    "tsconfig.json",
                    "{\n  // keep the local paths\n  \"extends\": [\"./tsconfig.base.json\", \
                     \"@tsconfig/node20/tsconfig.json\"],\n  \"compilerOptions\": { \"paths\": \
                     { \"@/*\": [\"./src/*\"] }, },\n}\n",
                ),
            ],
        );
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let mut warnings = Vec::new();
        let view = ViewFs::new(&ctx, &mut warnings);
        assert_eq!(
            warnings,
            [
                "tsconfig.json: extends `@tsconfig/node20/tsconfig.json` is not in the scanned \
              files; its options are not applied"
            ]
        );
        let served: serde_json::Value =
            serde_json::from_str(&view.read_to_string(&root.join("tsconfig.json")).unwrap())
                .unwrap();
        assert_eq!(served["extends"], "./tsconfig.base.json");
        assert_eq!(served["compilerOptions"]["paths"]["@/*"][0], "./src/*");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_extends_of_a_linked_package_loads() {
        // a config-only workspace member, linked by name
        let root = repo(
            "linked-extends",
            &[
                (
                    "packages/tsconfig/package.json",
                    "{ \"name\": \"@acme/tsconfig\" }",
                ),
                (
                    "packages/tsconfig/base.json",
                    "{ \"compilerOptions\": { \"strict\": true } }",
                ),
                (
                    "apps/web/tsconfig.json",
                    "{ \"extends\": [\"@acme/tsconfig/base.json\", \"@acme/tsconfig/react\"] }",
                ),
                ("packages/tsconfig/react.json", "{}"),
            ],
        );
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let links = BTreeMap::from([(
            "@acme/tsconfig".to_owned(),
            PathBuf::from("packages/tsconfig"),
        )]);
        let mut warnings = Vec::new();
        let view = ViewFs::new_linked(&ctx, &links, &mut warnings);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        // the resolver reads the member through the link
        let linked = root.join("node_modules/@acme/tsconfig/base.json");
        assert!(view.metadata(&linked).is_ok_and(|m| m.is_file()));
    }

    #[test]
    fn a_tsconfig_with_a_byte_order_mark_is_read() {
        let root = repo(
            "bom",
            &[(
                "tsconfig.json",
                "\u{feff}{ \"extends\": \"@tsconfig/node20/tsconfig.json\" }",
            )],
        );
        let ctx = RepoContext::load(&root, ScanOptions::default()).unwrap();
        let mut warnings = Vec::new();
        let view = ViewFs::new(&ctx, &mut warnings);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(view.0.served.contains_key(&root.join("tsconfig.json")));
    }
}
