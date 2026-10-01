//! `package.json`: the package name, whether it is a workspace root, and
//! the dependencies it declares with the line of each.

use std::collections::BTreeMap;

/// What one `package.json` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PackageJson {
    pub name: Option<String>,
    /// Declares `workspaces`: a monorepo root, a package even without code.
    pub workspaces: bool,
    /// `"type": "module"`: Node runs the `.js` files below it as ES modules.
    pub module: bool,
    /// In file order within each section, sections in [`Section::ALL`]
    /// order.
    pub declarations: Vec<Declaration>,
}

/// One declared dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declaration {
    pub name: String,
    pub section: Section,
    pub line: Option<u32>,
}

/// The dependency sections of `package.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Section {
    Dependencies,
    PeerDependencies,
    DevDependencies,
    OptionalDependencies,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::Dependencies,
        Section::PeerDependencies,
        Section::DevDependencies,
        Section::OptionalDependencies,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Section::Dependencies => "dependencies",
            Section::PeerDependencies => "peerDependencies",
            Section::DevDependencies => "devDependencies",
            Section::OptionalDependencies => "optionalDependencies",
        }
    }

    /// Needed at runtime, so an import of it is an edge. A peer dependency
    /// is provided by the package's consumer but needed all the same.
    pub(crate) fn required(self) -> bool {
        matches!(self, Section::Dependencies | Section::PeerDependencies)
    }
}

impl PackageJson {
    /// The declaration that covers an import of `package`: the package
    /// itself or its `@types` package, a required one first.
    pub(crate) fn declaration_of(&self, package: &str) -> Option<&Declaration> {
        let types = types_package(package);
        self.declarations
            .iter()
            .filter(|d| d.name == package || d.name == types)
            .min_by_key(|d| (!d.section.required(), d.name != package, d.section))
    }
}

/// The `@types` package that holds the types of `package`:
/// `@types/aws-lambda`, or `@types/scope__name` for `@scope/name`.
pub(crate) fn types_package(package: &str) -> String {
    match package.strip_prefix('@') {
        Some(scoped) => format!("@types/{}", scoped.replacen('/', "__", 1)),
        None => format!("@types/{package}"),
    }
}

/// Parse a `package.json` text, after a byte order mark if it has one.
pub(crate) fn parse(text: &str) -> Result<PackageJson, String> {
    let text = text.trim_start_matches('\u{feff}');
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let lines = key_lines(text);
    let mut declarations = Vec::new();
    for section in Section::ALL {
        let Some(dependencies) = value.get(section.key()).and_then(|v| v.as_object()) else {
            continue;
        };
        for name in dependencies.keys() {
            declarations.push(Declaration {
                name: name.clone(),
                section,
                line: lines
                    .get(&(section.key().to_owned(), name.clone()))
                    .copied(),
            });
        }
    }
    Ok(PackageJson {
        name: value
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        workspaces: value.get("workspaces").is_some(),
        module: value.get("type").and_then(|v| v.as_str()) == Some("module"),
        declarations,
    })
}

/// The line of every key of the objects at the top level of a JSON text,
/// by top-level key: `("dependencies", "react") -> 5`. `package.json` is
/// plain JSON, so strings followed by `:` are keys and no string spans
/// lines.
fn key_lines(text: &str) -> BTreeMap<(String, String), u32> {
    let bytes = text.as_bytes();
    let mut found = BTreeMap::new();
    let mut section = String::new();
    let (mut depth, mut line, mut i) = (0usize, 1u32, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => line += 1,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                let end = i.min(bytes.len());
                let is_key = text
                    .get(end + 1..)
                    .is_some_and(|rest| rest.trim_start().starts_with(':'));
                if is_key {
                    let key = text[start..end].to_owned();
                    match depth {
                        1 => section = key,
                        2 => {
                            found.insert((section.clone(), key), line);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKAGE: &str = r#"{
  "name": "ts-shop",
  "workspaces": ["packages/*"],
  "dependencies": {
    "react": "^19.0.0",
    "zod": "^4.0.0"
  },
  "peerDependencies": { "react-dom": "^19.0.0" },
  "devDependencies": {
    "@types/aws-lambda": "^8.10.0",
    "react": "^19.0.0"
  },
  "scripts": { "test": "vitest" }
}"#;

    #[test]
    fn sections_names_and_lines() {
        let p = parse(PACKAGE).unwrap();
        assert_eq!(p.name.as_deref(), Some("ts-shop"));
        assert!(p.workspaces);
        let found: Vec<(&str, Section, Option<u32>)> = p
            .declarations
            .iter()
            .map(|d| (d.name.as_str(), d.section, d.line))
            .collect();
        assert_eq!(
            found,
            [
                ("react", Section::Dependencies, Some(5)),
                ("zod", Section::Dependencies, Some(6)),
                ("react-dom", Section::PeerDependencies, Some(8)),
                ("@types/aws-lambda", Section::DevDependencies, Some(10)),
                ("react", Section::DevDependencies, Some(11)),
            ]
        );
    }

    #[test]
    fn a_required_declaration_wins_and_types_packages_count() {
        let p = parse(PACKAGE).unwrap();
        assert_eq!(
            p.declaration_of("react").map(|d| d.section),
            Some(Section::Dependencies)
        );
        let lambda = p.declaration_of("aws-lambda").unwrap();
        assert_eq!(
            (lambda.name.as_str(), lambda.section),
            ("@types/aws-lambda", Section::DevDependencies)
        );
        assert!(p.declaration_of("left-pad").is_none());
        // scripts are no declarations
        assert!(p.declaration_of("test").is_none());
    }

    #[test]
    fn types_packages_of_scoped_names() {
        assert_eq!(types_package("aws-lambda"), "@types/aws-lambda");
        assert_eq!(types_package("@babel/core"), "@types/babel__core");
    }

    #[test]
    fn a_byte_order_mark_is_skipped() {
        let p =
            parse("\u{feff}{ \"name\": \"x\", \"dependencies\": { \"react\": \"^19\" } }").unwrap();
        assert_eq!(p.name.as_deref(), Some("x"));
        assert_eq!(p.declarations.len(), 1);
    }

    #[test]
    fn type_module_is_read() {
        assert!(parse(r#"{ "type": "module" }"#).unwrap().module);
        assert!(!parse(r#"{ "type": "commonjs" }"#).unwrap().module);
        assert!(!parse("{}").unwrap().module);
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse("{ name: x }").is_err());
    }
}
