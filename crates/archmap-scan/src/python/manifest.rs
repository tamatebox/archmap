//! Python project manifests: `pyproject.toml` and `requirements*.txt`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::{Spanned, Value};

use crate::lines::Lines;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyDependency {
    /// PEP 503 normalized distribution name (`PyYAML` -> `pyyaml`).
    pub name: String,
    /// The line the dependency was declared on, when the format has lines.
    pub line: Option<u32>,
    /// Manifest section, for evidence notes.
    pub section: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyProject {
    /// Declared project name, if the manifest has one.
    pub name: Option<String>,
    pub manifest_path: PathBuf,
    /// Directory containing the manifest, relative to the repository root.
    pub dir: PathBuf,
    pub dependencies: Vec<PyDependency>,
    /// Distributions declared for extras, development or tests, with the
    /// table and group that declare them. They are not dependency edges,
    /// but importing them is not undeclared either.
    pub optional_dependencies: Vec<PyDependency>,
}

/// Normalize a distribution name per PEP 503.
pub fn normalize_dist_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_sep = false;
    for c in name.chars() {
        if c == '-' || c == '_' || c == '.' {
            if !last_sep {
                out.push('-');
            }
            last_sep = true;
        } else {
            out.push(c.to_ascii_lowercase());
            last_sep = false;
        }
    }
    out
}

/// Extract the distribution name from a PEP 508 requirement string
/// (`requests[security]>=2.0; python_version > "3"` -> `requests`).
pub fn requirement_name(spec: &str) -> Option<String> {
    let name: String = spec
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    (!name.is_empty()).then(|| normalize_dist_name(&name))
}

pub fn parse_pyproject(text: &str, manifest_path: &Path) -> Result<PyProject, toml::de::Error> {
    let value: Value = toml::from_str(text)?;
    let lines = declaration_lines(text);
    let line =
        |section: &str, name: &str| lines.get(&(section.to_owned(), name.to_owned())).copied();
    let dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut dependencies = Vec::new();

    let project = value.get("project");
    let poetry = value.get("tool").and_then(|t| t.get("poetry"));

    let name = project
        .and_then(|p| p.get("name"))
        .or_else(|| poetry.and_then(|p| p.get("name")))
        .and_then(Value::as_str)
        .map(str::to_owned);

    if let Some(list) = project
        .and_then(|p| p.get("dependencies"))
        .and_then(Value::as_array)
    {
        for spec in list.iter().filter_map(Value::as_str) {
            if let Some(dep) = requirement_name(spec) {
                let section = "[project] dependencies";
                dependencies.push(PyDependency {
                    line: line(section, &dep),
                    name: dep,
                    section: section.to_owned(),
                });
            }
        }
    }

    if let Some(table) = poetry
        .and_then(|p| p.get("dependencies"))
        .and_then(Value::as_table)
    {
        for key in table.keys().filter(|k| k.as_str() != "python") {
            let (name, section) = (normalize_dist_name(key), "[tool.poetry.dependencies]");
            dependencies.push(PyDependency {
                line: line(section, &name),
                name,
                section: section.to_owned(),
            });
        }
    }

    let optional_dependencies = optional_declarations(&value, project, poetry, &lines);

    Ok(PyProject {
        name,
        manifest_path: manifest_path.to_path_buf(),
        dir,
        dependencies,
        optional_dependencies,
    })
}

