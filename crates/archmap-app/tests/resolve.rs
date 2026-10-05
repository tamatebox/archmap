//! One resolver for `query` and `impact`: every form a target can take,
//! and candidates instead of an error when it names several things.

use std::path::{Path, PathBuf};

use archmap_app::{
    Answer, BySymbolRequest, Format, Found, ImpactRequest, QueryRequest, ScanMode, Workspace,
    DEFAULT_DEPTH,
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
    impact_as(ws, target, Format::Json)
}

fn impact_as(ws: &Workspace, target: &str, format: Format) -> Answer {
    ws.impact(&ImpactRequest {
        target,
        depth: DEFAULT_DEPTH,
        format,
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
    assert_eq!(
        impact_as(&ws, absolute, Format::Text).output,
        impact_as(&ws, "src/shop/users.py", Format::Text).output
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
        "`tests` names 2 components; retry with one of them by id or path:\n  \
         a::tests  a/tests  module\n  b::tests  b/tests  module\n"
    );
    // impact lists them the same way, and in JSON when asked to
    let text = impact_as(&ws, "tests", Format::Text);
    assert_eq!(text.found, Found::Candidates);
    assert_eq!(text.output, answer.output);
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
    assert_eq!(impact_as(&ws, "helper", Format::Text).output, query.output);
    assert_eq!(query_as(&ws, "helper", Format::Json).output, impact.output);
    for expected in [
        "`helper` names 2 symbols and a directory; retry with one of them by id or path:\n",
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
    // the form to retry with, as the text shows it
    assert_eq!(value["candidates"][2]["path"], "./helper");
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
fn a_package_subpath_lists_only_the_statements_that_import_it() {
    let repo = Repo::new(
        "subpath-importers",
        &[
            (
                "package.json",
                "{\"name\": \"site\", \"dependencies\": {\"react-dom\": \"19.0.0\"}}\n",
            ),
            (
                "src/client/mount.ts",
                "import { createRoot } from 'react-dom/client';\nexport const mount = createRoot;\n",
            ),
            (
                "src/server/render.ts",
                "import { renderToString } from 'react-dom/server';\nexport const render = renderToString;\n",
            ),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "react-dom/client");
    assert!(
        answer.output.contains("src/client/mount.ts:1"),
        "{}",
        answer.output
    );
    assert!(!answer.output.contains("src/server"), "{}", answer.output);
    // the package's declaration stays
    assert!(
        answer.output.contains("declared in package.json"),
        "{}",
        answer.output
    );

    let json = query_as(&ws, "react-dom/client", Format::Json).output;
    assert!(
        json.contains("\"note\": \"import react-dom/client\""),
        "{json}"
    );
    assert!(!json.contains("react-dom/server"), "{json}");

    let json = impact(&ws, "react-dom/client").output;
    assert!(json.contains("src/client/mount.ts"), "{json}");
    assert!(!json.contains("src/server"), "{json}");
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
    assert_eq!(value["direct"][0]["id"], "shop::scripts");
    assert_eq!(value["direct"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["importers"]["statements"][0]["file"],
        "scripts/report.py"
    );
    assert_eq!(value["importers"]["statements"][0]["line"], 2);
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
        "no component, file, symbol or import named `../nope`: a path names a file or directory \
         from the root; query the directory above it to see what is there"
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
            format: Format::Text,
            verbose: false,
        })
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "no component, file, symbol or import named `app/nowhere.py`: a path names a file or directory \
         from the root; query the directory above it to see what is there"
    );
}

/// A TS/JS package with a directory component under `components/` and one
/// under `lib/` that a test file shares a stem with.
fn pantry_repo(name: &str) -> Repo {
    Repo::new(
        name,
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            (
                "components/pantry/shelf.ts",
                "export function shelfLabel() { return 'x'; }\nexport const hold_until = 1;\n",
            ),
            (
                "components/pantry/editor.ts",
                "import { shelfLabel } from './shelf';\nexport const editor = shelfLabel;\n",
            ),
            ("lib/notify/index.ts", "export function send() {}\n"),
            ("lib/notify/queue.ts", "export const queue = 1;\n"),
            (
                "lib/__tests__/notify.test.ts",
                "import { send } from '../notify';\nsend();\n",
            ),
        ],
    )
}

#[test]
fn a_bare_word_finds_a_directory_component_by_its_last_segment() {
    let repo = pantry_repo("last-segment");
    let ws = scan(&repo.0);
    let answer = query(&ws, "pantry");
    assert_eq!(answer.found, Found::One, "{}", answer.output);
    assert!(
        answer.output.starts_with("components/pantry (module"),
        "{}",
        answer.output
    );

    // a test file with that stem is one more candidate, not the answer
    let answer = query(&ws, "notify");
    assert_eq!(answer.found, Found::Candidates, "{}", answer.output);
    assert!(
        answer
            .output
            .contains("\n  web::lib/notify  lib/notify  module\n"),
        "{}",
        answer.output
    );
    assert!(
        answer
            .output
            .contains("\n  lib/__tests__/notify.test.ts  file\n"),
        "{}",
        answer.output
    );
    let json = query_as(&ws, "notify", Format::Json).output;
    assert!(json.contains("\"match\": \"segment\""), "{json}");

    // the same in Python: a package by the last part of its dotted name
    let ws = scan(&fixture("simple-python-project"));
    let answer = query(&ws, "billing");
    assert_eq!(answer.found, Found::One, "{}", answer.output);
    assert!(
        answer.output.starts_with("shop.billing (module"),
        "{}",
        answer.output
    );
}

#[test]
fn names_that_contain_a_word_that_names_nothing_are_candidates() {
    let repo = pantry_repo("contains");
    let ws = scan(&repo.0);
    for target in ["shelflabel", "Label", "LABEL", "shelf_label", "holdUntil"] {
        let answer = query(&ws, target);
        assert_eq!(
            answer.found,
            Found::Candidates,
            "{target}: {}",
            answer.output
        );
        assert!(
            answer.output.starts_with(&format!(
                "No name is `{target}`. Names that contain it, ignoring case: "
            )),
            "{target}: {}",
            answer.output
        );
    }
    let answer = query(&ws, "label");
    assert!(
        answer.output.contains("\n  web::components/pantry/shelf.ts::shelfLabel  components/pantry/shelf.ts:1  function"),
        "{}",
        answer.output
    );
    let json = query_as(&ws, "label", Format::Json).output;
    assert!(json.contains("\"match\": \"contains\""), "{json}");

    // equal ignoring case ranks first, before what only contains it
    let answer = query(&ws, "QUEUE");
    let first = answer.output.lines().nth(1).unwrap_or_default();
    assert!(first.contains("::queue "), "{}", answer.output);

    // too short to look for
    let err = ws
        .query(&QueryRequest {
            target: "zq",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: false,
        })
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "no component, file, symbol or import named `zq` (the names that contain a word are \
         looked for from 3 characters)"
    );

    // nothing contains it either: the error says what to do next
    let err = ws
        .impact(&ImpactRequest {
            target: "zebra",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: false,
        })
        .err()
        .unwrap();
    assert!(
        err.to_string().starts_with(
            "no component, file, symbol or import named `zebra`, nor a name that contains it"
        ),
        "{err}"
    );
}

#[test]
fn statements_say_which_names_they_take() {
    let repo = Repo::new(
        "taken-names",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            (
                "src/m.ts",
                "export const a = 1, b = 2, c = 3, d = 4, e = 5;\n",
            ),
            (
                "src/u.ts",
                "import { a, b, c, d, e } from './m';\nexport const u = a + b + c + d + e;\n",
            ),
            (
                "src/w.ts",
                "import * as m from './m';\nexport const w = m.a;\n",
            ),
            (
                "src/one.ts",
                "import { a } from './m';\nexport const one = a;\n",
            ),
            (
                "src/k.ts",
                "export const B_CONST = 1;\nexport function alpha() {}\n",
            ),
            (
                "src/kk.ts",
                "import { B_CONST, alpha } from './k';\nexport const kk = [B_CONST, alpha];\n",
            ),
        ],
    );
    let ws = scan(&repo.0);
    // in their order ignoring case
    let answer = query(&ws, "src/kk.ts");
    assert!(
        answer.output.contains("-> src/k.ts (names alpha, B_CONST)"),
        "{}",
        answer.output
    );
    // every name when verbose, and the capped text says it is capped
    let every = ws
        .query(&QueryRequest {
            target: "src/m.ts",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: true,
        })
        .unwrap()
        .output;
    assert!(
        every.contains("src/u.ts:1 (names a, b, c, d, e)"),
        "{every}"
    );
    let answer = query(&ws, "src/m.ts");
    assert!(
        answer.output.contains("Lists are capped"),
        "{}",
        answer.output
    );
    for line in [
        "src/u.ts:1 (names a, b, c, +2 more)",
        "src/w.ts:1 (whole module)",
        "src/one.ts:1 (names a)",
        "Marks: (names a, b) the names it takes; (whole module) takes the module whole",
    ] {
        assert!(answer.output.contains(line), "{line}: {}", answer.output);
    }
    let answer = query(&ws, "src/one.ts");
    assert!(
        answer.output.contains("src/one.ts:1 -> src/m.ts (names a)"),
        "{}",
        answer.output
    );
    let answer = impact_as(&ws, "src/m.ts", Format::Text);
    assert!(
        answer.output.contains("  src/one.ts:1 (names a)"),
        "{}",
        answer.output
    );
    // a symbol's importers take it: no names
    let answer = impact_as(&ws, "web::src/m.ts::e", Format::Text);
    assert!(!answer.output.contains("(names"), "{}", answer.output);
}

