//! Where a Python symbol is used, read on demand from `fixtures/python-uses`
//! with Python's scope rules.

use std::path::{Path, PathBuf};

use archmap_core::{Evidence, SymbolUses, UnreadReason};
use archmap_scan::{scan, symbol_uses, ScanOptions, ScanReport};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn report() -> ScanReport {
    scan(&fixture("python-uses"), &ScanOptions::default()).unwrap()
}

fn uses_of(report: &ScanReport, name: &str) -> SymbolUses {
    let symbol = report
        .graph
        .symbols
        .values()
        .find(|s| s.name == name)
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

fn places(list: &[Evidence]) -> Vec<String> {
    list.iter()
        .map(|e| format!("{}:{}", e.file, e.line.unwrap_or(0)))
        .collect()
}

fn unread(found: &SymbolUses) -> Vec<(String, Option<u32>, UnreadReason)> {
    found
        .unread
        .iter()
        .map(|u| (u.file.clone(), u.line, u.reason))
        .collect()
}

#[test]
fn a_name_is_used_where_python_resolves_it_to_the_import() {
    let report = report();
    let pay = uses_of(&report, "pay");
    assert_eq!(
        shown(&pay),
        [
            "bazaar/aliased.py:5:12 call as settle via 1",
            // the defining file's own calls go through no statement
            "bazaar/billing/charge.py:9:12 call",
            "bazaar/billing/charge.py:18:16 call",
            // a local import binds in its function only, so `lazy` uses
            // nothing
            "bazaar/local.py:4:13 call via 2",
            "bazaar/modules.py:6:27 call as bazaar.billing.charge.pay via 1",
            "bazaar/modules.py:7:19 call as charge.pay via 2",
            // through the package's `__init__.py`, which passes it on
            "bazaar/passing.py:5:12 call via 1",
            // a method sees the module's name, a default runs outside the
            // function; a parameter, a local assigned later, a
            // comprehension target and a class attribute shadow it
            "bazaar/shadowed.py:23:16 call via 1",
            "bazaar/shadowed.py:26:24 read via 1",
            "bazaar/shadowed.py:30:1 call via 1",
            "bazaar/starred.py:5:12 call via 1",
            "bazaar/tests/test_pay.py:7:12 call via 3 (test)",
        ]
    );
    // `model.eval()` and a method named `eval` read no name by a computed
    // one: only the builtins do, as `globals()` does in `dynamic.py`
    assert_eq!(
        places(&pay.unused),
        ["bazaar/evaluated.py:1", "bazaar/unused.py:1"]
    );
    assert_eq!(places(&pay.passed_on), ["bazaar/billing/__init__.py:1"]);
    // the module passed as a value, and read by a dunder
    assert_eq!(
        places(&pay.escapes),
        ["bazaar/dunder.py:5", "bazaar/escaped.py:5"]
    );
    assert_eq!(
        unread(&pay),
        [
            ("bazaar/broken.py".into(), Some(1), UnreadReason::ParseError),
            (
                "bazaar/dynamic.py".into(),
                Some(1),
                UnreadReason::DynamicAccess
            ),
            // `global pay` in a function, and an `except ImportError` that
            // binds the name again
            ("bazaar/rebinds.py".into(), Some(1), UnreadReason::Rebound),
            ("bazaar/retry.py".into(), Some(2), UnreadReason::Rebound),
        ]
    );
}

#[test]
fn a_name_a_package_passes_on_under_another_is_used_by_that_name() {
    let report = report();
    let refund = uses_of(&report, "refund");
    assert_eq!(
        shown(&refund),
        [
            // `from .charge import refund as settle` in the package
            "bazaar/passing.py:5:20 call as settle via 1",
            "bazaar/tests/test_pay.py:14:12 read via 12 (test)",
        ]
    );
    assert_eq!(places(&refund.passed_on), ["bazaar/billing/__init__.py:2"]);
    // a star of the module binds it, and nothing names it there
    assert_eq!(places(&refund.unused), ["bazaar/starred.py:1"]);
}

#[test]
fn a_dotted_string_that_names_the_symbol_is_an_escape_noted_string() {
    let report = report();
    let refund = uses_of(&report, "refund");
    let strings: Vec<&Evidence> = refund
        .escapes
        .iter()
        .filter(|e| e.note.as_deref() == Some("string"))
        .collect();
    assert_eq!(strings.len(), 1, "{:#?}", refund.escapes);
    assert_eq!(
        (strings[0].file.as_str(), strings[0].line, strings[0].test),
        ("bazaar/tests/test_pay.py", Some(10), true)
    );
}

#[test]
fn a_class_is_made_by_a_call_and_named_in_annotations_and_string_annotations() {
    let report = report();
    let wallet = uses_of(&report, "Wallet");
    assert_eq!(
        shown(&wallet),
        [
            "bazaar/methods.py:5:12 read via 1",
            // a base class
            "bazaar/methods.py:8:12 read via 1",
            // `"list[Wallet]"` and `"Wallet"`; not `Literal["Wallet"]`
            "bazaar/typed.py:7:29 type via 4",
            "bazaar/typed.py:7:43 type via 4",
            "bazaar/values.py:5:12 new via 1",
        ]
    );
    assert!(wallet
        .uses
        .iter()
        .all(|u| u.evidence.type_only == (u.role.as_str() == "type")));
}

#[test]
fn a_method_is_used_through_its_class_self_and_cls_and_values_stay_open() {
    let report = report();
    let open = uses_of(&report, "Wallet.open");
    assert_eq!(
        shown(&open),
        [
            "bazaar/billing/charge.py:17:14 call as self.open",
            "bazaar/billing/charge.py:22:20 call as cls.open",
            // not `value.open()` in a static method
            "bazaar/methods.py:5:19 call via 1",
        ]
    );
    // statements that bind the class and use the method no way the pass
    // reads: values of the class may reach it there
    assert_eq!(
        places(&open.values),
        [
            "bazaar/starred.py:1",
            "bazaar/typed.py:4",
            "bazaar/values.py:1"
        ]
    );
    assert_eq!(places(&open.subclasses), ["bazaar/methods.py:8"]);
    assert!(open.unused.is_empty());
}

#[test]
fn a_star_binds_what_the_module_exports() {
    let report = report();
    // `__all__` lists it
    assert_eq!(
        shown(&uses_of(&report, "fmt")),
        [
            // the file holds a syntax error further down, read past it
            "bazaar/broken.py:6:12 call via 2",
            "bazaar/listed.py:5:12 call via 1",
        ]
    );
    // `__all__` leaves it out: the star never names it
    let cents = uses_of(&report, "cents");
    assert!(cents.uses.is_empty());
    assert_eq!(places(&cents.unused), ["bazaar/listed.py:1"]);
    // an `__all__` that code builds may take it: no negative fact
    let duty = uses_of(&report, "duty");
    assert!(duty.unused.is_empty());
    assert_eq!(
        unread(&duty),
        [(
            "bazaar/built.py".into(),
            Some(1),
            UnreadReason::DynamicAccess
        )]
    );
}

#[test]
fn every_statement_read_ends_in_one_of_the_lists() {
    for name in [
        "python-uses",
        "python-bindings",
        "simple-python-project",
        "nested-manifests-project",
        "mixed-utils-project",
    ] {
        let report = scan(&fixture(name), &ScanOptions::default()).unwrap();
        let graph = &report.graph;
        for symbol in graph.symbols.values() {
            let python = graph
                .component(&symbol.component)
                .and_then(|c| c.language.as_deref())
                == Some("python");
            let Some(importers) = graph.symbol_importers(symbol).filter(|_| python) else {
                continue;
            };
            let found = symbol_uses(&report, symbol);
            let defining = symbol.location().map(|e| e.file.as_str());
            for (_, statement) in importers.by_name.iter().chain(&importers.may_use) {
                let (file, line) = (statement.file.as_str(), statement.line);
                if Some(file) == defining {
                    continue;
                }
                let at = |e: &Evidence| e.file == file && e.line == line;
                let ended = found.uses.iter().any(|u| {
                    u.statement
                        .as_ref()
                        .is_some_and(|s| s.file == file && Some(s.line) == line)
                }) || found.unused.iter().any(at)
                    || found.passed_on.iter().any(at)
                    || found.values.iter().any(at)
                    || found.escapes.iter().any(|e| e.file == file)
                    || found
                        .unread
                        .iter()
                        .any(|u| u.file == file && (u.line.is_none() || u.line == line));
                assert!(
                    ended,
                    "{name}: {} at {file}:{line:?} ends in no list: {found:#?}",
                    symbol.id
                );
            }
        }
    }
}
