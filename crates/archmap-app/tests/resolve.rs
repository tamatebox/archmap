//! One resolver for `query` and `impact`: every form a target can take,
//! and candidates instead of an error when it names several things.

use std::path::{Path, PathBuf};

use archmap_app::{
    Answer, Format, Found, ImpactRequest, QueryRequest, ScanMode, Workspace, DEFAULT_DEPTH,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

/// A throwaway repository with `files`, removed when the guard drops.
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str, files: &[(&str, &str)]) -> Repo {
        let dir = std::env::temp_dir().join(format!("archmap-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (file, text) in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        Repo(dir)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scan(root: &Path) -> Workspace {
    Workspace::scan(root, ScanMode::Full).unwrap()
}

fn query(ws: &Workspace, target: &str) -> Answer {
    query_as(ws, target, Format::Text)
}

fn query_as(ws: &Workspace, target: &str, format: Format) -> Answer {
    ws.query(&QueryRequest {
        target,
        depth: DEFAULT_DEPTH,
        format,
        verbose: false,
    })
    .unwrap()
}

fn impact(ws: &Workspace, target: &str) -> Answer {
    ws.impact(&ImpactRequest {
        target,
        depth: DEFAULT_DEPTH,
        verbose: false,
    })
    .unwrap()
}

/// Two Python projects, each with a `tests` package.
fn two_tests_packages(name: &str) -> Repo {
    let mut files = Vec::new();
    for project in ["a", "b"] {
        files.push((
            format!("{project}/pyproject.toml"),
            format!("[project]\nname = \"{project}\"\nversion = \"0.1.0\"\n"),
        ));
        files.push((
            format!("{project}/tests/test_it.py"),
            "def test_it():\n    pass\n".to_owned(),
        ));
    }
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(f, t)| (f.as_str(), t.as_str()))
        .collect();
    Repo::new(name, &files)
}

/// Two TS exports named `helper`, one under a directory the shell would
/// expand, and a directory named `helper` that holds no code.
fn helpers_beside_a_directory(name: &str) -> Repo {
    Repo::new(
        name,
        &[
            ("package.json", "{\"name\": \"two\"}\n"),
            ("src/(group)/a.ts", "export const helper = 1;\n"),
            ("src/b.ts", "export const helper = 2;\n"),
            ("helper/README.md", "notes\n"),
        ],
    )
}

#[test]
fn a_target_in_matching_quotes_answers_as_without_them() {
    let ws = scan(&fixture("simple-python-project"));
    assert_eq!(
        query(&ws, "'shop.users'").output,
        query(&ws, "shop.users").output
    );
    assert_eq!(query(&ws, "\"shop.users\"").found, Found::One);
}

#[test]
fn an_absolute_target_answers_as_its_relative_form() {
    let root = fixture("simple-python-project");
    let ws = scan(&root);
    let absolute = std::fs::canonicalize(root.join("src/shop/users.py")).unwrap();
    let absolute = absolute.to_str().unwrap();
    assert_eq!(
        query(&ws, absolute).output,
        query(&ws, "src/shop/users.py").output
    );
    let without_request = |answer: Answer| {
        answer
            .output
            .lines()
            .filter(|l| !l.contains("\"requested\""))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        without_request(impact(&ws, absolute)),
        without_request(impact(&ws, "src/shop/users.py"))
    );
}

#[test]
fn a_name_several_components_share_gives_their_ids_and_paths() {
    let repo = two_tests_packages("shared-name");
    let ws = scan(&repo.0);
    let answer = query(&ws, "tests");
    assert_eq!(answer.found, Found::Candidates, "{}", answer.output);
    assert_eq!(
        answer.output,
        "`tests` names 2 components; query one of them by id or path:\n  \
         a::tests  a/tests  module\n  b::tests  b/tests  module\n"
    );
    // impact answers in JSON
    let impact = impact(&ws, "tests");
    assert_eq!(impact.found, Found::Candidates);
    let value: serde_json::Value = serde_json::from_str(&impact.output).unwrap();
    assert_eq!(value["total"], 2);
    assert_eq!(value["candidates"][0]["id"], "a::tests");
    assert_eq!(value["candidates"][1]["path"], "b/tests");
    // a path written as one picks a component
    assert_eq!(query(&ws, "./a/tests").found, Found::One);
}

#[test]
fn query_and_impact_list_the_same_candidates_of_every_kind() {
    let repo = helpers_beside_a_directory("every-kind");
    let ws = scan(&repo.0);
    let query = query(&ws, "helper");
    let impact = impact(&ws, "helper");
    assert_eq!(query.found, Found::Candidates);
    assert_eq!(impact.found, Found::Candidates);
    // impact answers in JSON, as query does when asked for it
    assert_eq!(query_as(&ws, "helper", Format::Json).output, impact.output);
    for expected in [
        "`helper` names 2 symbols and a directory; query one of them by id or path:\n",
        "\n  'two::src/(group)/a.ts::helper'  src/(group)/a.ts:1  constant\n",
        // no import anywhere: no names recorded, so no counts
        "\n  two::src/b.ts::helper  src/b.ts:1  constant\n",
        "\n  ./helper  directory\n",
    ] {
        assert!(
            query.output.contains(expected),
            "missing {expected:?} in:\n{}",
            query.output
        );
    }
}

#[test]
fn candidates_in_json_say_what_each_one_is() {
    let repo = helpers_beside_a_directory("json-candidates");
    let ws = scan(&repo.0);
    let answer = query_as(&ws, "helper", Format::Json);
    assert_eq!(answer.found, Found::Candidates);
    let value: serde_json::Value = serde_json::from_str(&answer.output).unwrap();
    assert_eq!(value["requested"], "helper");
    assert_eq!(value["total"], 3);
    let kinds: Vec<&str> = value["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["symbol", "symbol", "directory"]);
    assert_eq!(value["candidates"][1]["id"], "two::src/b.ts::helper");
    assert_eq!(value["candidates"][1]["file"], "src/b.ts");
    assert_eq!(value["candidates"][1]["line"], 1);
    assert_eq!(value["candidates"][2]["path"], "helper");
}

#[test]
fn an_id_of_a_component_and_of_another_symbol_gives_both() {
    let repo = Repo::new(
        "id-collision",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
            ),
            ("src/lib.rs", "mod config;\n\npub fn config() {}\n"),
            ("src/config.rs", "pub fn load() {}\n"),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "pkg::config");
    assert_eq!(answer.found, Found::Candidates, "{}", answer.output);
    assert!(
        answer
            .output
            .starts_with("`pkg::config` names a component and a symbol;"),
        "{}",
        answer.output
    );
}

#[test]
fn a_module_and_its_own_symbol_are_one_target() {
    // `pub mod invoice;` is both a component and a symbol of one id
    let ws = scan(&fixture("simple-rust-workspace"));
    assert_eq!(query(&ws, "lib_core::billing::invoice").found, Found::One);
}

#[test]
fn a_name_with_a_slash_that_is_also_another_path_gives_both() {
    let repo = Repo::new(
        "slash-collision",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            (
                "src/components/button/index.ts",
                "export const button = 1;\n",
            ),
            ("src/components/button/size.ts", "export const size = 2;\n"),
            ("components/button/README.md", "notes\n"),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "components/button");
    assert_eq!(answer.found, Found::Candidates, "{}", answer.output);
    assert!(
        answer
            .output
            .contains("\n  ./components/button  directory\n"),
        "{}",
        answer.output
    );
}

#[test]
fn a_bare_file_at_the_root_answers_for_that_file() {
    let repo = Repo::new(
        "root-file",
        &[
            (
                "pyproject.toml",
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n",
            ),
            ("manage.py", "import app.core\n"),
            ("app/__init__.py", ""),
            ("app/core.py", "def run():\n    pass\n"),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "manage.py");
    assert_eq!(answer.found, Found::One);
    assert!(
        answer.output.starts_with("manage.py (file)"),
        "{}",
        answer.output
    );
}

#[test]
fn a_package_subpath_answers_for_its_package() {
    let ws = scan(&fixture("simple-ts-project"));
    let answer = query(&ws, "react-dom/client");
    assert_eq!(answer.found, Found::One);
    assert!(
        answer.output.starts_with(
            "react-dom (external, typescript), depth 2\nid: ext:npm:react-dom\nsubpath: client\n"
        ),
        "{}",
        answer.output
    );
    let json = impact(&ws, "react-dom/client").output;
    assert!(json.contains("\"target\": \"ext:npm:react-dom\""), "{json}");
    assert!(json.contains("\"subpath\": \"client\""), "{json}");
}

#[test]
fn a_subpath_of_a_workspace_package_answers_for_that_package() {
    let repo = Repo::new(
        "internal-subpath",
        &[
            (
                "package.json",
                "{\"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"]}\n",
            ),
            ("packages/ui/package.json", "{\"name\": \"@acme/ui\"}\n"),
            ("packages/ui/src/button.ts", "export const button = 1;\n"),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "@acme/ui/button");
    assert_eq!(answer.found, Found::One, "{}", answer.output);
    assert!(
        answer.output.starts_with("@acme/ui (package"),
        "{}",
        answer.output
    );
    assert!(
        answer.output.contains("\nsubpath: button\n"),
        "{}",
        answer.output
    );
}

#[test]
fn impact_on_an_import_name_follows_the_files_that_import_it() {
    let ws = scan(&fixture("simple-python-project"));
    let answer = impact(&ws, "pytest");
    assert_eq!(answer.found, Found::One);
    let value: serde_json::Value = serde_json::from_str(&answer.output).unwrap();
    assert_eq!(value["module"], "pytest", "{}", answer.output);
    // the key stays, so every answer has one shape
    assert!(
        value.as_object().unwrap().contains_key("target"),
        "{}",
        answer.output
    );
    assert!(value["target"].is_null(), "{}", answer.output);
    assert_eq!(value["direct"], serde_json::json!(["shop::scripts"]));
    assert_eq!(value["importers"]["shown"][0]["file"], "scripts/report.py");
    assert_eq!(value["importers"]["shown"][0]["line"], 2);
}

#[test]
fn a_file_name_answers_when_one_file_has_it() {
    let ws = scan(&fixture("simple-python-project"));
    let by_name = query(&ws, "users.py");
    assert_eq!(by_name.found, Found::One);
    assert_eq!(by_name.output, query(&ws, "src/shop/users.py").output);
}

#[test]
fn a_file_name_several_files_have_gives_them_all() {
    let repo = Repo::new(
        "file-names",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            ("src/a/actions.ts", "export const a = 1;\n"),
            ("src/b/actions.ts", "export const b = 2;\n"),
        ],
    );
    let ws = scan(&repo.0);
    for target in ["actions.ts", "actions"] {
        let answer = query(&ws, target);
        assert_eq!(answer.found, Found::Candidates, "{}", answer.output);
        assert!(
            answer.output.contains("\n  src/a/actions.ts  file\n"),
            "{}",
            answer.output
        );
        assert!(
            answer.output.contains("\n  src/b/actions.ts  file\n"),
            "{}",
            answer.output
        );
    }
}