#[test]
fn a_file_says_its_react_directive_and_a_client_s_import_of_server_functions() {
    let repo = Repo::new(
        "directives",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            (
                "app/actions.ts",
                "'use server';\nexport async function save() {}\n",
            ),
            (
                "components/editor.tsx",
                "'use client';\nimport { save } from '../app/actions';\nexport const editor = save;\n",
            ),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "app/actions.ts");
    for line in [
        "\ndirective: \"use server\"\n",
        "components/editor.tsx:2 (names save) (server reference)",
        "(server reference) calls server functions, loads no code",
    ] {
        assert!(answer.output.contains(line), "{line}: {}", answer.output);
    }
    let json = query_as(&ws, "components/editor.tsx", Format::Json).output;
    assert!(json.contains("\"directive\": \"use client\""), "{json}");
    assert!(json.contains("\"server_reference\": true"), "{json}");
}

/// A package `kit` whose subpaths give `refresh` and `route`, and a second
/// package `other` that gives `route` too.
fn packages_repo(name: &str) -> Repo {
    Repo::new(
        name,
        &[
            (
                "package.json",
                "{\"name\": \"web\", \"dependencies\": {\"kit\": \"1.0.0\", \"other\": \"1.0.0\"}}\n",
            ),
            (
                "src/save.ts",
                "import { refresh } from 'kit/cache';\nimport Link from 'kit/link';\nexport function save() { refresh('/'); return Link; }\n",
            ),
            (
                "src/bust.js",
                "const cache = require('kit/cache');\nconst nav = require('kit/nav');\nmodule.exports = () => cache.refresh('/');\n",
            ),
            ("src/relay.ts", "export { refresh } from 'kit/cache';\n"),
            ("src/a.ts", "import { route } from 'kit/nav';\nexport const a = route;\n"),
            ("src/b.ts", "import { route } from 'other';\nexport const b = route;\n"),
            (
                "tests/save.test.ts",
                "import { refresh } from 'kit/cache';\nrefresh('/');\n",
            ),
        ],
    )
}

