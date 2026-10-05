//! Every mark an answer shows is explained in its `Marks` line: `query` on
//! every component and symbol of every fixture, and `impact` on every
//! component, each with every entry listed.

use std::collections::BTreeSet;
use std::path::Path;

use archmap_app::{Format, ImpactRequest, QueryRequest, ScanMode, Workspace, DEFAULT_DEPTH};

/// One-word parentheses that are no marks: kinds that name themselves (a
/// target's, a piece of test code's that runs as no test).
const WORDS: [&str; 4] = ["bench", "example", "file", "helper"];

/// The one-word parentheses after a location, which is how marks are
/// written: after a space that follows `file:line`, a path or another mark.
/// A path's own (`app/(test)/page.tsx`) follows no space, and a
/// signature's (`pad = (text) =>`) no location.
fn one_word(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (at, _) in text.match_indices(" (") {
        let before = text[..at].rsplit(char::is_whitespace).next().unwrap_or("");
        let located = before.ends_with(')')
            || before.ends_with(|c: char| c.is_ascii_digit())
            || before.contains('/')
            || before.contains('.');
        let rest = &text[at + 2..];
        let Some(end) = rest.find(')') else { continue };
        let word = &rest[..end];
        if located && !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase()) {
            found.insert(word.to_owned());
        }
    }
    found
}

/// The marks an answer's `Marks` line explains.
fn explained(text: &str) -> BTreeSet<String> {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("Marks: "))
        .unwrap_or("");
    line.split("; ")
        .filter_map(|entry| entry.strip_prefix('('))
        .filter_map(|entry| entry.split_once(')'))
        .map(|(word, _)| word.to_owned())
        .collect()
}

/// What `text` shows as marks and its `Marks` line does not explain.
fn unexplained(text: &str) -> BTreeSet<String> {
    let body: String = text
        .lines()
        .filter(|l| !l.starts_with("Marks: "))
        .collect::<Vec<_>>()
        .join("\n");
    let explained = explained(text);
    one_word(&body)
        .into_iter()
        .filter(|w| !explained.contains(w) && !WORDS.contains(&w.as_str()))
        .collect()
}

#[test]
fn the_helpers_tell_marks_and_their_line_apart() {
    let text = "a.ts:1 (local) (type)\nb (helper, for 1 test listed)\napp/(test)/x.tsx\n\
                exports.pad = (text) =>  f.cjs:1\nMarks: (type) types only, never runs\n";
    assert_eq!(
        one_word(text),
        BTreeSet::from(["local".to_owned(), "type".to_owned()])
    );
    assert_eq!(unexplained(text), BTreeSet::from(["local".to_owned()]));
}

#[test]
fn every_mark_a_fixture_shows_is_explained() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    // their history is archmap's own: keep it out
    std::env::set_var("GIT_CEILING_DIRECTORIES", &fixtures);
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut fixture_dirs: Vec<_> = std::fs::read_dir(&fixtures)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    fixture_dirs.sort();
    for root in fixture_dirs {
        let ws = Workspace::scan(&root, ScanMode::Full).unwrap();
        let graph = ws.graph();
        let components = graph
            .components
            .keys()
            .map(|id| id.as_str())
            .filter(|id| !id.starts_with("ext:"));
        let symbols = graph.symbols.keys().map(|id| id.as_str());
        let name = root.file_name().unwrap().to_string_lossy();
        for target in components.clone().chain(symbols) {
            let query = QueryRequest {
                target,
                depth: DEFAULT_DEPTH,
                format: Format::Text,
                verbose: true,
            };
            let text = ws.query(&query).unwrap().output;
            for word in unexplained(&text) {
                missing.insert(format!("{name}: query {target}: ({word})"));
            }
        }
        for target in components {
            let impact = ImpactRequest {
                target,
                depth: DEFAULT_DEPTH,
                format: Format::Text,
                verbose: true,
            };
            let text = ws.impact(&impact).unwrap().output;
            for word in unexplained(&text) {
                missing.insert(format!("{name}: impact {target}: ({word})"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "marks with no entry in query_text::MARKS:\n{}",
        missing.into_iter().collect::<Vec<_>>().join("\n")
    );
}
