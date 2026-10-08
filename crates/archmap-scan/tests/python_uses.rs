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

/// A project written to a directory of its own, scanned; the directory
/// goes when it drops, since the pass reads the files when asked.
struct Project {
    root: PathBuf,
    report: ScanReport,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Project {
        let root =
            std::env::temp_dir().join(format!("archmap-py-uses-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (file, text) in [("pyproject.toml", "[project]\nname = \"till\"\n")]
            .iter()
            .chain(files)
        {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let report = scan(&root, &ScanOptions::default()).unwrap();
        Project { root, report }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
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
fn a_call_that_loads_a_module_by_a_literal_name_binds_it_as_an_import_does() {
    let report = scan(&fixture("python-bindings"), &ScanOptions::default()).unwrap();
    let rate = uses_of(&report, "rate");
    let loader = |list: Vec<String>| -> Vec<String> {
        list.into_iter()
            .filter(|s| s.starts_with("store/loader.py"))
            .collect()
    };
    assert_eq!(
        loader(shown(&rate)),
        [
            // through the name a statement binds it to
            "store/loader.py:22:19 call as module.rate via 21",
            // an attribute of the call itself
            r#"store/loader.py:26:59 call as importlib.import_module("store.billing.rates").rate via 26"#,
            // `__import__` returns the package the name starts with
            "store/loader.py:31:34 call as package.billing.rates.rate via 30",
        ]
    );
    // returned, the module goes on as a value; dropped, it names nothing
    assert_eq!(
        loader(places(&rate.escapes)),
        ["store/loader.py:5", "store/loader.py:9"]
    );
    assert_eq!(loader(places(&rate.unused)), ["store/loader.py:35"]);
    assert!(
        !rate.unread.iter().any(|u| u.file == "store/loader.py"),
        "{:#?}",
        rate.unread
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
fn a_module_level_import_that_another_file_takes_the_name_from_passes_it_on() {
    let project = Project::new(
        "taken",
        &[
            ("till/__init__.py", ""),
            ("till/impl.py", "def pay(amount):\n    return amount\n"),
            // taken by name, directly and through the module
            ("till/api.py", "from .impl import pay\n"),
            ("till/app.py", "from till.api import pay\n\npay(1)\n"),
            ("till/mod.py", "from .impl import pay\n"),
            ("till/dotted.py", "import till.mod\n\ntill.mod.pay(2)\n"),
            // taken by a star, without `__all__` and with one code builds
            ("till/open.py", "from .impl import pay\n"),
            ("till/starred.py", "from till.open import *\n"),
            (
                "till/built.py",
                "from .impl import pay\n\n__all__ = [\"pay\"] + []\n",
            ),
            ("till/built_star.py", "from till.built import *\n"),
            // another name taken from the module: nothing takes `pay`
            ("till/lone.py", "from .impl import pay\n\nRATE = 2\n"),
            ("till/rate.py", "from till.lone import RATE\n"),
        ],
    );
    let pay = uses_of(&project.report, "pay");
    assert_eq!(
        shown(&pay),
        [
            "till/app.py:3:1 call via 1",
            "till/dotted.py:3:10 call as till.mod.pay via 1"
        ]
    );
    assert_eq!(
        places(&pay.passed_on),
        [
            "till/api.py:1",
            "till/built.py:1",
            "till/mod.py:1",
            "till/open.py:1"
        ]
    );
    assert_eq!(places(&pay.unused), ["till/lone.py:1"]);
}

#[test]
fn an_import_under_global_binds_in_the_module_and_one_under_nonlocal_binds_again() {
    let project = Project::new(
        "declared",
        &[
            ("till/__init__.py", ""),
            ("till/wallet.py", "def pay(amount):\n    return amount\n"),
            (
                "till/lazy.py",
                "def setup():\n    global pay\n    from till.wallet import pay\n    pay(2)\n\n\n\
                 def run():\n    return pay(1)\n",
            ),
            (
                "till/loaded.py",
                "from importlib import import_module\n\n\n\
                 def load():\n    global wallet\n    wallet = import_module(\"till.wallet\")\n\n\n\
                 def run():\n    return wallet.pay(3)\n",
            ),
            // the module binds it as well: no telling which code reads
            (
                "till/preset.py",
                "pay = None\n\n\n\
                 def setup():\n    global pay\n    from till.wallet import pay\n\n\n\
                 def run():\n    return pay(1)\n",
            ),
            // Python requires a function around to bind it too
            (
                "till/inner.py",
                "def outer():\n    pay = None\n\n    def inner():\n        nonlocal pay\n        \
                 from till.wallet import pay\n\n    inner()\n    return pay(1)\n",
            ),
        ],
    );
    let pay = uses_of(&project.report, "pay");
    assert_eq!(
        shown(&pay),
        [
            "till/lazy.py:4:5 call via 3",
            "till/lazy.py:8:12 call via 3",
            "till/loaded.py:10:19 call as wallet.pay via 6",
        ]
    );
    assert!(pay.unused.is_empty(), "{:#?}", pay.unused);
    assert_eq!(
        unread(&pay),
        [
            ("till/inner.py".into(), Some(6), UnreadReason::Rebound),
            ("till/preset.py".into(), Some(6), UnreadReason::Rebound),
        ]
    );
}

#[test]
fn the_defining_file_uses_it_past_overloads_and_leaves_another_binding_unread() {
    let project = Project::new(
        "overloads",
        &[
            ("till/__init__.py", ""),
            (
                "till/wallet.py",
                "from typing import overload\n\n\n\
                 @overload\ndef pay(amount: int) -> int: ...\n\
                 @overload\ndef pay(amount: str) -> str: ...\n\
                 def pay(amount):\n    return amount\n\n\n\
                 def refund(amount):\n    return pay(-amount)\n",
            ),
            ("till/app.py", "from till.wallet import pay\n\npay(1)\n"),
            // the name holds the wrapper once the module has run
            (
                "till/charge.py",
                "def traced(f):\n    return f\n\n\n\
                 def settle(amount):\n    return amount\n\n\n\
                 settle = traced(settle)\n\n\n\
                 def again():\n    return settle(1)\n",
            ),
        ],
    );
    let pay = uses_of(&project.report, "pay");
    assert_eq!(
        shown(&pay),
        ["till/app.py:3:1 call via 1", "till/wallet.py:13:12 call"]
    );
    assert!(pay.unread.is_empty(), "{:#?}", pay.unread);
    let settle = uses_of(&project.report, "settle");
    assert!(settle.uses.is_empty(), "{:#?}", settle.uses);
    assert_eq!(
        unread(&settle),
        [("till/charge.py".into(), Some(5), UnreadReason::Rebound)]
    );
}

#[test]
fn a_package_that_wraps_a_name_it_imports_binds_it_again_rather_than_passing_it_on() {
    let project = Project::new(
        "wrapped",
        &[
            (
                "till/__init__.py",
                "from .charge import pay\nfrom .log import traced\n\npay = traced(pay)\n",
            ),
            ("till/charge.py", "def pay(amount):\n    return amount\n"),
            ("till/log.py", "def traced(f):\n    return f\n"),
            ("till/app.py", "from till import pay\n\npay(1)\n"),
        ],
    );
    let pay = uses_of(&project.report, "pay");
    assert!(pay.passed_on.is_empty(), "{:#?}", pay.passed_on);
    assert_eq!(
        unread(&pay),
        [("till/__init__.py".into(), Some(1), UnreadReason::Rebound)]
    );
}

#[test]
fn a_module_passes_a_name_on_only_where_nothing_reaches_it_by_another_way() {
    let project = Project::new(
        "relays",
        &[
            ("till/__init__.py", ""),
            ("till/wallet.py", "def pay(amount):\n    return amount\n"),
            // its own code may call it by a computed name
            (
                "till/api.py",
                "from till.wallet import pay\n\n\n\
                 def checkout(name):\n    return globals()[name](1)\n",
            ),
            ("till/app.py", "from till.api import pay\n"),
            // a function binds it for the module, only when it runs
            (
                "till/lazy.py",
                "def load():\n    global pay\n    from till.wallet import pay\n\n\nload()\n",
            ),
            ("till/shop.py", "from till.lazy import pay\n\npay(1)\n"),
        ],
    );
    let pay = uses_of(&project.report, "pay");
    assert!(pay.passed_on.is_empty(), "{:#?}", pay.passed_on);
    assert_eq!(
        unread(&pay),
        [("till/api.py".into(), Some(1), UnreadReason::DynamicAccess)]
    );
    // app.py takes it and calls nothing
    assert_eq!(places(&pay.unused), ["till/app.py:1", "till/lazy.py:3"]);
}

#[test]
fn from_a_package_a_module_of_the_symbols_name_is_the_module_unless_the_package_binds_it() {
    let project = Project::new(
        "submodule",
        &[
            ("till/__init__.py", ""),
            ("till/pay.py", "def pay(amount):\n    return amount\n"),
            (
                "till/app.py",
                "from till import pay\n\n\ndef run():\n    return pay.pay(1)\n\n\n\
                 def keep():\n    return [pay]\n",
            ),
            ("till/relative.py", "from . import pay\n\npay.pay(2)\n"),
            // the module's name, not the module
            ("till/named.py", "from .pay import pay\n\npay(3)\n"),
            ("till/absolute.py", "from till.pay import pay\n\npay(4)\n"),
            // a package that binds the name of its module to the function
            ("till/fees/__init__.py", "from .levy import levy\n"),
            (
                "till/fees/levy.py",
                "def levy(amount):\n    return amount\n",
            ),
            ("till/levied.py", "from till.fees import levy\n\nlevy(5)\n"),
        ],
    );
    let pay = uses_of(&project.report, "pay");
    assert_eq!(
        shown(&pay),
        [
            "till/absolute.py:3:1 call via 1",
            "till/app.py:5:16 call as pay.pay via 1",
            "till/named.py:3:1 call via 1",
            "till/relative.py:3:5 call as pay.pay via 1",
        ]
    );
    // the module as a value
    assert_eq!(places(&pay.escapes), ["till/app.py:9"]);
    let levy = uses_of(&project.report, "levy");
    assert_eq!(shown(&levy), ["till/levied.py:3:1 call via 1"]);
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