#[test]
fn a_name_taken_from_a_package_answers_with_its_statements_and_uses() {
    let repo = packages_repo("package-names");
    let ws = scan(&repo.0);
    let answer = query(&ws, "refresh");
    assert_eq!(answer.found, Found::One, "{}", answer.output);
    for line in [
        "refresh (a name taken from kit), depth 2\nid: ext:npm:kit::refresh\n",
        "\nImported by: 3 (from kit/cache; 1 in tests; 1 re-export)\n  src/relay.ts:1 (export)\n  src/save.ts:1\n  tests/save.test.ts:1 (test)\n",
        // a module taken whole, of the subpath the name comes from only
        "\nMay use: 1 (imports the whole module)\n  src/bust.js:1\n",
        "src/save.ts:3 (call)",
        "src/bust.js:3 (call) as cache.refresh",
        "relays: 1 statement passes the name on from the package, and what imports it from \
         their files is not read: src/relay.ts:1",
    ] {
        assert!(answer.output.contains(line), "{line}: {}", answer.output);
    }
    // the id form gives the same, a default import only through it
    let by_id = query(&ws, "ext:npm:kit::refresh");
    assert_eq!(by_id.output, answer.output);
    assert!(
        query(&ws, "ext:npm:kit::default")
            .output
            .contains("src/save.ts:2"),
        "default by id"
    );
    let default = ws.query(&QueryRequest {
        target: "default",
        depth: DEFAULT_DEPTH,
        format: Format::Text,
        verbose: false,
    });
    assert!(
        default.map_or(true, |a| !a.output.contains("a name taken from")),
        "a bare `default` is no package name"
    );
    // two packages give it: candidates to retry by id
    let answer = query(&ws, "route");
    assert_eq!(answer.found, Found::Candidates, "{}", answer.output);
    assert!(
        answer
            .output
            .contains("\n  ext:npm:kit::route  a name taken from kit  imported by 1\n"),
        "{}",
        answer.output
    );
    let json = query_as(&ws, "route", Format::Json).output;
    assert!(json.contains("\"id\": \"ext:npm:kit::route\""), "{json}");
    // a package's importers say what each statement takes
    let answer = query(&ws, "kit/cache");
    assert!(
        answer.output.contains("src/save.ts:1 (names refresh)"),
        "{}",
        answer.output
    );
    // a word that only part of it holds finds it too
    let answer = query(&ws, "refre");
    assert!(
        answer
            .output
            .contains("ext:npm:kit::refresh  a name taken from kit  imported by 3"),
        "{}",
        answer.output
    );
    // impact follows the statements as for an import name
    let answer = impact_as(&ws, "refresh", Format::Text);
    assert!(
        answer
            .output
            .starts_with("refresh (a name taken from kit), depth 2\n"),
        "{}",
        answer.output
    );
    assert!(
        answer.output.contains("\n  src/save.ts:1\n"),
        "{}",
        answer.output
    );
}

