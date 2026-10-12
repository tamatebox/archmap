//! `fetch github` run as processes against stand-ins on PATH: gh, which
//! holds a login for the hosts `FAKE_GH_TOKEN_HOSTS` lists (github.com
//! unless told otherwise), answers a repository with no items and records
//! each call, and ssh, whose configuration takes the alias `github-work` to
//! github.com and `ghe-work` to ghe.example.com: no network.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const GH: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GH_LOG"
if [ "$1" = "auth" ]; then
  host=github.com
  [ "$3" = "--hostname" ] && host=$4
  case " ${FAKE_GH_TOKEN_HOSTS-github.com} " in
    *" $host "*) [ "$2" = "token" ] && echo token; exit 0 ;;
  esac
  echo "no oauth token found for $host" >&2; exit 1
fi
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

const SSH: &str = r#"#!/bin/sh
[ "$1" = "-G" ] || exit 255
case $2 in
  github-work) echo "hostname github.com" ;;
  ghe-work) echo "hostname ghe.example.com" ;;
  *) echo "hostname $2" ;;
esac
"#;

/// A root with no repository, the stand-ins, gh's call log, and a gate that
/// holds each request to GitHub while it exists.
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
        for (name, script) in [("gh", GH), ("ssh", SSH)] {
            let tool = dir.join("bin").join(name);
            std::fs::write(&tool, script).unwrap();
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join("log"), "").unwrap();
        Fake { dir }
    }

    fn fetch(&self, args: &[&str]) -> Command {
        let mut command = self.fetch_from_origin(&["--repo", "acme/shop"]);
        command.args(args);
        command
    }

    /// A fetch that names no repository: the root's `origin` does.
    fn fetch_from_origin(&self, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_archmap"));
        command
            .args(["fetch", "github", "--path"])
            .arg(self.dir.join("root"))
            .args(args)
            .env("PATH", path)
            .env("FAKE_GH_LOG", self.dir.join("log"))
            .env("FAKE_GH_GATE", self.dir.join("gate"))
            .env_remove("FAKE_GH_FAIL")
            .env_remove("FAKE_GH_TOKEN_HOSTS")
            // no repository above the root is read
            .env("GIT_CEILING_DIRECTORIES", &self.dir);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.fetch(args).output().unwrap()
    }

    /// The requests asked of GitHub.
    fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.join("log"))
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("api "))
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

#[test]
fn a_refresh_of_an_ssh_alias_updates_the_snapshot_fetched_with_repo() {
    let fake = Fake::new("alias");
    let root = fake.dir.join("root");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["remote", "add", "origin", "git@github-work:acme/shop.git"]);
    let refresh = |args: &[&str]| fake.fetch_from_origin(args).output().unwrap();
    // before any fetch with --repo there is nothing to refresh
    let none = refresh(&["--if-older-than", "30m"]);
    assert_eq!(none.status.code(), Some(2));
    assert!(
        stderr(&none).contains(
            "is an SSH alias of github.com, else with --repo HOST/acme/shop; an automatic \
             refresh would update the repository of a snapshot fetched so, but there is none at \
             .archmap/github.json"
        ),
        "{}",
        stderr(&none)
    );
    assert!(!root.join(".archmap").exists());
    assert_eq!(fake.calls(), 0);
    // a fetch by hand names it once
    assert_eq!(fake.run(&[]).status.code(), Some(0));
    let asked = fake.calls();
    // then the refresh takes it from the snapshot, fresh or not
    let fresh = refresh(&["--if-older-than", "30m"]);
    assert_eq!(fresh.status.code(), Some(0), "{}", stderr(&fresh));
    assert!(stdout(&fresh).starts_with("fresh: .archmap/github.json was fetched "));
    assert_eq!(fake.calls(), asked);
    let updated = refresh(&["--if-older-than", "0s"]);
    assert_eq!(updated.status.code(), Some(0), "{}", stderr(&updated));
    assert!(
        stdout(&updated).starts_with("updated .archmap/github.json with 0 items changed since "),
        "{}",
        stdout(&updated)
    );
    assert!(fake.calls() > asked);
    // a fetch asked for still takes origin as it is
    let plain = fake.fetch_from_origin(&[]).output().unwrap();
    assert_eq!(plain.status.code(), Some(2));
    // and an origin of another name refreshes nothing
    git(&[
        "remote",
        "set-url",
        "origin",
        "git@github-work:acme/web.git",
    ]);
    let other = refresh(&["--if-older-than", "0s"]);
    assert_eq!(other.status.code(), Some(2));
    assert!(
        stderr(&other).ends_with(
            "but the one at .archmap/github.json is of github.com/acme/shop, not acme/web\n"
        ),
        "{}",
        stderr(&other)
    );
}

