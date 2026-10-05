//! Where a TS/JS symbol is used, read on demand from `fixtures/ts-uses`.

use std::path::{Path, PathBuf};

use archmap_core::SymbolUses;
use archmap_scan::{scan, symbol_uses, ScanOptions, ScanReport};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-uses")
}

fn graph() -> ScanReport {
    scan(&fixture(), &ScanOptions::default()).unwrap()
}

fn uses_of(report: &ScanReport, name: &str) -> SymbolUses {
    let symbol = report
        .graph
        .symbols_named(name)
        .next()
        .unwrap_or_else(|| panic!("no symbol {name}"));
    symbol_uses(report, symbol)
}

/// Each use as `file:line:column role`, with ` as <binding>`, ` via
/// <statement line>` and ` (test)` when they apply.
fn shown(found: &SymbolUses) -> Vec<String> {
    found
        .uses
        .iter()
        .map(|u| {
            let mut line = format!(
                "{}:{}:{} {}",
                u.evidence.file,
                u.evidence.line.unwrap_or(0),
                u.column,
                u.role.as_str()
            );
            if let Some(binding) = &u.binding {
                line.push_str(&format!(" as {binding}"));
            }
            if let Some(statement) = &u.statement {
                line.push_str(&format!(" via {}", statement.line));
            }
            if u.evidence.test {
                line.push_str(" (test)");
            }
            line
        })
        .collect()
}

fn places(list: &[archmap_core::Evidence]) -> Vec<String> {
    list.iter()
        .map(|e| format!("{}:{}", e.file, e.line.unwrap_or(0)))
        .collect()
}

#[test]
fn a_function_is_used_through_every_binding_that_reaches_it() {
    let graph = graph();
    let found = uses_of(&graph, "formatPrice");
    assert_eq!(
        shown(&found),
        [
            "scripts/cjs.cjs:4:23 call via 1",
            "scripts/cjs.cjs:4:48 call as money.formatPrice via 2",
            "scripts/lazy.mjs:3:10 call via 2",
            // `export { formatPrice as fp3 }` in the defining file
            "src/aliased.ts:3:29 call as all.fp3 via 1",
            // the destructuring binds `g`, as an import binds a name
            "src/app.ts:11:11 call as fp via 1",
            "src/app.ts:11:20 call as m.formatPrice via 2",
            "src/app.ts:11:38 call as m.formatPrice via 2",
            "src/app.ts:11:57 call as g via 2",
            // `as any` and `!` change no value
            "src/cast.ts:3:32 call as m.formatPrice via 1",
            "src/cast.ts:4:24 call as m.formatPrice via 1",
            "src/hoisted.ts:1:28 call via 3",
            // the module's type holds every export's (`typeof import(..)`,
            // `typeof m`)
            "src/importtype.ts:1:28 type via 1",
            "src/money.ts:10:12 call",
            // `import { formatPrice }; export { formatPrice as fmt2 }`
            "src/relayed.ts:3:24 call as fmt2 via 1",
            "src/typeof.ts:3:26 type as m via 1",
            "src/types.ts:4:50 type via 4",
            "src/view.tsx:9:27 call as money.formatPrice via 1",
            "src/view.tsx:10:21 call via 1",
            "src/view.tsx:11:34 call as all.money.formatPrice via 2",
            // `typeof import(..)` as the type argument, and the call through
            // `await vi.importActual(..)`
            "tests/actual.test.ts:2:56 type via 2 (test)",
            "tests/actual.test.ts:3:10 call via 2 (test)",
            // a mock stands in for the module, but the call names the symbol
            "tests/mocked.test.ts:8:24 call via 1 (test)",
            "tests/money.test.ts:3:24 call via 1 (test)",
            "tests/partial.test.ts:8:24 call via 1 (test)",
        ],
        "{:#?}",
        shown(&found)
    );
    // `send(m)`, the promise of `import()` and a spread of the real module
    // into a mock may use it unseen
    assert_eq!(
        places(&found.escapes),
        [
            "scripts/lazy.mjs:7",
            "src/app.ts:8",
            "tests/partial.test.ts:4"
        ],
        "{:?}",
        places(&found.escapes)
    );
    // a namespace that reads only another export never names it, while the
    // module's type holds every export's (above)
    assert_eq!(places(&found.unused), ["src/rates.ts:1", "src/unused.ts:1"]);
    assert_eq!(
        places(&found.passed_on),
        ["src/index.ts:1", "src/index.ts:2", "src/relay.ts:1"]
    );
    let renamed: Vec<(String, &str)> = found
        .renamed
        .iter()
        .map(|r| {
            (
                format!("{}:{}", r.evidence.file, r.evidence.line.unwrap_or(0)),
                r.name.as_str(),
            )
        })
        .collect();
    assert_eq!(renamed, [("src/index.ts:3".to_owned(), "price")]);
    // two statements on one line that load different files
    let unread: Vec<String> = found
        .unread
        .iter()
        .map(|u| format!("{}:{} {}", u.file, u.line.unwrap_or(0), u.reason.as_str()))
        .collect();
    assert_eq!(unread, ["src/two.ts:1 ambiguous statement"]);
}