#[test]
fn an_environment_variable_answers_with_where_the_code_reads_and_writes_it() {
    let repo = Repo::new(
        "env",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            (
                "src/region.ts",
                "export const region = process.env.APP_REGION ?? 'eu';\nexport const { APP_REGION: r } = process.env;\n",
            ),
            (
                "src/app.ts",
                "import { region } from './region';\nexport const app = region;\n",
            ),
            (
                "src/all.ts",
                "export const copy = { ...process.env };\nexport const pick = (k: string) => process.env[k];\n",
            ),
            (
                "tests/region.test.ts",
                "import { vi } from 'vitest';\nvi.stubEnv('APP_REGION', 'us');\n// process.env.APP_REGION in a comment\n",
            ),
            ("tools/run.py", "import os\nos.environ['APP_REGION']\n"),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "APP_REGION");
    assert_eq!(answer.found, Found::One, "{}", answer.output);
    for line in [
        "APP_REGION (environment variable)\nid: env:APP_REGION\n",
        "\nRead at: 2 in 1 file\n  src/region.ts:1, src/region.ts:2\n",
        "\nWritten at: 1 in 1 file\n  tests/region.test.ts:2 (test)\n",
        "computed keys: 1 place reads the environment by a computed key, which may be this one: \
         src/all.ts:2",
        "whole environment: 1 place takes the environment whole, and what takes it may read this \
         one: src/all.ts:1",
        "  set: where its value is set is not read",
        "other languages: their reads of the environment are not read: python",
    ] {
        assert!(answer.output.contains(line), "{line}: {}", answer.output);
    }
    assert_eq!(query(&ws, "env:APP_REGION").output, answer.output);
    let json = query_as(&ws, "APP_REGION", Format::Json).output;
    assert!(json.contains("\"reads\": ["), "{json}");
    assert!(json.contains("\"note\": \"vi.stubEnv\""), "{json}");

    // a name in capitals that the code never reads is no variable
    let answer = ws.query(&QueryRequest {
        target: "NEVER_SET",
        depth: DEFAULT_DEPTH,
        format: Format::Text,
        verbose: false,
    });
    assert!(answer.is_err(), "{:?}", answer.map(|a| a.output));
    // by its id it answers anyway, with none found
    assert!(
        query(&ws, "env:NEVER_SET")
            .output
            .contains("\nRead at: none found\n"),
        "env id"
    );

    // impact starts from the files that read it
    let answer = impact_as(&ws, "APP_REGION", Format::Text);
    for line in [
        "\nDirect dependents: 1\n  region.ts  2 reads\n",
        "\nRead at: 2\n  src/region.ts:1\n  src/region.ts:2\n",
        "\n  app.ts  2 steps, through src/region.ts\n",
    ] {
        assert!(answer.output.contains(line), "{line}: {}", answer.output);
    }
}

