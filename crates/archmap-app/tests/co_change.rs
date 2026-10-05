//! `impact`'s files changed in the same commits as the target, from git
//! histories the tests build with fixed identities and dates, so their SHAs
//! are the same on every machine.

use std::path::{Path, PathBuf};
use std::process::Command;

use archmap_app::{Format, ImpactRequest, ScanMode, Workspace, DEFAULT_DEPTH};

/// A repository in a temp directory, its git config kept apart from the
/// user's (this isolation is for tests only: the product keeps it).
struct Repo {
    base: PathBuf,
    dir: PathBuf,
    commits: u32,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let base = std::env::temp_dir().join(format!(
            "archmap-app-co-change-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let repo = Repo {
            base,
            dir,
            commits: 0,
        };
        repo.git(&["init", "-q", "--object-format=sha1", "-b", "main"]);
        repo
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let date = format!("2026-01-{:02}T00:00:00Z", self.commits + 1);
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"])
            .args(["-c", "protocol.file.allow=always"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.base.join("home"))
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.dir, args)
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Commit everything, one day after the last commit; its short SHA.
    fn commit(&mut self) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "change"]);
        self.commits += 1;
        self.git(&["rev-parse", "--short=7", "HEAD"])
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// A notes file long enough that one edited line keeps a rename found.
fn notes(last: &str) -> String {
    let mut text: String = (0..9).map(|i| format!("Prices, line {i}.\n")).collect();
    text.push_str(last);
    text
}

/// A shop whose price module changes with its rates, its test, its notes
/// (renamed on the way) and once in a mass change that is left out.
fn shop(name: &str) -> (Repo, Vec<String>) {
    let mut repo = Repo::new(name);
    let mut commits = Vec::new();
    repo.write("package.json", "{ \"name\": \"shop\" }\n");
    repo.write(
        "src/pricing/price.ts",
        "export function price(n: number) {\n  return n;\n}\n",
    );
    repo.write("src/pricing/round.ts", "export const places = 2;\n");
    repo.write(
        "src/cart.ts",
        "import { price } from './pricing/price';\nexport const total = price(1);\n",
    );
    repo.write("config/rates.yaml", "rate: 1\n");
    repo.write(
        "tests/price.test.ts",
        "import { price } from '../src/pricing/price';\nprice(1);\n",
    );
    repo.write("docs/prices.md", &notes("End.\n"));
    commits.push(repo.commit());
    // the price, its rounding and its rates
    repo.write(
        "src/pricing/price.ts",
        "export function price(n: number) {\n  return n * 2;\n}\n",
    );
    repo.write("src/pricing/round.ts", "export const places = 3;\n");
    repo.write("config/rates.yaml", "rate: 2\n");
    commits.push(repo.commit());
    // the price and its test
    repo.write(
        "src/pricing/price.ts",
        "export function price(n: number) {\n  return n * 3;\n}\n",
    );
    repo.write(
        "tests/price.test.ts",
        "import { price } from '../src/pricing/price';\nprice(2);\n",
    );
    commits.push(repo.commit());
    // the notes renamed with an edit, and the price
    repo.git(&["mv", "docs/prices.md", "docs/pricing.md"]);
    repo.write("docs/pricing.md", &notes("The end.\n"));
    repo.write(
        "src/pricing/price.ts",
        "export function price(n: number) {\n  return n * 4;\n}\n",
    );
    commits.push(repo.commit());
    // a mass change: more than 30 files, left out
    for i in 0..31 {
        repo.write(&format!("data/d{i}.json"), "{}\n");
    }
    repo.write(
        "src/pricing/price.ts",
        "export function price(n: number) {\n  return n * 5;\n}\n",
    );
    commits.push(repo.commit());
    // the cart alone
    repo.write(
        "src/cart.ts",
        "import { price } from './pricing/price';\nexport const total = price(2);\n",
    );
    commits.push(repo.commit());
    (repo, commits)
}

fn impact(root: &Path, target: &str, format: Format) -> String {
    let ws = Workspace::scan(root, ScanMode::Full).unwrap();
    ws.impact(&ImpactRequest {
        target,
        depth: DEFAULT_DEPTH,
        format,
        verbose: false,
    })
    .unwrap()
    .output
}

/// How the heading of a list says to read its counts.
const PER_FILE: &str = "; per file: commits shared, of the target's and of its own";

/// The section's lines, from its heading to the history line.
fn section(text: &str) -> Vec<&str> {
    let mut lines = text
        .lines()
        .skip_while(|l| !l.starts_with("Changed in the same commits"));
    let mut found = Vec::new();
    for line in lines.by_ref() {
        found.push(line);
        if line.starts_with("  history:") || line.contains(": not read (") {
            break;
        }
    }
    found
}

