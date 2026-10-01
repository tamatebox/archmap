//! Resolve Python import names to declared distributions.
//!
//! Only distributions declared in the project's manifests are candidates. An
//! import of anything else produces no dependency edge: an observed import is
//! not a declared dependency, and undeclared imports are a separate problem
//! (a rule, not a graph edge). Resolution tries, in order:
//!
//! 1. **Name conventions.** The longest dotted prefix whose PEP 503 form is a
//!    declared distribution: `requests`, `pandas_gbq` for `pandas-gbq`,
//!    `google.cloud.bigquery` for `google-cloud-bigquery`.
//! 2. **Installed metadata.** The `RECORD` files of a `.venv` next to the
//!    project list the files each installed distribution placed. The longest
//!    import prefix with any provider decides; if its providers are not
//!    declared, resolution stops instead of guessing a shorter prefix.
//! 3. **Known import names.** A small built-in table for well-known
//!    mismatches such as `sklearn` for `scikit-learn`.
//!
//! Every result says how it was reached, so evidence can record it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::manifest::normalize_dist_name;

/// Import prefixes of well-known distributions whose import name differs
/// from the distribution name. Several candidates are allowed; the declared
/// one wins.
const KNOWN_IMPORT_NAMES: &[(&str, &[&str])] = &[
    ("Crypto", &["pycryptodome"]),
    ("MySQLdb", &["mysqlclient"]),
    ("OpenSSL", &["pyopenssl"]),
    ("PIL", &["pillow"]),
    ("attr", &["attrs"]),
    ("bs4", &["beautifulsoup4"]),
    (
        "cv2",
        &[
            "opencv-python",
            "opencv-python-headless",
            "opencv-contrib-python",
            "opencv-contrib-python-headless",
        ],
    ),
    ("dateutil", &["python-dateutil"]),
    ("docx", &["python-docx"]),
    ("dotenv", &["python-dotenv"]),
    ("git", &["gitpython"]),
    (
        "google.cloud.secretmanager",
        &["google-cloud-secret-manager"],
    ),
    (
        "google.cloud.secretmanager_v1",
        &["google-cloud-secret-manager"],
    ),
    ("google.oauth2", &["google-auth"]),
    ("google.protobuf", &["protobuf"]),
    ("googleapiclient", &["google-api-python-client"]),
    ("grpc", &["grpcio"]),
    ("imblearn", &["imbalanced-learn"]),
    ("jose", &["python-jose"]),
    ("jwt", &["pyjwt"]),
    ("magic", &["python-magic"]),
    ("multipart", &["python-multipart"]),
    ("pkg_resources", &["setuptools"]),
    ("pptx", &["python-pptx"]),
    ("psycopg2", &["psycopg2", "psycopg2-binary"]),
    ("serial", &["pyserial"]),
    ("skimage", &["scikit-image"]),
    ("sklearn", &["scikit-learn"]),
    ("yaml", &["pyyaml"]),
    ("zmq", &["pyzmq"]),
];

/// How an import name was matched to a distribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// The top-level import name is the distribution name.
    Name,
    /// The dotted import path joined with `-` is the distribution name.
    DottedName,
    /// An installed distribution's `RECORD` lists files under the import.
    Installed { record: String },
    /// archmap's table of well-known import names.
    KnownImportName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// PEP 503 normalized distribution name.
    pub distribution: String,
    pub method: Method,
    /// The import prefix that matched (`google.cloud.bigquery` for
    /// `google.cloud.bigquery.Client`).
    pub matched: String,
}

impl Resolved {
    /// Evidence note for an import statement resolved this way.
    pub fn note(&self) -> String {
        match self.method_note() {
            Some(method) => format!("import {}, {method}", self.matched),
            None => "import".to_owned(),
        }
    }

    /// How the import name was matched, unless it is simply the
    /// distribution name: `matched by dotted name`.
    pub fn method_note(&self) -> Option<String> {
        match &self.method {
            Method::Name => None,
            Method::DottedName => Some("matched by dotted name".to_owned()),
            Method::Installed { record } => Some(format!("provided per {record}")),
            Method::KnownImportName => Some("matched by known import name".to_owned()),
        }
    }
}