#[test]
fn symbols_that_share_a_name_are_one_candidate_and_verbose_lists_every_one() {
    let mut files: Vec<(String, String)> =
        vec![("package.json".into(), "{\"name\": \"web\"}\n".into())];
    for i in 0..3 {
        files.push((format!("src/t{i}.ts"), "export const tick = 1;\n".into()));
    }
    for i in 0..12 {
        files.push((
            format!("src/u{i}.ts"),
            format!("export const tickle{i} = 1;\n"),
        ));
    }
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(f, t)| (f.as_str(), t.as_str()))
        .collect();
    let repo = Repo::new("shared-names", &files);
    let ws = scan(&repo.0);
    let answer = query(&ws, "tic");
    let first = answer.output.lines().nth(1).unwrap_or_default();
    assert_eq!(
        first, "  tick  3 symbols of that name (query tick)",
        "{}",
        answer.output
    );
    assert!(answer.output.contains("showing 10"), "{}", answer.output);
    let json = query_as(&ws, "tic", Format::Json).output;
    assert!(json.contains("\"kind\": \"symbols\""), "{json}");
    assert!(json.contains("\"count\": 3"), "{json}");
    let every = ws
        .query(&QueryRequest {
            target: "tic",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: true,
        })
        .unwrap()
        .output;
    assert_eq!(
        every.lines().filter(|l| l.starts_with("  ")).count(),
        13,
        "{every}"
    );
}