#[test]
fn a_file_lists_the_files_changed_in_its_commits_with_counts_and_examples() {
    let (repo, c) = shop("file");
    let text = impact(&repo.dir, "src/pricing/price.ts", Format::Text);
    assert_eq!(
        section(&text),
        [
            format!(
                "Changed in the same commits (history, not imports): 6 files, showing 5, in the \
                 4 commits that changed src/pricing/price.ts{PER_FILE}"
            ),
            // as close to it: by path
            format!(
                "  config/rates.yaml  2 of the target's 4, 2 of its own 2: {} 2026-01-02, {} \
                 2026-01-01",
                c[1], c[0]
            ),
            format!(
                "  docs/pricing.md  2 of the target's 4, 2 of its own 2: {} 2026-01-04, {} \
                 2026-01-01 (then docs/prices.md)",
                c[3], c[0]
            ),
            format!(
                "  src/pricing/round.ts  2 of the target's 4, 2 of its own 2: {} 2026-01-02, {} \
                 2026-01-01",
                c[1], c[0]
            ),
            format!(
                "  tests/price.test.ts  2 of the target's 4, 2 of its own 2: {} 2026-01-03, {} \
                 2026-01-01",
                c[2], c[0]
            ),
            format!(
                "  package.json  1 of the target's 4, 1 of its own 1: {} 2026-01-01",
                c[0]
            ),
            format!(
                "  history: HEAD {}, full clone; 6 commits read, 5 counted; left out 1 over 30 \
                 files; renames -M50%; files by shared commits over the mean of both counts",
                c[5]
            ),
        ],
        "{text}"
    );
    // a full clone read whole hides nothing
    assert!(!text.contains("  history: files changed"), "{text}");
}

#[test]
fn json_lists_every_file_with_its_commits_and_the_history_read() {
    let (repo, c) = shop("json");
    let json = impact(&repo.dir, "src/pricing/price.ts", Format::Json);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let section = &value["co_change"];
    assert_eq!(
        section["target_paths"],
        serde_json::json!(["src/pricing/price.ts"])
    );
    assert_eq!(section["history"]["state"], "read");
    assert_eq!(section["history"]["renames"], "detected");
    assert_eq!(section["counts"]["large"], 1);
    assert_eq!(section["settings"]["max_files"], 30);
    let files = section["files"].as_array().unwrap();
    // every file, the one the text leaves out too
    assert_eq!(files.len(), 6);
    assert_eq!(files[5]["path"], "src/cart.ts");
    assert_eq!(files[1]["path"], "docs/pricing.md");
    let first = &files[1]["commits"][0]["id"];
    assert!(first.as_str().unwrap().starts_with(&c[3]), "{first}");
    assert_eq!(files[1]["earlier"].as_object().unwrap().len(), 1);
}

#[test]
fn a_component_counts_each_commit_once_for_its_files() {
    let (repo, _) = shop("component");
    let text = impact(&repo.dir, "src/pricing", Format::Text);
    let lines = section(&text);
    // the price and its rounding changed together in one commit: the rates
    // count it once
    assert!(
        lines[0].ends_with(&format!(
            "in the 4 commits that changed pricing's 2 files{PER_FILE}"
        )),
        "{text}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("  config/rates.yaml  2 of the target's 4, 2 of its own 2:")),
        "{text}"
    );
    assert!(!lines.iter().any(|l| l.contains("round.ts")), "{text}");
    // a component that holds every file its commits changed
    let whole = impact(&repo.dir, "shop", Format::Text);
    assert!(
        section(&whole)[0].ends_with("that changed shop's 38 files: they changed nothing else"),
        "{whole}"
    );
}

#[test]
fn a_symbol_reads_the_commits_of_its_file() {
    let (repo, _) = shop("symbol");
    let text = impact(&repo.dir, "price", Format::Text);
    assert!(
        section(&text)[0].ends_with(&format!(
            "in the 4 commits that changed its file src/pricing/price.ts{PER_FILE}"
        )),
        "{text}"
    );
}

#[test]
fn a_file_not_committed_says_so_and_uncommitted_edits_change_nothing() {
    let (repo, _) = shop("uncommitted");
    let before = impact(&repo.dir, "src/pricing/price.ts", Format::Text);
    repo.write("src/pricing/fresh.ts", "export const fresh = 1;\n");
    repo.write(
        "src/pricing/price.ts",
        "export function price(n: number) {\n  return n * 9;\n}\n",
    );
    let text = impact(&repo.dir, "src/pricing/fresh.ts", Format::Text);
    assert_eq!(
        section(&text)[0],
        "Changed in the same commits: none: HEAD does not hold src/pricing/fresh.ts, so no \
         commit changed it yet (uncommitted changes are not read)",
        "{text}"
    );
    let after = impact(&repo.dir, "src/pricing/price.ts", Format::Text);
    assert_eq!(section(&before), section(&after));
}