#[test]
fn a_missing_path_above_the_root_is_looked_up_as_a_name() {
    let ws = scan(&fixture("simple-python-project"));
    let err = ws
        .query(&QueryRequest {
            target: "../nope",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: false,
        })
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "no component, file, symbol or import named `../nope`"
    );
}

#[test]
fn a_missing_file_under_a_python_package_is_no_package_subpath() {
    // `app` is a Python package; `app/nowhere.py` names a file that is not there
    let ws = scan(&fixture("mixed-utils-project"));
    let err = ws
        .impact(&ImpactRequest {
            target: "app/nowhere.py",
            depth: DEFAULT_DEPTH,
            verbose: false,
        })
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "no component, file, symbol or import named `app/nowhere.py`"
    );
}

#[test]
fn production_files_come_before_test_files_among_candidates() {
    let repo = Repo::new(
        "test-files-last",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            (
                "src/__tests__/actions.test.ts",
                "import { b } from '../b/actions';\nexport const t = b;\n",
            ),
            ("src/b/actions.ts", "export const b = 2;\n"),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "actions");
    assert_eq!(answer.found, Found::Candidates, "{}", answer.output);
    let files: Vec<&str> = answer
        .output
        .lines()
        .filter(|l| l.ends_with("  file"))
        .collect();
    assert_eq!(
        files,
        [
            "  src/b/actions.ts  file",
            "  src/__tests__/actions.test.ts  file"
        ]
    );
}