/// Distributions declared outside the runtime dependencies: PEP 621
/// extras, PEP 735 dependency groups, poetry groups and dev-dependencies,
/// and uv dev-dependencies, each with the table and group that declare it.
fn optional_declarations(
    value: &Value,
    project: Option<&Value>,
    poetry: Option<&Value>,
    lines: &BTreeMap<(String, String), u32>,
) -> Vec<PyDependency> {
    let mut found = Vec::new();
    let mut add = |name: String, section: String| {
        let line = lines.get(&(section.clone(), name.clone())).copied();
        found.push(PyDependency {
            name,
            line,
            section,
        })
    };
    let lists = [
        (
            "[project.optional-dependencies]",
            project.and_then(|p| p.get("optional-dependencies")),
        ),
        ("[dependency-groups]", value.get("dependency-groups")),
    ];
    for (table_name, table) in lists {
        for (group, list) in table.and_then(Value::as_table).into_iter().flatten() {
            // PEP 735 also allows `{ include-group = "..." }` entries: skip them
            for name in list
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(requirement_name)
            {
                add(name, format!("{table_name} {group}"));
            }
        }
    }

    let groups = poetry
        .and_then(|p| p.get("group"))
        .and_then(Value::as_table)
        .into_iter()
        .flatten()
        .filter_map(|(group, t)| {
            let section = format!("[tool.poetry.group.{group}.dependencies]");
            t.get("dependencies").map(|deps| (section, deps))
        });
    let dev = poetry
        .and_then(|p| p.get("dev-dependencies"))
        .map(|deps| ("[tool.poetry.dev-dependencies]".to_owned(), deps));
    for (section, table) in groups.chain(dev) {
        for key in table.as_table().into_iter().flat_map(|t| t.keys()) {
            if key != "python" {
                add(normalize_dist_name(key), section.clone());
            }
        }
    }

    if let Some(list) = value
        .get("tool")
        .and_then(|t| t.get("uv"))
        .and_then(|uv| uv.get("dev-dependencies"))
        .and_then(Value::as_array)
    {
        for name in list
            .iter()
            .filter_map(Value::as_str)
            .filter_map(requirement_name)
        {
            add(name, "[tool.uv] dev-dependencies".to_owned());
        }
    }

    found.sort_by(|a, b| (&a.name, &a.section).cmp(&(&b.name, &b.section)));
    found.dedup();
    found
}

/// Where each declaration of a `pyproject.toml` is written, by the section
/// its evidence note names and its normalized name: a second, typed read
/// that keeps the place of each list item and of each poetry key (a key has
/// one whatever its value, a dotted `sqlalchemy.version = "^2"` included,
/// while a dotted key's value has none). A manifest it cannot read that way
/// gives no lines.
fn declaration_lines(text: &str) -> BTreeMap<(String, String), u32> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Manifest {
        project: ProjectTable,
        #[serde(rename = "dependency-groups")]
        groups: BTreeMap<String, Vec<Spanned<Value>>>,
        tool: ToolTable,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct ProjectTable {
        dependencies: Vec<Spanned<Value>>,
        #[serde(rename = "optional-dependencies")]
        extras: BTreeMap<String, Vec<Spanned<Value>>>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct ToolTable {
        poetry: Poetry,
        uv: Uv,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Poetry {
        dependencies: BTreeMap<Spanned<String>, Value>,
        #[serde(rename = "dev-dependencies")]
        dev: BTreeMap<Spanned<String>, Value>,
        group: BTreeMap<String, PoetryGroup>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct PoetryGroup {
        dependencies: BTreeMap<Spanned<String>, Value>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Uv {
        #[serde(rename = "dev-dependencies")]
        dev: Vec<Spanned<Value>>,
    }

    let Ok(manifest) = toml::from_str::<Manifest>(text) else {
        return BTreeMap::new();
    };
    let positions = Lines::new(text);
    let mut lines = BTreeMap::new();
    let mut at = |section: String, name: String, start: usize| {
        lines.entry((section, name)).or_insert(positions.of(start));
    };
    let mut lists = vec![(
        "[project] dependencies".to_owned(),
        &manifest.project.dependencies,
    )];
    for (group, specs) in &manifest.project.extras {
        lists.push((format!("[project.optional-dependencies] {group}"), specs));
    }
    for (group, specs) in &manifest.groups {
        lists.push((format!("[dependency-groups] {group}"), specs));
    }
    lists.push((
        "[tool.uv] dev-dependencies".to_owned(),
        &manifest.tool.uv.dev,
    ));
    for (section, specs) in lists {
        for spec in specs {
            if let Some(name) = spec.get_ref().as_str().and_then(requirement_name) {
                at(section.clone(), name, spec.span().start);
            }
        }
    }
    let poetry = &manifest.tool.poetry;
    let mut tables = vec![
        (
            "[tool.poetry.dependencies]".to_owned(),
            &poetry.dependencies,
        ),
        ("[tool.poetry.dev-dependencies]".to_owned(), &poetry.dev),
    ];
    for (group, table) in &poetry.group {
        tables.push((
            format!("[tool.poetry.group.{group}.dependencies]"),
            &table.dependencies,
        ));
    }
    for (section, table) in tables {
        for key in table.keys() {
            at(
                section.clone(),
                normalize_dist_name(key.get_ref()),
                key.span().start,
            );
        }
    }
    lines
}

/// Dependencies from a `requirements.txt`-style file.
pub fn parse_requirements(text: &str) -> Vec<PyDependency> {
    text.lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('-') {
                return None; // blank, comment, or pip option such as `-r base.txt`
            }
            requirement_name(line).map(|name| PyDependency {
                name,
                line: Some((idx + 1) as u32),
                section: REQUIREMENTS.to_owned(),
            })
        })
        .collect()
}

/// Is this file name a requirements file we should read?
/// `requirements*.txt`, `*-requirements.txt` / `*_requirements.txt`, and
/// any `.txt` inside a `requirements/` directory.
pub fn is_requirements_file(path: &Path) -> bool {
    let Some(stem) = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".txt"))
    else {
        return false;
    };
    let in_requirements_dir = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d == "requirements");
    let suffixed = stem
        .strip_suffix("requirements")
        .is_some_and(|rest| rest.ends_with(['-', '_']));
    in_requirements_dir || stem.starts_with("requirements") || suffixed
}