#[test]
fn every_statement_read_ends_in_one_of_the_lists() {
    for name in [
        "ts-uses",
        "ts-reexports",
        "simple-ts-project",
        "ts-module-value",
        "ts-static-members",
    ] {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name);
        let report = scan(&root, &ScanOptions::default()).unwrap();
        let graph = &report.graph;
        for symbol in graph.symbols.values() {
            let Some(importers) = graph.symbol_importers(symbol) else {
                continue;
            };
            let found = symbol_uses(&report, symbol);
            let defining = symbol.location().map(|e| e.file.as_str());
            for (_, statement) in importers.by_name.iter().chain(&importers.may_use) {
                let (file, line) = (statement.file.as_str(), statement.line);
                let note = statement.note.as_deref().unwrap_or("");
                // a mock call takes no name the code uses, unless it stands
                // in for the module
                let helper = [
                    "jest.requireActual",
                    "jest.requireMock",
                    "vi.importActual",
                    "vi.importMock",
                ]
                .contains(&note);
                let mock = note.contains('.') && !helper && !statement.replaces;
                if Some(file) == defining || mock {
                    continue;
                }
                let at = |e: &archmap_core::Evidence| e.file == file && e.line == line;
                let ended = found.uses.iter().any(|u| {
                    u.statement
                        .as_ref()
                        .is_some_and(|s| s.file == file && Some(s.line) == line)
                }) || found.unused.iter().any(at)
                    || found.passed_on.iter().any(at)
                    || found.values.iter().any(at)
                    || found.renamed.iter().any(|r| at(&r.evidence))
                    || found.escapes.iter().any(|e| e.file == file)
                    || found.mocked.iter().any(|e| e.file == file)
                    || found
                        .unread
                        .iter()
                        .any(|u| u.file == file && (u.line.is_none() || u.line == line));
                assert!(
                    ended,
                    "{name}: {} at {file}:{line:?} ({note}) ends in no list: {found:#?}",
                    symbol.id
                );
            }
        }
    }
}

#[test]
fn the_keys_of_a_mock_factory_that_name_the_symbol_are_listed_apart_from_uses() {
    let graph = graph();
    let found = uses_of(&graph, "formatPrice");
    // a factory that stands in for the module, and one beside a spread of
    // the real module
    assert_eq!(
        places(&found.mocked),
        ["tests/mocked.test.ts:4", "tests/partial.test.ts:5"]
    );
    assert!(found.mocked.iter().all(|e| e.test));
    // the calls in those files still name it
    let shown = shown(&found);
    for call in ["tests/mocked.test.ts:8:24", "tests/partial.test.ts:8:24"] {
        assert!(shown.iter().any(|u| u.starts_with(call)), "{shown:#?}");
    }
    assert!(found.mocked.iter().all(|e| e.names.is_empty()));
    // a member's class stands in for it, by the class's name
    let pay = uses_of(&graph, "Wallet.pay").mocked;
    assert_eq!(places(&pay), ["tests/mocked.test.ts:5"]);
    assert_eq!(pay[0].names.iter().collect::<Vec<_>>(), ["Wallet"]);
}

#[test]
fn a_class_is_used_by_new_by_its_static_members_and_in_types() {
    let graph = graph();
    assert_eq!(
        shown(&uses_of(&graph, "Wallet")),
        [
            "src/app.ts:9:17 type via 1",
            "src/app.ts:9:26 read via 1",
            "src/app.ts:10:7 new via 1",
            "src/importtype.ts:1:28 type via 1",
            "src/money.ts:6:18 type",
            "src/money.ts:7:16 new",
            "src/money.ts:14:35 type",
            "src/money.ts:20:20 type",
            "src/rich.ts:3:27 read via 1",
            "src/rich.ts:8:19 type via 1",
            "src/rich.ts:13:25 type via 1",
            "src/typeof.ts:3:26 type as m via 1",
            "src/types.ts:3:32 type via 1",
            "tests/actual.test.ts:2:56 type via 2 (test)",
        ]
    );
}

