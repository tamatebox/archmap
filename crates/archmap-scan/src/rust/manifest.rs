//! Minimal Cargo.toml reader. Only the fields archmap needs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component as PathComponent, Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;

use crate::lines::Lines;

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
    /// The line the dependency is declared on.
    pub line: Option<u32>,
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
    /// What the manifest says of the package's targets.
    pub declared: DeclaredTargets,
}

/// The kind of a Cargo target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetKind {
    Lib,
    Bin,
    Test,
    Example,
    Bench,
    Build,
}

impl TargetKind {
    /// Tests, examples and benches may use `[dev-dependencies]` and are no
    /// part of the package's library and binaries: their code is test code.
    pub fn is_test(self) -> bool {
        matches!(
            self,
            TargetKind::Test | TargetKind::Example | TargetKind::Bench
        )
    }
}

/// A target that `Cargo.toml` declares: `[[bin]]`, `[[test]]`,
/// `[[example]]` or `[[bench]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoTarget {
    pub kind: TargetKind,
    pub name: Option<String>,
    /// `path`, relative to the package directory.
    pub path: Option<PathBuf>,
}

/// What a manifest says of its package's targets, beside the ones Cargo
/// finds by itself. A manifest whose targets cannot be read says nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredTargets {
    /// `[lib] path`, relative to the package directory.
    pub lib_path: Option<PathBuf>,
    pub targets: Vec<CargoTarget>,
    /// `package.build`: `Some(None)` for `false`, `Some(Some(path))` for a
    /// path relative to the package directory.
    pub build: Option<Option<PathBuf>>,
    /// The kinds Cargo does not look for, by `autolib`, `autobins`,
    /// `autotests`, `autoexamples` or `autobenches = false`.
    pub undiscovered: BTreeSet<TargetKind>,
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

