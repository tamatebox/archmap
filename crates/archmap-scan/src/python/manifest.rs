//! Python project manifests: `pyproject.toml` and `requirements*.txt`.

use std::path::{Path, PathBuf};

use toml::Value;

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
                dependencies.push(PyDependency {
                    name: dep,
                    line: None,
                    section: "[project] dependencies".to_owned(),
                });
            }
        }
    }

    if let Some(table) = poetry
        .and_then(|p| p.get("dependencies"))
        .and_then(Value::as_table)
    {
        for key in table.keys().filter(|k| k.as_str() != "python") {
            dependencies.push(PyDependency {
                name: normalize_dist_name(key),
                line: None,
                section: "[tool.poetry.dependencies]".to_owned(),
            });
        }
    }

    Ok(PyProject {
        name,
        manifest_path: manifest_path.to_path_buf(),
        dir,
        dependencies,
    })
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
                section: "requirements".to_owned(),
            })
        })
        .collect()
}

/// Is this file name a requirements file we should read?
pub fn is_requirements_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let in_requirements_dir = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d == "requirements");
    name.ends_with(".txt") && (name.starts_with("requirements") || in_requirements_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!is_requirements_file(Path::new("README.txt")));
    }
}