#[test]
fn a_member_is_used_through_its_class_and_through_this_in_its_own_kind() {
    let graph = graph();
    // static: through the class, and `this` in a static method
    let open = uses_of(&graph, "Wallet.open");
    assert_eq!(
        shown(&open),
        [
            "src/app.ts:9:33 call via 1",
            "src/importtype.ts:1:28 type via 1",
            "src/money.ts:21:17 call as this.open",
            "src/typeof.ts:3:26 type as m via 1",
            "tests/actual.test.ts:2:56 type via 2 (test)",
        ]
    );
    // a subclass inherits statics too (`Rich.open()`, `super.open()`): the
    // file that extends the class is no negative fact, while an import of
    // its type only, which never runs, is
    assert_eq!(places(&open.subclasses), ["src/rich.ts:3"]);
    assert_eq!(places(&open.values), ["src/rich.ts:1"]);
    assert!(places(&open.unused).contains(&"src/types.ts:1".to_owned()));
    assert!(!places(&open.unused).contains(&"src/rich.ts:1".to_owned()));
    // instance: `this` in its own methods and their arrows, never in a
    // nested function, and never through a value (`new Wallet().pay(1)`)
    let pay = uses_of(&graph, "Wallet.pay");
    assert_eq!(
        shown(&pay),
        [
            "src/importtype.ts:1:28 type via 1",
            "src/money.ts:13:10 call as this.pay",
            "src/money.ts:17:29 call as this.pay",
            "src/typeof.ts:3:26 type as m via 1",
            "tests/actual.test.ts:2:56 type via 2 (test)",
        ]
    );
    // the class's importers may call it through values (`w?.pay(7)`,
    // `this.pay(6)` in a subclass, `super.pay(5)`): never a negative fact
    assert!(pay.unused.is_empty(), "{:?}", places(&pay.unused));
    let values = places(&pay.values);
    assert!(
        values.contains(&"src/rich.ts:1".to_owned()) && values.contains(&"src/app.ts:1".to_owned()),
        "{values:?}"
    );
}

#[test]
fn jsx_elements_count_once_and_statements_on_one_line_are_not_guessed() {
    let graph = graph();
    let found = uses_of(&graph, "Badge");
    assert_eq!(
        shown(&found),
        [
            "src/view.tsx:9:8 jsx via 3",
            "src/view.tsx:10:8 jsx via 3",
            "src/view.tsx:11:11 jsx as ui.Badge via 4",
        ]
    );
    let unread: Vec<String> = found
        .unread
        .iter()
        .map(|u| format!("{}:{} {}", u.file, u.line.unwrap_or(0), u.reason.as_str()))
        .collect();
    assert_eq!(unread, ["src/two.ts:1 ambiguous statement"]);
}

#[test]
fn a_default_export_and_a_renamed_export_are_used_by_their_local_names() {
    let graph = graph();
    // `export default total` is no use; the default import is
    assert_eq!(
        shown(&uses_of(&graph, "total")),
        [
            "src/app.ts:11:70 call via 3",
            "src/importtype.ts:1:28 type via 1",
            "src/typeof.ts:3:26 type as m via 1",
            "tests/actual.test.ts:2:56 type via 2 (test)",
        ]
    );
    // `export { rates as RATES }`: the uses of `rates`
    assert_eq!(
        shown(&uses_of(&graph, "RATES")),
        [
            "src/importtype.ts:1:28 type via 1",
            "src/money.ts:29:46 read as rates",
            "src/rates.ts:3:26 read as money.RATES via 1",
            "src/typeof.ts:3:26 type as m via 1",
            "tests/actual.test.ts:2:56 type via 2 (test)",
        ]
    );
}