#[test]
fn an_alias_or_host_origin_that_ssh_does_not_tie_to_the_snapshot_refreshes_nothing() {
    let fake = Fake::new("unconfirmed");
    let root = fake.dir.join("root");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    assert_eq!(fake.run(&[]).status.code(), Some(0));
    let before = std::fs::read(root.join(".archmap/github.json")).unwrap();
    let asked = fake.calls();
    for (origin, why) in [
        // an alias SSH names no host for
        (
            "git@github-other:acme/shop.git",
            "but the one at .archmap/github.json is of github.com, and ssh takes github-other \
             to github-other\n",
        ),
        // an enterprise host never takes github.com's repository of its name
        (
            "https://ghe.example.com/acme/shop.git",
            "but the one at .archmap/github.json is of github.com, and `origin` names \
             ghe.example.com\n",
        ),
    ] {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["remote", "remove", "origin"])
            .output();
        git(&["remote", "add", "origin", origin]);
        let refused = fake
            .fetch_from_origin(&["--if-older-than", "0s"])
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(2), "{origin}");
        assert!(stderr(&refused).ends_with(why), "{}", stderr(&refused));
    }
    assert_eq!(fake.calls(), asked);
    assert_eq!(
        std::fs::read(root.join(".archmap/github.json")).unwrap(),
        before
    );
}

#[test]
fn an_automatic_refresh_with_no_login_for_its_host_writes_nothing() {
    let fake = Fake::new("no-login");
    let refused = fake
        .fetch(&["--if-older-than", "30m"])
        .env("FAKE_GH_TOKEN_HOSTS", "")
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        stderr(&refused),
        "gh holds no login for github.com: nothing fetched (`gh auth login --hostname \
         github.com`)\n"
    );
    assert_eq!(fake.calls(), 0);
    assert!(!fake.dir.join("root/.archmap").exists());
    // a fetch asked for checks no login first, as before
    let asked = fake
        .fetch(&[])
        .env("FAKE_GH_TOKEN_HOSTS", "")
        .output()
        .unwrap();
    assert_eq!(asked.status.code(), Some(0), "{}", stderr(&asked));
}

#[test]
fn an_automatic_refresh_without_gh_says_so_and_writes_nothing() {
    let fake = Fake::new("no-gh");
    let empty = fake.dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    // nothing on PATH: --repo needs no git, and gh is not there
    let refused = fake
        .fetch(&["--if-older-than", "30m"])
        .env("PATH", &empty)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        stderr(&refused),
        "the gh CLI is not installed: see https://cli.github.com: nothing fetched\n"
    );
    assert!(!fake.dir.join("root/.archmap").exists());
}

/// The log of gh's calls, whatever they asked.
fn gh_log(fake: &Fake) -> String {
    std::fs::read_to_string(fake.dir.join("log")).unwrap()
}

#[test]
fn an_automatic_refresh_asks_for_a_login_to_the_host_it_fetches_from() {
    let fake = Fake::new("hosts");
    let root = fake.dir.join("root");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    // a snapshot of an enterprise host, fetched once by hand with --repo,
    // through a login gh holds for that host only
    let enterprise = fake
        .fetch_from_origin(&["--repo", "ghe.example.com/acme/shop"])
        .env("FAKE_GH_TOKEN_HOSTS", "ghe.example.com")
        .output()
        .unwrap();
    assert_eq!(enterprise.status.code(), Some(0), "{}", stderr(&enterprise));
    for origin in [
        // the host itself, and an SSH alias of it
        "https://ghe.example.com/acme/shop.git",
        "git@ghe-work:acme/shop.git",
    ] {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["remote", "remove", "origin"])
            .output();
        git(&["remote", "add", "origin", origin]);
        std::fs::write(fake.dir.join("log"), "").unwrap();
        let fresh = fake
            .fetch_from_origin(&["--if-older-than", "30m"])
            .env("FAKE_GH_TOKEN_HOSTS", "ghe.example.com")
            .output()
            .unwrap();
        assert_eq!(fresh.status.code(), Some(0), "{origin}: {}", stderr(&fresh));
        assert!(
            stdout(&fresh).starts_with("fresh: "),
            "{origin}: {}",
            stdout(&fresh)
        );
        assert_eq!(
            gh_log(&fake),
            "auth token --hostname ghe.example.com\n",
            "{origin}"
        );
        // a login to github.com alone is no login to the host
        let refused = fake
            .fetch_from_origin(&["--if-older-than", "30m"])
            .env("FAKE_GH_TOKEN_HOSTS", "github.com")
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(1), "{origin}");
        assert!(
            stderr(&refused).starts_with("gh holds no login for ghe.example.com: nothing fetched"),
            "{origin}: {}",
            stderr(&refused)
        );
    }
}