#[test]
fn a_shallow_clone_says_where_its_history_ends() {
    let (repo, _) = shop("shallow");
    let clone = repo.base.join("clone");
    let source = format!("file://{}", repo.dir.display());
    repo.git_in(
        &repo.base,
        &[
            "clone",
            "-q",
            "--depth",
            "2",
            &source,
            clone.to_str().unwrap(),
        ],
    );
    let text = impact(&clone, "src/pricing/price.ts", Format::Text);
    let lines = section(&text);
    // its last change is at the boundary, whose changes are not read
    assert_eq!(
        lines[0],
        "Changed in the same commits: none: no commit read changed src/pricing/price.ts, and \
         older commits were not read",
        "{text}"
    );
    assert!(
        lines[1].contains(
            ", shallow clone; 2 commits read, 1 counted; left out 1 at the shallow boundary;"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "  history: files changed in the same commits may be missing: the clone is \
             shallow, so the history ends at its depth\n"
        ),
        "{text}"
    );
}

#[test]
fn a_merge_counts_through_the_commits_it_merges() {
    let mut repo = Repo::new("merge");
    repo.write("package.json", "{ \"name\": \"shop\" }\n");
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.write("src/b.ts", "export const b = 1;\n");
    let first = repo.commit();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("src/a.ts", "export const a = 2;\n");
    repo.write("src/b.ts", "export const b = 2;\n");
    let branch = repo.commit();
    repo.git(&["checkout", "-q", "main"]);
    repo.write("src/c.ts", "export const c = 1;\n");
    repo.commit();
    repo.git(&["merge", "-q", "--no-ff", "-m", "merge", "feature"]);
    let text = impact(&repo.dir, "src/a.ts", Format::Text);
    let lines = section(&text);
    // what the merge changed itself is not read, and the heading says so
    assert_eq!(
        lines[0],
        format!(
            "Changed in the same commits (history, not imports): 2 files, in the 2 non-merge \
             commits that changed src/a.ts{PER_FILE}"
        ),
        "{text}"
    );
    assert_eq!(
        lines[1],
        format!(
            "  src/b.ts  2 of the target's 2, 2 of its own 2: {branch} 2026-01-02, {first} \
             2026-01-01"
        ),
        "{text}"
    );
    assert!(
        lines
            .last()
            .unwrap()
            .contains("; 4 commits read, 3 counted; left out 1 merge;"),
        "{text}"
    );
}

#[test]
fn a_submodule_changed_with_the_target_is_marked() {
    let mut repo = Repo::new("submodule");
    repo.write("package.json", "{ \"name\": \"shop\" }\n");
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.commit();
    // a submodule as HEAD's tree holds it: a commit of another repository,
    // with nothing checked out
    let commit = repo.git(&["rev-parse", "HEAD"]);
    repo.write("src/a.ts", "export const a = 2;\n");
    repo.git(&["add", "src/a.ts"]);
    let link = format!("160000,{commit},vendor/lib");
    repo.git(&["update-index", "--add", "--cacheinfo", &link]);
    repo.git(&["commit", "-q", "-m", "change"]);
    repo.commits += 1;
    let text = impact(&repo.dir, "src/a.ts", Format::Text);
    assert!(
        section(&text).iter().any(
            |l| l.starts_with("  vendor/lib (submodule)  1 of the target's 2, 1 of its own 1:")
        ),
        "{text}"
    );
    assert!(
        text.contains("\nMarks: (submodule) a git submodule, not a file\n"),
        "{text}"
    );
}

#[test]
fn a_root_below_the_top_reads_the_commits_under_it() {
    let (repo, _) = shop("below");
    let text = impact(&repo.dir.join("src"), "pricing/price.ts", Format::Text);
    let lines = section(&text);
    // paths are the root's, files outside it are no candidates, and the
    // mass change counts: it changed one file under the root
    assert!(
        lines[0].ends_with(&format!(
            "in the 5 commits that changed pricing/price.ts{PER_FILE}"
        )),
        "{text}"
    );
    assert!(
        lines[1].starts_with("  pricing/round.ts  2 of the target's 5, 2 of its own 2"),
        "{text}"
    );
    assert!(
        lines[2].starts_with("  cart.ts  1 of the target's 5, 1 of its own 2"),
        "{text}"
    );
    assert!(
        lines
            .last()
            .unwrap()
            .contains(", full clone, root src of the repository;"),
        "{text}"
    );
}