/// Which installed distributions placed files under each import prefix.
#[derive(Debug, Default, Clone)]
pub struct InstalledIndex {
    providers: BTreeMap<String, BTreeSet<String>>,
    /// Distribution -> its `RECORD`, relative to the scanned root.
    records: BTreeMap<String, String>,
}

impl InstalledIndex {
    /// Read every `*.dist-info/RECORD` under the `site-packages` of `venv`
    /// (absolute), recording paths relative to `root` (absolute).
    pub fn load(root: &Path, venv: &Path) -> Self {
        let mut index = InstalledIndex::default();
        for site_packages in site_packages_dirs(venv) {
            let Ok(entries) = std::fs::read_dir(&site_packages) else {
                continue;
            };
            let mut dist_infos: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "dist-info"))
                .collect();
            dist_infos.sort();
            for dist_info in dist_infos {
                let Some(distribution) = distribution_of(&dist_info) else {
                    continue;
                };
                let record = dist_info.join("RECORD");
                let Ok(text) = std::fs::read_to_string(&record) else {
                    continue;
                };
                let shown = record.strip_prefix(root).unwrap_or(&record);
                index.add_record(&distribution, &crate::context::display_path(shown), &text);
            }
        }
        index
    }

    /// Distributions that placed files under the longest prefix of `dotted`
    /// that any installed distribution provides, sorted.
    pub fn providers_of(&self, dotted: &str) -> Vec<String> {
        let segments: Vec<&str> = dotted.split('.').collect();
        (1..=segments.len())
            .rev()
            .find_map(|len| self.providers.get(&segments[..len].join(".")))
            .map(|dists| dists.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether an installed distribution placed a module or package at
    /// exactly `dotted`: `google.cloud.bigquery` is one, `yaml.safe_load`
    /// is not.
    pub fn is_module(&self, dotted: &str) -> bool {
        self.providers.contains_key(dotted)
    }

    /// Register one distribution's `RECORD` text.
    pub fn add_record(&mut self, distribution: &str, record: &str, text: &str) {
        for line in text.lines() {
            let Some(module) = module_of_record_path(record_path(line)) else {
                continue;
            };
            let segments: Vec<&str> = module.split('.').collect();
            for len in 1..=segments.len() {
                self.providers
                    .entry(segments[..len].join("."))
                    .or_default()
                    .insert(distribution.to_owned());
            }
        }
        self.records
            .insert(distribution.to_owned(), record.to_owned());
    }
}

/// Resolves imports of one project.
pub struct Resolver<'a> {
    /// Declared distributions, PEP 503 normalized.
    pub declared: &'a BTreeSet<String>,
    pub installed: &'a InstalledIndex,
}

impl Resolver<'_> {
    /// Resolve a dotted import path (a module, or `module.name` from a
    /// `from` import) to a declared distribution.
    pub fn resolve(&self, dotted: &str) -> Option<Resolved> {
        let segments: Vec<&str> = dotted.split('.').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return None;
        }
        let prefixes = || {
            (1..=segments.len())
                .rev()
                .map(|len| (len, segments[..len].join(".")))
        };

        for (len, prefix) in prefixes() {
            let name = normalize_dist_name(&segments[..len].join("-"));
            if self.declared.contains(&name) {
                let method = if len == 1 {
                    Method::Name
                } else {
                    Method::DottedName
                };
                return Some(Resolved {
                    distribution: name,
                    method,
                    matched: prefix,
                });
            }
        }

        for (_, prefix) in prefixes() {
            let Some(providers) = self.installed.providers.get(&prefix) else {
                continue;
            };
            let declared: Vec<&String> = providers
                .iter()
                .filter(|d| self.declared.contains(*d))
                .collect();
            // The first prefix with providers decides. A shorter prefix such
            // as `google` would otherwise attribute an undeclared
            // `google.api_core` import to whatever google-* is declared.
            return match declared.as_slice() {
                [only] => Some(Resolved {
                    distribution: (*only).clone(),
                    method: Method::Installed {
                        record: self.installed.records[*only].clone(),
                    },
                    matched: prefix,
                }),
                _ => None,
            };
        }

        for (_, prefix) in prefixes() {
            let Some((_, candidates)) = KNOWN_IMPORT_NAMES.iter().find(|(name, _)| *name == prefix)
            else {
                continue;
            };
            let declared: Vec<&&str> = candidates
                .iter()
                .filter(|d| self.declared.contains(**d))
                .collect();
            return match declared.as_slice() {
                [only] => Some(Resolved {
                    distribution: (**only).to_owned(),
                    method: Method::KnownImportName,
                    matched: prefix,
                }),
                _ => None,
            };
        }
        None
    }
}

