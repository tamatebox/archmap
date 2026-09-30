//! Minimal Cargo.toml reader. Only the fields archmap needs.

use std::collections::BTreeMap;
use std::path::{Component as PathComponent, Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Normal,
    Build,
    Dev,
}

impl DependencyKind {
    /// The manifest section this kind comes from, for evidence notes.
    pub fn section(self) -> &'static str {
        match self {
            DependencyKind::Normal => "[dependencies]",
            DependencyKind::Build => "[build-dependencies]",
            DependencyKind::Dev => "[dev-dependencies]",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoDependency {
    /// Key in the dependency table (the name used in source).
    pub name: String,
    /// `package = "..."` rename target, when present.
    pub package: Option<String>,
    /// `path = "..."`, resolved relative to the repository root.
    pub path: Option<PathBuf>,
    /// `workspace = true`
    pub workspace: bool,
    pub kind: DependencyKind,
}

impl CargoDependency {
    /// Identifier used in `use` statements (`archmap-core` -> `archmap_core`).
    pub fn import_name(&self) -> String {
        self.name.replace('-', "_")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoPackage {
    pub name: String,
    /// `[lib] name`, when the library target is named differently from the
    /// package.
    pub lib_name: Option<String>,
    /// `Cargo.toml` path relative to the repository root.
    pub manifest_path: PathBuf,
    /// Directory containing the manifest, relative to the repository root.
    pub dir: PathBuf,
    pub dependencies: Vec<CargoDependency>,
}

impl CargoPackage {
    /// Name of the library crate in source (`archmap-core` -> `archmap_core`,
    /// or the `[lib] name`): what other crates write in `use`.
    pub fn crate_name(&self) -> String {
        self.lib_name
            .clone()
            .unwrap_or_else(|| self.name.replace('-', "_"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoWorkspace {
    pub dir: PathBuf,
    /// `[workspace.dependencies]`, keyed by dependency name.
    pub dependencies: BTreeMap<String, CargoDependency>,
}

/// A manifest may declare a package, a workspace, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedManifest {
    pub package: Option<CargoPackage>,
    pub workspace: Option<CargoWorkspace>,
}

#[derive(Deserialize)]
struct RawManifest {
    package: Option<RawPackage>,
    lib: Option<RawLib>,
    workspace: Option<RawWorkspace>,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
    #[serde(default, rename = "build-dependencies")]
    build_dependencies: BTreeMap<String, RawDependency>,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: BTreeMap<String, RawDependency>,
}

#[derive(Deserialize)]
struct RawPackage {
    name: String,
}

#[derive(Deserialize)]
struct RawLib {
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawWorkspace {
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawDependency {
    /// `dep = "1.0"`; the version string itself is not an architectural fact.
    Version(#[allow(dead_code)] String),
    Detailed(RawDetailedDependency),
}

#[derive(Deserialize)]
struct RawDetailedDependency {
    package: Option<String>,
    path: Option<String>,
    #[serde(default)]
    workspace: bool,
}

/// Parse a manifest. `manifest_path` is relative to the repository root.
pub fn parse_manifest(text: &str, manifest_path: &Path) -> Result<ParsedManifest, toml::de::Error> {
    let raw: RawManifest = toml::from_str(text)?;
    let dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let convert =
        |table: &BTreeMap<String, RawDependency>, kind: DependencyKind| -> Vec<CargoDependency> {
            table
                .iter()
                .map(|(name, raw)| match raw {
                    RawDependency::Version(_) => CargoDependency {
                        name: name.clone(),
                        package: None,
                        path: None,
                        workspace: false,
                        kind,
                    },
                    RawDependency::Detailed(d) => CargoDependency {
                        name: name.clone(),
                        package: d.package.clone(),
                        path: d.path.as_deref().map(|p| normalize(&dir.join(p))),
                        workspace: d.workspace,
                        kind,
                    },
                })
                .collect()
        };

    let package = raw.package.map(|p| {
        let mut dependencies = convert(&raw.dependencies, DependencyKind::Normal);
        dependencies.extend(convert(&raw.build_dependencies, DependencyKind::Build));
        dependencies.extend(convert(&raw.dev_dependencies, DependencyKind::Dev));
        CargoPackage {
            name: p.name,
            lib_name: raw.lib.and_then(|lib| lib.name),
            manifest_path: manifest_path.to_path_buf(),
            dir: dir.clone(),
            dependencies,
        }
    });

    let workspace = raw.workspace.map(|w| CargoWorkspace {
        dir: dir.clone(),
        dependencies: convert(&w.dependencies, DependencyKind::Normal)
            .into_iter()
            .map(|d| (d.name.clone(), d))
            .collect(),
    });

    Ok(ParsedManifest { package, workspace })
}

/// Lexically normalize `.` and `..` so that `crates/a/../b` becomes `crates/b`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            PathComponent::CurDir => {}
            PathComponent::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_with_mixed_dependency_forms() {
        let text = r#"
[package]
name = "app"

[dependencies]
serde = "1"
lib_core = { path = "../lib_core" }
renamed = { package = "real-name", version = "1" }
shared = { workspace = true }

[dev-dependencies]
assert_cmd = "2"
"#;
        let parsed = parse_manifest(text, Path::new("crates/app/Cargo.toml")).unwrap();
        let pkg = parsed.package.unwrap();
        assert_eq!(pkg.name, "app");
        assert_eq!(pkg.dir, PathBuf::from("crates/app"));

        let find = |n: &str| {
            pkg.dependencies
                .iter()
                .find(|d| d.name == n)
                .unwrap()
                .clone()
        };
        assert_eq!(find("serde").path, None);
        assert_eq!(
            find("lib_core").path,
            Some(PathBuf::from("crates/lib_core"))
        );
        assert_eq!(find("renamed").package.as_deref(), Some("real-name"));
        assert!(find("shared").workspace);
        assert_eq!(find("assert_cmd").kind, DependencyKind::Dev);
        assert!(parsed.workspace.is_none());
        assert_eq!(pkg.crate_name(), "app");
    }

    #[test]
    fn the_crate_name_follows_the_library_target() {
        let parse = |text: &str| {
            parse_manifest(text, Path::new("Cargo.toml"))
                .unwrap()
                .package
                .unwrap()
        };
        assert_eq!(
            parse("[package]\nname = \"archmap-core\"\n").crate_name(),
            "archmap_core"
        );
        let renamed = parse("[package]\nname = \"foo-cli\"\n\n[lib]\nname = \"foo\"\n");
        assert_eq!(renamed.lib_name.as_deref(), Some("foo"));
        assert_eq!(renamed.crate_name(), "foo");
    }

    #[test]
    fn parses_virtual_workspace() {
        let text = r#"
[workspace]
members = ["crates/*"]

[workspace.dependencies]
lib_core = { path = "crates/lib_core" }
serde = { version = "1", features = ["derive"] }
"#;
        let parsed = parse_manifest(text, Path::new("Cargo.toml")).unwrap();
        assert!(parsed.package.is_none());
        let ws = parsed.workspace.unwrap();
        assert_eq!(ws.dir, PathBuf::from(""));
        assert_eq!(
            ws.dependencies["lib_core"].path,
            Some(PathBuf::from("crates/lib_core"))
        );
        assert_eq!(ws.dependencies["serde"].path, None);
    }

    #[test]
    fn normalizes_parent_segments() {
        assert_eq!(
            normalize(Path::new("crates/a/../b")),
            PathBuf::from("crates/b")
        );
        assert_eq!(
            normalize(Path::new("./crates/./a")),
            PathBuf::from("crates/a")
        );
    }
}
