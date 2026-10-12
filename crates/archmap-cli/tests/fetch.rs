//! `fetch github` run as processes against a stand-in for gh on PATH, which
//! answers a repository with no items and records each call: no network.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const GH: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GH_LOG"
input=$(cat)
while [ -e "$FAKE_GH_GATE" ]; do sleep 0.05; done
if [ -n "$FAKE_GH_FAIL" ]; then echo "$FAKE_GH_FAIL" >&2; exit 1; fi
empty='{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}'
case "$input" in
  *"query Schema"*) echo '{"data":{"issue":{"fields":[]},"pull":{"fields":[]},"viewer":{"login":"fetcher","databaseId":7}}}' ;;
  *"query Issues"*) echo "{\"data\":{\"repository\":{\"nameWithOwner\":\"acme/shop\",\"issues\":$empty}}}" ;;
  *"query PullRequests"*) echo "{\"data\":{\"repository\":{\"pullRequests\":$empty}}}" ;;
  *) echo "unexpected request" >&2; exit 1 ;;
esac
"#;

/// A root with no repository, a gh stand-in, its call log, and a gate that
/// holds each gh call while it exists.
struct Fake {
    dir: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Fake {
        let dir =
            std::env::temp_dir().join(format!("archmap-cli-fetch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root")).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let gh = dir.join("bin/gh");
        std::fs::write(&gh, GH).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("log"), "").unwrap();
        Fake { dir }
    }

    fn fetch(&self, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_archmap"));
        command
            .args(["fetch", "github", "--repo", "acme/shop", "--path"])
            .arg(self.dir.join("root"))
            .args(args)
            .env("PATH", path)
            .env("FAKE_GH_LOG", self.dir.join("log"))
            .env("FAKE_GH_GATE", self.dir.join("gate"))
            .env_remove("FAKE_GH_FAIL")
            // the root is no repository, and no repository above it is read
            .env("GIT_CEILING_DIRECTORIES", &self.dir);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.fetch(args).output().unwrap()
    }

    fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.join("log"))
            .unwrap()
            .lines()
            .count()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn refreshes_started_together_fetch_once_and_the_others_ask_nothing() {
    let fake = Fake::new("together");
    std::fs::write(fake.dir.join("gate"), "").unwrap();
    let first = fake
        .fetch(&["--if-older-than", "30m"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // the first is inside gh, holding the snapshot's lock
    let started = Instant::now();
    while fake.calls() == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "gh was never called"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // another automatic refresh has nothing to do
    let busy = fake.run(&["--if-older-than", "30m"]);
    assert_eq!(busy.status.code(), Some(0), "{}", stderr(&busy));
    assert_eq!(
        stdout(&busy),
        "busy: another archmap fetch is updating .archmap/github.json: nothing fetched\n"
    );
    // a fetch asked for, whole or as an update, did not happen
    for args in [&[][..], &["--update"][..]] {
        let refused = fake.run(args);
        assert_eq!(refused.status.code(), Some(1), "{args:?}");
        assert_eq!(
            stderr(&refused),
            "another archmap fetch is updating .archmap/github.json: nothing fetched\n"
        );
        assert_eq!(stdout(&refused), "");
    }
    assert_eq!(fake.calls(), 1, "none of them asked gh");
    std::fs::remove_file(fake.dir.join("gate")).unwrap();
    let first = first.wait_with_output().unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert!(
        stdout(&first)
            .starts_with("wrote .archmap/github.json (fetched whole: there is no snapshot): "),
        "{}",
        stdout(&first)
    );
    let asked = fake.calls();
    // and then it is fresh
    let fresh = fake.run(&["--if-older-than", "30m"]);
    assert_eq!(fresh.status.code(), Some(0));
    assert!(stdout(&fresh).starts_with("fresh: .archmap/github.json was fetched "));
    assert!(stdout(&fresh).ends_with(", within 30m: nothing fetched\n"));
    assert_eq!(fake.calls(), asked);
    assert!(fake.dir.join("root/.archmap/github.json.lock").is_file());
}

#[test]
fn a_failed_fetch_exits_1_and_the_next_refresh_waits_without_asking() {
    let fake = Fake::new("failed");
    let failed = fake
        .fetch(&["--if-older-than", "30m"])
        .env(
            "FAKE_GH_FAIL",
            "HTTP 502: Bad Gateway (https://api.github.com/graphql)",
        )
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    let message = stderr(&failed);
    assert!(
        message.starts_with("gh failed: HTTP 502: Bad Gateway"),
        "{message}"
    );
    assert!(
        message.contains(
            "\n  .archmap/github.json is as it was; the failure (network) is recorded in \
             .archmap/github.status.json: no automatic attempt before "
        ),
        "{message}"
    );
    assert!(!fake.dir.join("root/.archmap/github.json").exists());
    let asked = fake.calls();
    let waiting = fake.run(&["--if-older-than", "30m"]);
    assert_eq!(waiting.status.code(), Some(0));
    assert!(
        stdout(&waiting).starts_with("waiting: the last attempt, "),
        "{}",
        stdout(&waiting)
    );
    assert_eq!(fake.calls(), asked);
    // a fetch asked for tries now
    let now = fake.run(&[]);
    assert_eq!(now.status.code(), Some(0), "{}", stderr(&now));
    assert!(fake.calls() > asked);
    // a wrong duration is an invocation error
    let wrong = fake.run(&["--if-older-than", "soon"]);
    assert_eq!(wrong.status.code(), Some(2));
}