pub(crate) fn site_packages_dirs(venv: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(venv.join("lib")) {
        let mut pythons: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("python"))
            })
            .collect();
        pythons.sort();
        dirs.extend(pythons.into_iter().map(|p| p.join("site-packages")));
    }
    dirs.push(venv.join("Lib").join("site-packages")); // Windows layout
    dirs.into_iter().filter(|d| d.is_dir()).collect()
}

/// `PyYAML-6.0.2.dist-info` -> `pyyaml`.
fn distribution_of(dist_info: &Path) -> Option<String> {
    let stem = dist_info.file_stem()?.to_str()?;
    let (name, _version) = stem.rsplit_once('-')?;
    Some(normalize_dist_name(name))
}

/// The path column of a `RECORD` line, which is CSV.
fn record_path(line: &str) -> &str {
    match line.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => line.split(',').next().unwrap_or(""),
    }
}

/// Can `segment` be one part of a dotted import path (`import a.b`)?
pub(super) fn is_identifier(segment: &str) -> bool {
    let mut chars = segment.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// Importable module for an installed file: `google/cloud/bigquery/table.py`
/// is `google.cloud.bigquery.table`, `yaml/__init__.py` is `yaml`, and
/// `_yaml.cpython-312-darwin.so` is `_yaml`. Metadata, scripts and caches are
/// not modules.
fn module_of_record_path(path: &str) -> Option<String> {
    if path.is_empty()
        || path.starts_with("..")
        || path.starts_with('/')
        || path.contains(".dist-info/")
        || path.contains(".data/")
        || path.contains("__pycache__")
    {
        return None;
    }
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let module = if let Some(stem) = file.strip_suffix(".py") {
        stem
    } else if file.ends_with(".so") || file.ends_with(".pyd") {
        file.split('.').next()?
    } else {
        return None;
    };
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    if module != "__init__" {
        parts.push(module);
    }
    if parts.is_empty() || !parts.iter().all(|p| is_identifier(p)) {
        return None;
    }
    Some(parts.join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn resolve(
        declared: &BTreeSet<String>,
        installed: &InstalledIndex,
        dotted: &str,
    ) -> Option<Resolved> {
        Resolver {
            declared,
            installed,
        }
        .resolve(dotted)
    }

    #[test]
    fn name_conventions_prefer_the_longest_declared_prefix() {
        let d = declared(&[
            "requests",
            "pandas-gbq",
            "google-cloud-bigquery",
            "google-cloud-bigquery-storage",
        ]);
        let none = InstalledIndex::default();
        let dist = |s: &str| resolve(&d, &none, s).map(|r| (r.distribution, r.method));
        assert_eq!(
            dist("requests.adapters"),
            Some(("requests".into(), Method::Name))
        );
        assert_eq!(
            dist("pandas_gbq"),
            Some(("pandas-gbq".into(), Method::Name))
        );
        assert_eq!(
            dist("google.cloud.bigquery.table"),
            Some(("google-cloud-bigquery".into(), Method::DottedName))
        );
        assert_eq!(
            dist("google.cloud.bigquery_storage"),
            Some(("google-cloud-bigquery-storage".into(), Method::DottedName))
        );
        // a namespace shared by several declared distributions is not an edge
        assert_eq!(dist("google.cloud"), None);
        // undeclared and standard-library imports are not edges
        assert_eq!(dist("os.path"), None);
        assert_eq!(dist("httpx"), None);
    }

    #[test]
    fn installed_metadata_resolves_and_stops_at_undeclared_providers() {
        let mut installed = InstalledIndex::default();
        installed.add_record(
            "pyyaml",
            ".venv/lib/python3.12/site-packages/PyYAML-6.0.2.dist-info/RECORD",
            "yaml/__init__.py,sha256=x,1\nyaml/_yaml.cpython-312-darwin.so,sha256=x,2\nPyYAML-6.0.2.dist-info/RECORD,,\n",
        );
        installed.add_record(
            "google-auth",
            "auth/RECORD",
            "google/auth/__init__.py,,\ngoogle/oauth2/credentials.py,,\n",
        );
        installed.add_record(
            "google-api-core",
            "core/RECORD",
            "google/api_core/exceptions.py,,\n",
        );
        installed.add_record(
            "google-cloud-storage",
            "storage/RECORD",
            "google/cloud/storage/__init__.py,,\n",
        );
        let d = declared(&["pyyaml", "google-auth", "google-cloud-storage"]);

        let yaml = resolve(&d, &installed, "yaml").unwrap();
        assert_eq!(yaml.distribution, "pyyaml");
        assert_eq!(
            yaml.note(),
            "import yaml, provided per .venv/lib/python3.12/site-packages/PyYAML-6.0.2.dist-info/RECORD"
        );
        assert_eq!(
            resolve(&d, &installed, "google.oauth2.credentials")
                .unwrap()
                .distribution,
            "google-auth"
        );
        // google.api_core belongs to an undeclared distribution: no guessing
        assert_eq!(resolve(&d, &installed, "google.api_core.exceptions"), None);
        assert_eq!(
            installed.providers_of("google.api_core.exceptions"),
            vec!["google-api-core"]
        );
        assert!(installed.providers_of("numpy").is_empty());
    }

    #[test]
    fn known_import_names_are_the_last_resort() {
        let none = InstalledIndex::default();
        let d = declared(&["scikit-learn", "opencv-python-headless"]);
        let sklearn = resolve(&d, &none, "sklearn.metrics").unwrap();
        assert_eq!(sklearn.distribution, "scikit-learn");
        assert_eq!(sklearn.method, Method::KnownImportName);
        assert_eq!(
            sklearn.note(),
            "import sklearn, matched by known import name"
        );
        assert_eq!(
            resolve(&d, &none, "cv2").unwrap().distribution,
            "opencv-python-headless"
        );
        // known, but not declared
        assert_eq!(resolve(&d, &none, "yaml"), None);
    }

    #[test]
    fn record_paths_map_to_modules() {
        assert_eq!(
            module_of_record_path("yaml/__init__.py").as_deref(),
            Some("yaml")
        );
        assert_eq!(module_of_record_path("six.py").as_deref(), Some("six"));
        assert_eq!(
            module_of_record_path("google/cloud/bigquery/table.py").as_deref(),
            Some("google.cloud.bigquery.table")
        );
        assert_eq!(
            module_of_record_path("_cffi_backend.cpython-312-darwin.so").as_deref(),
            Some("_cffi_backend")
        );
        assert_eq!(module_of_record_path("../../bin/tool"), None);
        assert_eq!(module_of_record_path("pkg-1.0.dist-info/METADATA"), None);
        assert_eq!(
            module_of_record_path("pkg/__pycache__/x.cpython-312.pyc"),
            None
        );
        assert_eq!(module_of_record_path("requests-stubs/api.pyi"), None);
        assert_eq!(record_path("\"odd,name.py\",sha256=x,1"), "odd,name.py");
    }

    #[test]
    fn dist_info_names_normalize() {
        assert_eq!(
            distribution_of(Path::new("PyYAML-6.0.2.dist-info")).as_deref(),
            Some("pyyaml")
        );
        assert_eq!(
            distribution_of(Path::new("google_cloud_bigquery-3.30.0.dist-info")).as_deref(),
            Some("google-cloud-bigquery")
        );
    }
}