/// Section of a dependency in a requirements file for running the code.
pub const REQUIREMENTS: &str = "requirements";
/// Section of a dependency in a requirements file for development, and why.
pub const DEV_BY_FILE_NAME: &str = "dev by file name";
pub const DEV_BY_DIRECTORY_NAME: &str = "dev by directory name";

/// Words in a requirements file name that mark it as development-only.
const DEV_WORDS: &[&str] = &["dev", "test", "tests", "testing", "lint", "docs"];

/// Why a requirements file is for development rather than for running the
/// code, if it is: a dev word in its name (`requirements-dev.txt`,
/// `test_requirements.txt`, `requirements/lint.txt`, but not
/// `requirements-devices.txt`), or a directory below `project_dir` named by
/// one word (`docs/requirements.txt`, `docs/requirements/base.txt`, but not
/// `docs_api/requirements.txt`, nor the requirements of a project in
/// `docs/`).
pub fn dev_requirements(path: &Path, project_dir: &Path) -> Option<&'static str> {
    let stem = path.file_stem()?.to_str()?;
    if stem.split(['-', '_', '.']).any(|w| DEV_WORDS.contains(&w)) {
        return Some(DEV_BY_FILE_NAME);
    }
    let mut dir = path.parent()?;
    if dir.file_name().is_some_and(|n| n == "requirements") {
        dir = dir.parent()?;
    }
    if dir == project_dir {
        return None;
    }
    let name = dir.file_name()?.to_str()?;
    DEV_WORDS.contains(&name).then_some(DEV_BY_DIRECTORY_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyproject_declarations_have_lines() {
        let text = "[project]\nname = \"shop\"\ndependencies = [\n  \"requests>=2\",\n  \"PyYAML\",\n]\n\n\
                    [project.optional-dependencies]\ndev = [\"pytest\"]\n\n\
                    [dependency-groups]\nlint = [\"ruff\", {include-group = \"dev\"}]\n\n\
                    [tool.poetry.dependencies]\npython = \"^3.11\"\nsqlalchemy = \"^2\"\ncelery.version = \"^5\"\n\n\
                    [tool.poetry.group.test.dependencies]\nhypothesis = \"*\"\n\n\
                    [tool.uv]\ndev-dependencies = [\"mypy\"]\n";
        let parsed = parse_pyproject(text, Path::new("pyproject.toml")).unwrap();
        let lines = |deps: &[PyDependency]| -> Vec<(String, Option<u32>)> {
            deps.iter().map(|d| (d.name.clone(), d.line)).collect()
        };
        let at = |name: &str, line| (name.to_owned(), Some(line));
        // a dotted key (`celery.version`) has its line like any other
        assert_eq!(
            lines(&parsed.dependencies),
            [
                at("requests", 4),
                at("pyyaml", 5),
                at("celery", 17),
                at("sqlalchemy", 16)
            ]
        );
        assert_eq!(
            lines(&parsed.optional_dependencies),
            [
                at("hypothesis", 20),
                at("mypy", 23),
                at("pytest", 9),
                at("ruff", 12)
            ]
        );
    }

    #[test]
    fn normalizes_names_like_pep_503() {
        assert_eq!(normalize_dist_name("PyYAML"), "pyyaml");
        assert_eq!(
            normalize_dist_name("typing_extensions"),
            "typing-extensions"
        );
        assert_eq!(normalize_dist_name("zope.interface"), "zope-interface");
    }

    #[test]
    fn requirement_name_strips_extras_versions_and_markers() {
        assert_eq!(
            requirement_name("requests[security]>=2.0").as_deref(),
            Some("requests")
        );
        assert_eq!(
            requirement_name("Django ; python_version>'3'").as_deref(),
            Some("django")
        );
        assert_eq!(requirement_name(""), None);
    }

    #[test]
    fn parses_pep621_and_poetry() {
        let text = r#"
[project]
name = "shop"
dependencies = ["requests>=2", "SQLAlchemy[asyncio]"]

[tool.poetry.dependencies]
python = "^3.11"
pydantic = "2"
"#;
        let p = parse_pyproject(text, Path::new("pyproject.toml")).unwrap();
        assert_eq!(p.name.as_deref(), Some("shop"));
        let names: Vec<&str> = p.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["requests", "sqlalchemy", "pydantic"]);
    }

    #[test]
    fn collects_optional_and_development_declarations() {
        let text = r#"
[project]
name = "app"
dependencies = ["requests"]

[project.optional-dependencies]
dev = ["pytest>=8", "pytest-mock"]

[dependency-groups]
lint = ["ruff", { include-group = "dev" }]

[tool.poetry.group.test.dependencies]
Hypothesis = "*"

[tool.poetry.dev-dependencies]
black = "*"

[tool.uv]
dev-dependencies = ["mypy"]
"#;
        let p = parse_pyproject(text, Path::new("pyproject.toml")).unwrap();
        let optional: Vec<(&str, &str)> = p
            .optional_dependencies
            .iter()
            .map(|d| (d.name.as_str(), d.section.as_str()))
            .collect();
        assert_eq!(
            optional,
            vec![
                ("black", "[tool.poetry.dev-dependencies]"),
                ("hypothesis", "[tool.poetry.group.test.dependencies]"),
                ("mypy", "[tool.uv] dev-dependencies"),
                ("pytest", "[project.optional-dependencies] dev"),
                ("pytest-mock", "[project.optional-dependencies] dev"),
                ("ruff", "[dependency-groups] lint"),
            ]
        );
        let runtime: Vec<&str> = p.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(runtime, vec!["requests"]);
    }

    #[test]
    fn requirements_files_named_for_development_are_dev() {
        for dev in [
            "requirements-dev.txt",
            "requirements_test.txt",
            "requirements-tests.txt",
            "requirements-dev-gpu.txt",
            "requirements/lint.txt",
            "requirements/test-ml.txt",
            "docs/requirements-docs.txt",
            "dev-requirements.txt",
            "test_requirements.txt",
            "docs/requirements.txt",
            "docs/requirements/base.txt",
        ] {
            assert!(
                dev_requirements(Path::new(dev), Path::new("")).is_some(),
                "{dev}"
            );
        }
        for runtime in [
            "requirements.txt",
            "requirements-prod.txt",
            "requirements-ml.txt",
            "requirements-devices.txt",
            "devices-requirements.txt",
            "requirements/base.txt",
            "requirements/devices.txt",
            "functions/notify/requirements.txt",
            "docs_api/requirements.txt",
            "tests/integration/requirements.txt",
            "requirements/requirements.txt",
        ] {
            assert_eq!(
                dev_requirements(Path::new(runtime), Path::new("")),
                None,
                "{runtime}"
            );
        }
        // a project of its own named `docs` runs on its requirements.txt
        assert_eq!(
            dev_requirements(Path::new("docs/requirements.txt"), Path::new("docs")),
            None
        );
    }

    #[test]
    fn parses_requirements_lines() {
        let deps = parse_requirements("# base\n-r other.txt\nflask==3.0  # web\n\nNumPy\n");
        let names: Vec<(&str, Option<u32>)> =
            deps.iter().map(|d| (d.name.as_str(), d.line)).collect();
        assert_eq!(names, vec![("flask", Some(3)), ("numpy", Some(5))]);
    }

    #[test]
    fn recognizes_requirements_files() {
        assert!(is_requirements_file(Path::new("requirements.txt")));
        assert!(is_requirements_file(Path::new("requirements-dev.txt")));
        assert!(is_requirements_file(Path::new("requirements/prod.txt")));
        assert!(is_requirements_file(Path::new("dev-requirements.txt")));
        assert!(is_requirements_file(Path::new("test_requirements.txt")));
        assert!(!is_requirements_file(Path::new("README.txt")));
        assert!(!is_requirements_file(Path::new("xrequirements.txt")));
    }
}