/// The tables of dependencies again, by the position of each key: a key
/// has one whether its value is a string, an inline table, a dotted key
/// (`serde.workspace = true`) or a `[dependencies.serde]` table, while a
/// dotted key's value has none.
#[derive(Deserialize, Default)]
#[serde(default)]
struct KeyPlaces {
    dependencies: BTreeMap<Spanned<String>, toml::Value>,
    #[serde(rename = "build-dependencies")]
    build_dependencies: BTreeMap<Spanned<String>, toml::Value>,
    #[serde(rename = "dev-dependencies")]
    dev_dependencies: BTreeMap<Spanned<String>, toml::Value>,
    workspace: WorkspaceKeyPlaces,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WorkspaceKeyPlaces {
    dependencies: BTreeMap<Spanned<String>, toml::Value>,
}

/// The target tables again, read loosely: a manifest whose targets this
/// read cannot take still gives its package and dependencies.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTargets {
    package: RawTargetSettings,
    lib: RawTarget,
    bin: Vec<RawTarget>,
    test: Vec<RawTarget>,
    example: Vec<RawTarget>,
    bench: Vec<RawTarget>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTargetSettings {
    build: Option<toml::Value>,
    autolib: Option<toml::Value>,
    autobins: Option<toml::Value>,
    autotests: Option<toml::Value>,
    autoexamples: Option<toml::Value>,
    autobenches: Option<toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTarget {
    name: Option<String>,
    path: Option<String>,
}

/// What the manifest says of its package's targets, or nothing when they
/// cannot be read.
fn declared_targets(text: &str) -> DeclaredTargets {
    let Ok(raw) = toml::from_str::<RawTargets>(text) else {
        return DeclaredTargets::default();
    };
    let settings = &raw.package;
    let build = match &settings.build {
        Some(toml::Value::Boolean(false)) => Some(None),
        Some(toml::Value::String(path)) => Some(Some(PathBuf::from(path))),
        _ => None,
    };
    let undiscovered = [
        (TargetKind::Lib, &settings.autolib),
        (TargetKind::Bin, &settings.autobins),
        (TargetKind::Test, &settings.autotests),
        (TargetKind::Example, &settings.autoexamples),
        (TargetKind::Bench, &settings.autobenches),
    ]
    .into_iter()
    .filter(|(_, auto)| matches!(auto, Some(toml::Value::Boolean(false))))
    .map(|(kind, _)| kind)
    .collect();
    let targets = [
        (TargetKind::Bin, raw.bin),
        (TargetKind::Test, raw.test),
        (TargetKind::Example, raw.example),
        (TargetKind::Bench, raw.bench),
    ]
    .into_iter()
    .flat_map(|(kind, list)| {
        list.into_iter().map(move |t| CargoTarget {
            kind,
            name: t.name,
            path: t.path.map(PathBuf::from),
        })
    })
    .collect();
    DeclaredTargets {
        lib_path: raw.lib.path.map(PathBuf::from),
        targets,
        build,
        undiscovered,
    }
}

/// The table that declares a dependency: its kind's table, or the
/// workspace's.
const WORKSPACE_TABLE: &str = "[workspace.dependencies]";

/// The line of each dependency, by its table and name, from a second read
/// that keeps the keys' positions. A manifest it cannot read that way gives
/// no lines; the first read still gives its dependencies.
fn dependency_lines(text: &str) -> BTreeMap<(&'static str, String), u32> {
    let Ok(places) = toml::from_str::<KeyPlaces>(text) else {
        return BTreeMap::new();
    };
    let lines = Lines::new(text);
    let tables = [
        (DependencyKind::Normal.section(), &places.dependencies),
        (DependencyKind::Build.section(), &places.build_dependencies),
        (DependencyKind::Dev.section(), &places.dev_dependencies),
        (WORKSPACE_TABLE, &places.workspace.dependencies),
    ];
    let mut found = BTreeMap::new();
    for (table, keys) in tables {
        for key in keys.keys() {
            found.insert((table, key.get_ref().clone()), lines.of(key.span().start));
        }
    }
    found
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

    let lines = dependency_lines(text);
    let convert = |table: &BTreeMap<String, RawDependency>,
                   kind: DependencyKind,
                   declared_in: &'static str|
     -> Vec<CargoDependency> {
        table
            .iter()
            .map(|(name, raw)| {
                let line = lines.get(&(declared_in, name.clone())).copied();
                match raw {
                    RawDependency::Version(_) => CargoDependency {
                        name: name.clone(),
                        package: None,
                        path: None,
                        workspace: false,
                        kind,
                        line,
                    },
                    RawDependency::Detailed(d) => CargoDependency {
                        name: name.clone(),
                        package: d.package.clone(),
                        path: d.path.as_deref().map(|p| normalize(&dir.join(p))),
                        workspace: d.workspace,
                        kind,
                        line,
                    },
                }
            })
            .collect()
    };

    let package = raw.package.map(|p| {
        let mut dependencies = Vec::new();
        for (table, kind) in [
            (&raw.dependencies, DependencyKind::Normal),
            (&raw.build_dependencies, DependencyKind::Build),
            (&raw.dev_dependencies, DependencyKind::Dev),
        ] {
            dependencies.extend(convert(table, kind, kind.section()));
        }
        CargoPackage {
            name: p.name,
            lib_name: raw.lib.and_then(|lib| lib.name),
            manifest_path: manifest_path.to_path_buf(),
            dir: dir.clone(),
            dependencies,
            declared: declared_targets(text),
        }
    });

    let workspace = raw.workspace.map(|w| CargoWorkspace {
        dir: dir.clone(),
        dependencies: convert(&w.dependencies, DependencyKind::Normal, WORKSPACE_TABLE)
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
    fn reads_the_targets_a_manifest_declares() {
        let text = r#"
[package]
name = "ledger"
build = false
autotests = false
autobenches = false

[lib]
path = "lib/ledger.rs"

[[bin]]
name = "ledger-cli"
path = "src/main.rs"

[[test]]
name = "books"
"#;
        let pkg = parse_manifest(text, Path::new("ledger/Cargo.toml"))
            .unwrap()
            .package
            .unwrap();
        let declared = pkg.declared;
        assert_eq!(declared.lib_path, Some(PathBuf::from("lib/ledger.rs")));
        assert_eq!(declared.build, Some(None));
        assert_eq!(
            declared.undiscovered,
            BTreeSet::from([TargetKind::Test, TargetKind::Bench])
        );
        assert_eq!(
            declared.targets,
            vec![
                CargoTarget {
                    kind: TargetKind::Bin,
                    name: Some("ledger-cli".into()),
                    path: Some(PathBuf::from("src/main.rs")),
                },
                CargoTarget {
                    kind: TargetKind::Test,
                    name: Some("books".into()),
                    path: None,
                },
            ]
        );
    }

    #[test]
    fn targets_it_cannot_read_leave_the_package_whole() {
        // a build script list, as an unstable Cargo writes it, and a target
        // path that is no string
        let text = r#"
[package]
name = "odd"
build = ["a.rs", "b.rs"]

[[bin]]
name = "x"
path = 7

[dependencies]
serde = "1"
"#;
        let pkg = parse_manifest(text, Path::new("Cargo.toml"))
            .unwrap()
            .package
            .unwrap();
        assert_eq!(pkg.dependencies.len(), 1);
        assert_eq!(pkg.declared, DeclaredTargets::default());
    }

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
    fn dependency_declarations_have_lines() {
        let text = "[package]\nname = \"app\"\n\n[dependencies]\nserde = \"1\"\n\
                    lib_core = { path = \"../lib_core\" }\n\n[dependencies.tokio]\nversion = \"1\"\n\n\
                    [dev-dependencies]\nassert_cmd = \"2\"\n\n[workspace.dependencies]\nanyhow = \"1\"\n";
        let parsed = parse_manifest(text, Path::new("Cargo.toml")).unwrap();
        let lines: Vec<(&str, Option<u32>)> = parsed
            .package
            .as_ref()
            .unwrap()
            .dependencies
            .iter()
            .map(|d| (d.name.as_str(), d.line))
            .collect();
        assert_eq!(
            lines,
            [
                ("lib_core", Some(6)),
                ("serde", Some(5)),
                ("tokio", Some(8)),
                ("assert_cmd", Some(12))
            ]
        );
        assert_eq!(
            parsed.workspace.unwrap().dependencies["anyhow"].line,
            Some(15)
        );
    }

    #[test]
    fn dotted_keys_declare_dependencies_with_their_lines() {
        let text = "[package]\nname = \"app\"\n\n[dependencies]\nserde.workspace = true\n\
                    regex.version = \"1\"\nregex.features = [\"std\"]\nanyhow = \"1\"\n";
        let parsed = parse_manifest(text, Path::new("Cargo.toml")).unwrap();
        let deps: Vec<(&str, bool, Option<u32>)> = parsed
            .package
            .as_ref()
            .unwrap()
            .dependencies
            .iter()
            .map(|d| (d.name.as_str(), d.workspace, d.line))
            .collect();
        assert_eq!(
            deps,
            [
                ("anyhow", false, Some(8)),
                ("regex", false, Some(6)),
                ("serde", true, Some(5))
            ]
        );
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