#[test]
fn files_that_changed_since_the_scan_are_unread_with_the_reason() {
    let dir = std::env::temp_dir().join(format!("archmap-uses-stale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let write = |path: &str, text: &str| std::fs::write(dir.join(path), text).unwrap();
    write("package.json", r#"{ "name": "stale" }"#);
    write(
        "src/a.ts",
        "export function f(): number {\n  return 1;\n}\n",
    );
    for name in ["moved", "broken", "gone", "kept"] {
        write(
            &format!("src/{name}.ts"),
            "import { f } from './a';\n\nexport const v = f();\n",
        );
    }
    let report = scan(&dir, &ScanOptions::default()).unwrap();
    let graph = &report.graph;
    // edited after the scan: the statement moved, the file no longer parses,
    // the file is gone
    write(
        "src/moved.ts",
        "// a new first line\nimport { f } from './a';\n\nexport const v = f();\n",
    );
    write(
        "src/broken.ts",
        "import { f } from './a';\nexport const v = f(;\n",
    );
    std::fs::remove_file(dir.join("src/gone.ts")).unwrap();
    let symbol = graph.symbols_named("f").next().unwrap();
    let found = symbol_uses(&report, symbol);
    let unread: Vec<String> = found
        .unread
        .iter()
        .map(|u| format!("{}:{} {}", u.file, u.line.unwrap_or(0), u.reason.as_str()))
        .collect();
    assert_eq!(
        unread,
        [
            "src/broken.ts:0 parse error",
            "src/gone.ts:0 file gone",
            "src/moved.ts:1 statement not found",
        ]
    );
    assert_eq!(shown(&found), ["src/kept.ts:3:18 call via 1"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_type_of_a_module_whole_takes_every_symbol_of_it() {
    let graph = graph();
    // `typeof m`, `typeof import('./money')` and a type argument of that
    // kind hold the type of every export, the symbol's included: no
    // statement of them may end `never named`
    for name in ["formatPrice", "RATES"] {
        let found = uses_of(&graph, name);
        let typed: Vec<String> = shown(&found)
            .into_iter()
            .filter(|u| {
                [
                    "src/typeof.ts",
                    "src/importtype.ts",
                    "tests/actual.test.ts:2",
                ]
                .iter()
                .any(|f| u.starts_with(f))
            })
            .collect();
        assert_eq!(
            typed,
            [
                "src/importtype.ts:1:28 type via 1",
                "src/typeof.ts:3:26 type as m via 1",
                "tests/actual.test.ts:2:56 type via 2 (test)",
            ],
            "{name}"
        );
    }
    // a namespace that reads only another export never names it
    let found = uses_of(&graph, "formatPrice");
    let unused: Vec<String> = found
        .unused
        .iter()
        .map(|e| format!("{}:{}", e.file, e.line.unwrap_or(0)))
        .collect();
    assert_eq!(unused, ["src/rates.ts:1", "src/unused.ts:1"]);
}

#[test]
fn a_module_that_is_one_declaration_gives_it_to_require() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-module-value");
    let report = scan(&root, &ScanOptions::default()).unwrap();
    // `module.exports = logger`: `require` binds the symbol itself, so a
    // member read through the binding is a use, and a destructuring takes
    // the symbol
    let logger = uses_of(&report, "logger");
    assert_eq!(
        shown(&logger),
        [
            "src/app.js:2:1 read via 1",
            "src/held.js:1:18 read via 1",
            "src/logger.js:6:18 read",
        ],
        "{:#?}",
        shown(&logger)
    );
    // a binding nothing reads still never names it
    assert_eq!(places(&logger.unused), ["src/idle.js:1"]);
    // `module.exports = slugify`: called through the binding and directly
    let slugify = uses_of(&report, "slugify");
    assert_eq!(
        shown(&slugify),
        [
            "src/slug.js:4:18 read",
            "src/use.js:2:23 call via 1",
            "src/use.js:2:40 call via 2",
            "src/use2.js:2:23 read as slug via 1",
        ],
        "{:#?}",
        shown(&slugify)
    );
    assert!(slugify.unused.is_empty() && slugify.escapes.is_empty());
    // `export = Engine`: `import x = require()` binds the class
    let boot = uses_of(&report, "Engine.boot");
    assert_eq!(shown(&boot), ["src/car.ts:2:25 call via 1"]);
    assert!(boot.unused.is_empty(), "{:?}", places(&boot.unused));
}

#[test]
fn a_static_member_may_be_called_where_its_class_is_used_as_a_value() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-static-members");
    let report = scan(&root, &ScanOptions::default()).unwrap();
    let open = uses_of(&report, "Wallet.open");
    assert!(open.uses.is_empty(), "{:#?}", shown(&open));
    // the class passed, kept in a variable, or reached through its module
    // and passed: code there may call it, so no statement of them is a
    // negative fact
    let class: Vec<String> = open
        .escapes
        .iter()
        .map(|e| format!("{}:{} {:?}", e.file, e.line.unwrap_or(0), e.note))
        .collect();
    assert_eq!(
        class,
        [
            "src/alias.ts:3 Some(\"class\")",
            "src/boot.ts:4 Some(\"class\")",
            "src/registry.ts:7 Some(\"class\")",
        ]
    );
    // constructed, compared by `instanceof` and named in a type: never
    // the static member
    assert_eq!(places(&open.unused), ["src/plain.ts:1"]);
    // a member that is not static goes by `values` instead
    let pay = uses_of(&report, "Wallet.pay");
    assert!(pay.escapes.is_empty(), "{:?}", places(&pay.escapes));
    assert!(pay.unused.is_empty(), "{:?}", places(&pay.unused));
}