#[test]
fn a_file_by_symbol_says_who_takes_each_symbol_and_where_it_is_used() {
    let repo = Repo::new(
        "by-symbol",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            (
                "src/m.ts",
                "export const a = 1, b = 2, c = 3, d = 4, e = 5;\nexport const self = a + 1;\n",
            ),
            (
                "src/u.ts",
                "import { a, b, c, d, e } from './m';\nexport const u = a + b + c + d + e;\n",
            ),
            (
                "src/w.ts",
                "import * as m from './m';\nexport const w = m.a;\n",
            ),
            (
                "src/one.ts",
                "import { a } from './m';\nexport const one = a;\n",
            ),
            ("src/dead.ts", "import { d } from './m';\n"),
            (
                "tests/m.test.ts",
                "import { e } from '../src/m';\ntest('e', () => e);\n",
            ),
        ],
    );
    let ws = scan(&repo.0);
    let by_symbol = |target: &str, format: Format| {
        ws.by_symbol(&BySymbolRequest {
            target,
            depth: DEFAULT_DEPTH,
            format,
            verbose: false,
        })
    };
    let answer = by_symbol("src/m.ts", Format::Text).unwrap();
    assert!(
        answer.output.contains(
            "\nPublic symbols and their uses: 6\n\
             \x20 a     imported by 2; may use 1; used at 4 in 4 files, 1 in this file\n\
             \x20 b     imported by 1; may use 1; used at 1 in 1 file\n\
             \x20 c     imported by 1; may use 1; used at 1 in 1 file\n\
             \x20 d     imported by 2; may use 1; used at 1 in 1 file\n\
             \x20 e     imported by 2, 1 in tests; may use 1; used at 2 in 2 files, 1 in tests\n\
             \x20 self  may use 1; no use found\n"
        ),
        "{}",
        answer.output
    );
    let json = by_symbol("src/m.ts", Format::Json).unwrap().output;
    assert!(json.contains("\"symbols\": ["), "{json}");
    assert!(json.contains("\"used_at\": {"), "{json}");
    // a component of several files is no file
    let err = by_symbol("src", Format::Text).err().unwrap();
    assert!(
        err.to_string().contains("by symbol takes one file"),
        "{err}"
    );
}

#[test]
fn names_of_test_code_that_contain_a_word_come_after_production_ones() {
    let repo = Repo::new(
        "contains-tests",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            ("src/lib/deep/helpers/a.ts", "export const a = 1;\n"),
            ("src/lib/deep/helpers/b.ts", "export const b = 1;\n"),
            ("tests/helpers/c.ts", "export const c = 1;\n"),
            ("tests/helpers/d.ts", "export const d = 1;\n"),
        ],
    );
    let ws = scan(&repo.0);
    let answer = query(&ws, "helper");
    assert_eq!(answer.found, Found::Candidates, "{}", answer.output);
    let rows: Vec<&str> = answer.output.lines().skip(1).take(2).collect();
    assert_eq!(
        rows,
        [
            "  web::src/lib/deep/helpers  src/lib/deep/helpers  module",
            "  web::tests/helpers  tests/helpers  module",
        ],
        "{}",
        answer.output
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

#[test]
fn a_root_reached_through_a_symlink_answers_alike_in_every_form() {
    let real = Repo::new(
        "symlinked-real",
        &[
            ("pkg/__init__.py", "from pkg import core\n"),
            ("pkg/core.py", "def run():\n    pass\n"),
        ],
    );
    let link =
        std::env::temp_dir().join(format!("archmap-app-symlinked-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real.0, &link).unwrap();
    #[cfg(not(unix))]
    return;
    let via_link = scan(&link);
    let direct = scan(&real.0);
    let expected = query(&direct, "pkg/core.py").output;
    let canonical = std::fs::canonicalize(real.0.join("pkg/core.py")).unwrap();
    for (ws, target) in [
        (&via_link, "pkg/core.py".to_owned()),
        (&via_link, link.join("pkg/core.py").display().to_string()),
        (&via_link, canonical.display().to_string()),
        (&direct, canonical.display().to_string()),
    ] {
        assert_eq!(query(ws, &target).output, expected, "{target}");
    }
    let _ = std::fs::remove_file(&link);
}
