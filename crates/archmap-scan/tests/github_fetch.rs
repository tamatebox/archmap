//! A GitHub fetch from recorded GraphQL answers with invented names: no
//! network, no gh. The transport is a closure that answers each request by
//! its operation and cursor.

use std::cell::RefCell;

use archmap_core::work::{ItemState, Ref, RelationType, SinceRule};
use archmap_scan::work::github::{fetch, update, FetchOptions, Update};
use serde_json::{json, Value};

fn options(since: Option<&str>) -> FetchOptions {
    FetchOptions {
        host: "github.com".into(),
        owner: "acme".into(),
        name: "shop".into(),
        since: since.map(str::to_owned),
        since_rule: match since {
            Some(_) => SinceRule::Given,
            None => SinceRule::All,
        },
        max_items: 5000,
        titles: true,
        now: "2026-10-03T09:00:00Z".into(),
        reread_since: "2026-10-03T08:55:00Z".into(),
    }
}

/// The schema of a host that offers every field the fetch reads, or not
/// sub-issues and dependencies.
fn schema(full: bool) -> Value {
    let field = |name: &str, args: &[&str]| json!({"name": name, "args": args.iter().map(|a| json!({"name": a})).collect::<Vec<_>>()});
    let mut issue = vec![
        field("duplicateOf", &[]),
        field(
            "closedByPullRequestsReferences",
            &["includeClosedPrs", "userLinkedOnly", "excludeUserLinked"],
        ),
    ];
    if full {
        issue.extend([
            field("subIssues", &[]),
            field("parent", &[]),
            field("blockedBy", &[]),
            field("blocking", &[]),
        ]);
    }
    json!({"data": {
        "issue": {"fields": issue},
        "pull": {"fields": [field("closingIssuesReferences", &["userLinkedOnly", "excludeUserLinked"])]},
        "viewer": {"login": "fetcher", "databaseId": 7},
    }})
}

fn page(nodes: Vec<Value>, next: Option<&str>) -> Value {
    json!({"pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}, "nodes": nodes})
}

fn reference(number: u64, repository: &str) -> Value {
    json!({"number": number, "repository": {"nameWithOwner": repository}})
}

fn empty() -> Value {
    page(vec![], None)
}

/// Issue #12 (closed by pull request #15, a sub-issue of #10 with more
/// sub-issues on a second page, cross-referenced from another repository
/// and from a source the account cannot see), and issue #9 on a second
/// page.
fn issues(cursor: Option<&str>) -> Value {
    match cursor {
        None => {
            json!({"data": {"repository": {"nameWithOwner": "acme/shop", "issues": page(vec![json!({
            "id": "I_12", "number": 12, "title": "Refunds round down", "state": "CLOSED",
            "stateReason": "COMPLETED", "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-04T00:00:00Z", "closedAt": "2026-01-04T00:00:00Z",
            "parent": reference(10, "acme/shop"),
            "duplicateOf": null,
            "subIssues": page(vec![reference(13, "acme/shop"), Value::Null], Some("sub-1")),
            "blockedBy": page(vec![reference(9, "acme/shop")], None),
            "blocking": empty(),
            "closes": page(vec![reference(15, "acme/shop")], None),
            "linked": empty(),
            "events": page(vec![
                json!({"__typename": "CrossReferencedEvent", "referencedAt": "2026-01-02T00:00:00Z",
                       "willCloseTarget": false, "source": {"__typename": "Issue", "number": 4,
                       "repository": {"nameWithOwner": "acme/web"}}}),
                json!({"__typename": "CrossReferencedEvent", "referencedAt": "2026-01-02T00:00:00Z",
                       "willCloseTarget": false, "source": null}),
                json!({"__typename": "ClosedEvent", "createdAt": "2026-01-04T00:00:00Z",
                       "closer": {"__typename": "PullRequest", "number": 15,
                                  "repository": {"nameWithOwner": "acme/shop"}}}),
                json!({"__typename": "ReferencedEvent", "createdAt": "2026-01-03T00:00:00Z",
                       "commit": {"oid": "a1"}, "commitRepository": {"nameWithOwner": "acme/shop"}}),
            ], None),
        })], Some("issues-1"))}}})
        }
        Some("issues-1") => {
            json!({"data": {"repository": {"nameWithOwner": "acme/shop", "issues": page(vec![json!({
            "id": "I_9", "number": 9, "title": "Rates", "state": "OPEN", "stateReason": null,
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "closedAt": null, "parent": null, "duplicateOf": reference(7, "acme/shop"),
            "subIssues": empty(), "blockedBy": empty(), "blocking": empty(),
            "closes": empty(), "linked": empty(), "events": empty(),
        })], None)}}})
        }
        other => panic!("issues page {other:?}"),
    }
}

/// Pull request #15, merged, with two commits listed of three; and #14,
/// updated before the range.
fn pulls() -> Value {
    json!({"data": {"repository": {"pullRequests": page(vec![
        json!({"id": "PR_15", "number": 15, "title": "Round refunds half up", "state": "MERGED",
               "createdAt": "2026-01-02T00:00:00Z", "updatedAt": "2026-01-04T00:00:00Z",
               "closedAt": "2026-01-04T00:00:00Z", "mergedAt": "2026-01-04T00:00:00Z",
               "mergeCommit": {"oid": "m1"},
               "commits": {"totalCount": 3, "pageInfo": {"hasNextPage": false, "endCursor": null},
                           "nodes": [{"commit": {"oid": "a1"}}, {"commit": {"oid": "a2"}}]},
               "closes": page(vec![reference(12, "acme/shop")], None),
               "linked": empty(), "events": empty()}),
        json!({"id": "PR_14", "number": 14, "title": "Old", "state": "CLOSED",
               "createdAt": "2025-01-01T00:00:00Z", "updatedAt": "2025-06-01T00:00:00Z",
               "closedAt": "2025-06-01T00:00:00Z", "mergedAt": null, "mergeCommit": null,
               "commits": {"totalCount": 0, "pageInfo": {"hasNextPage": false}, "nodes": []},
               "closes": empty(), "linked": empty(), "events": empty()}),
    ], Some("pulls-1"))}}})
}

/// The recorded answers, and the requests they were asked.
fn recorded(
    full: bool,
    requests: &RefCell<Vec<Value>>,
) -> impl FnMut(&Value) -> Result<Value, String> + '_ {
    move |request: &Value| {
        requests.borrow_mut().push(request.clone());
        let query = request["query"].as_str().unwrap();
        let cursor = request["variables"]["cursor"].as_str();
        if query.starts_with("query Schema") {
            return Ok(schema(full));
        }
        let again = request["variables"]["since"].as_str() == Some("2026-10-03T08:55:00Z");
        if query.starts_with("query Issues") && again {
            // nothing changed while the walks ran
            return Ok(
                json!({"data": {"repository": {"nameWithOwner": "acme/shop", "issues": empty()}}}),
            );
        }
        if query.starts_with("query Issues") {
            return Ok(issues(cursor));
        }
        if query.starts_with("query PullRequests") {
            assert_eq!(cursor, None, "the walk stops before the next page");
            return Ok(pulls());
        }
        if query.starts_with("query More") {
            assert!(query.contains("subIssues"), "{query}");
            assert_eq!(cursor, Some("sub-1"));
            return Ok(
                json!({"data": {"node": {"subIssues": page(vec![reference(14, "acme/shop")], None)}}}),
            );
        }
        panic!("unexpected request: {query}")
    }
}

#[test]
fn a_fetch_reads_items_and_links_with_every_page_and_counts_what_it_cannot_see() {
    let requests = RefCell::new(Vec::new());
    let snapshot = fetch(
        &options(Some("2025-10-03T00:00:00Z")),
        &mut recorded(true, &requests),
    )
    .unwrap();
    assert_eq!(snapshot.repository, "acme/shop");
    assert_eq!(
        snapshot
            .items
            .iter()
            .map(|i| (i.number, i.state))
            .collect::<Vec<_>>(),
        [
            (9, ItemState::Open),
            (12, ItemState::Closed),
            (15, ItemState::Merged)
        ]
    );
    assert_eq!(
        (snapshot.range.issues, snapshot.range.pull_requests),
        (2, 1)
    );
    let pull = snapshot.item(15).unwrap();
    assert_eq!(pull.commits, ["a1", "a2"]);
    assert_eq!(pull.commit_count, Some(3));
    assert_eq!(pull.merge_commit.as_deref(), Some("m1"));
    assert_eq!(
        snapshot.item(12).unwrap().state_reason.as_deref(),
        Some("completed")
    );
    let links = |kind: RelationType| -> Vec<(Ref, Ref, Vec<String>)> {
        snapshot
            .relations
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| (r.from.clone(), r.to.clone(), r.observed.clone()))
            .collect()
    };
    let item = |n| Ref::Item { item: n };
    // the second page of sub-issues, and the null node counted
    assert_eq!(
        links(RelationType::SubIssue)
            .iter()
            .map(|(f, t, _)| (f.clone(), t.clone()))
            .collect::<Vec<_>>(),
        [
            (item(10), item(12)),
            (item(12), item(13)),
            (item(12), item(14))
        ]
    );
    // seen from both ends: one link, two observations
    assert_eq!(
        links(RelationType::Closes),
        [(
            item(15),
            item(12),
            vec![
                "Issue.closedByPullRequestsReferences(excludeUserLinked)".to_owned(),
                "PullRequest.closingIssuesReferences(excludeUserLinked)".to_owned()
            ]
        )]
    );
    assert_eq!(links(RelationType::BlockedBy)[0].1, item(9));
    // another repository's item, foreign
    assert_eq!(
        links(RelationType::CrossReferenced)[0].0,
        Ref::Foreign {
            repository: "acme/web".into(),
            number: 4
        }
    );
    assert_eq!(links(RelationType::ClosedBy)[0].1, item(15));
    assert_eq!(
        links(RelationType::Referenced)[0].0,
        Ref::Commit {
            commit: "a1".into(),
            repository: None
        }
    );
    assert_eq!(snapshot.not_visible.links[&RelationType::SubIssue], 1);
    assert_eq!(
        snapshot.not_visible.links[&RelationType::CrossReferenced],
        1
    );
    // owner and name go as variables, never into the query; no token
    for request in requests.borrow().iter() {
        let query = request["query"].as_str().unwrap();
        assert!(
            !query.contains("acme") && !query.contains("shop"),
            "{query}"
        );
        let text = request.to_string().to_lowercase();
        assert!(
            !text.contains("authorization") && !text.contains("token"),
            "{text}"
        );
    }
    let written = serde_json::to_string(&snapshot).unwrap();
    for shape in ["ghp_", "gho_", "ghs_", "github_pat_"] {
        assert!(!written.contains(shape));
    }
}

#[test]
fn a_host_without_some_fields_fetches_the_other_types_and_says_which_it_lacks() {
    let requests = RefCell::new(Vec::new());
    let snapshot = fetch(
        &options(Some("2025-10-03T00:00:00Z")),
        &mut recorded(false, &requests),
    )
    .unwrap();
    assert_eq!(
        snapshot.unavailable,
        [RelationType::SubIssue, RelationType::BlockedBy]
    );
    assert!(!snapshot.relation_types.contains(&RelationType::SubIssue));
    // nor asked for
    for request in requests.borrow().iter() {
        let query = request["query"].as_str().unwrap();
        assert!(
            !query.contains("subIssues") && !query.contains("blockedBy"),
            "{query}"
        );
    }
}

#[test]
fn an_error_fails_the_whole_fetch() {
    let mut calls = 0;
    let mut failing = |request: &Value| {
        calls += 1;
        match request["query"].as_str().unwrap() {
            q if q.starts_with("query Schema") => Ok(schema(true)),
            _ => Ok(json!({"errors": [{"message": "API rate limit exceeded"}]})),
        }
    };
    let error = fetch(&options(None), &mut failing).unwrap_err();
    assert!(error.contains("API rate limit exceeded"), "{error}");
}

#[test]
fn the_bound_stops_the_read_and_says_from_when_the_range_holds_everything() {
    let requests = RefCell::new(Vec::new());
    let mut bounded = options(None);
    bounded.max_items = 1;
    let snapshot = fetch(&bounded, &mut recorded(true, &requests)).unwrap();
    assert!(snapshot.range.reached_bound);
    assert_eq!(snapshot.range.issues, 1);
    assert_eq!(
        snapshot.range.effective_since.as_deref(),
        Some("2026-01-04T00:00:00Z")
    );
    // without titles, numbers and states only
    let mut untitled = options(Some("2025-10-03T00:00:00Z"));
    untitled.titles = false;
    let snapshot = fetch(&untitled, &mut recorded(true, &RefCell::new(Vec::new()))).unwrap();
    assert!(snapshot.items.iter().all(|i| i.title.is_none()));
    assert!(snapshot.no_titles);
}

#[test]
fn an_item_updated_at_the_date_asked_is_in_the_range() {
    let requests = RefCell::new(Vec::new());
    // #15 was updated at exactly this time
    let snapshot = fetch(
        &options(Some("2026-01-04T00:00:00Z")),
        &mut recorded(true, &requests),
    )
    .unwrap();
    assert!(snapshot.item(15).is_some());
    assert!(snapshot.item(14).is_none());
}

#[test]
fn an_item_updated_during_the_fetch_is_read_again_and_its_last_read_stays() {
    let mut pass = 0;
    let mut moving = |request: &Value| {
        let query = request["query"].as_str().unwrap();
        let again = request["variables"]["since"].as_str() == Some("2026-10-03T08:55:00Z");
        if query.starts_with("query Schema") {
            return Ok(schema(true));
        }
        if query.starts_with("query Issues") {
            pass += 1;
            let state = if again { "CLOSED" } else { "OPEN" };
            let updated = if again {
                "2026-10-03T09:01:00Z"
            } else {
                "2026-01-02T00:00:00Z"
            };
            return Ok(
                json!({"data": {"repository": {"nameWithOwner": "acme/shop", "issues": page(vec![json!({
                "id": "I_9", "number": 9, "title": "Rates", "state": state, "stateReason": null,
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": updated, "closedAt": null,
                "parent": null, "duplicateOf": null, "subIssues": empty(), "blockedBy": empty(),
                "blocking": empty(), "closes": empty(), "linked": empty(), "events": empty(),
            })], None)}}}),
            );
        }
        if query.starts_with("query PullRequests") {
            return Ok(json!({"data": {"repository": {"pullRequests": empty()}}}));
        }
        panic!("unexpected request: {query}")
    };
    let snapshot = fetch(&options(None), &mut moving).unwrap();
    assert_eq!(pass, 2, "the issues are walked, then read again");
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].state, ItemState::Closed);
    assert_eq!(snapshot.range.issues, 1);
}

#[test]
fn an_error_says_where_in_the_answer_it_arose() {
    let mut failing = |request: &Value| match request["query"].as_str().unwrap() {
        q if q.starts_with("query Schema") => Ok(schema(true)),
        _ => Ok(json!({"errors": [{"message": "Resource not accessible",
                                   "path": ["repository", "issues", "nodes", 3, "events"]}]})),
    };
    let error = fetch(&options(None), &mut failing).unwrap_err();
    assert!(
        error.contains("Resource not accessible (at repository.issues.nodes.3.events)"),
        "{error}"
    );
}

/// The options of an update a day after the fetch `options` describes.
fn next_day() -> FetchOptions {
    FetchOptions {
        now: "2026-10-04T09:00:00Z".into(),
        reread_since: "2026-10-04T08:55:00Z".into(),
        ..options(Some("2025-10-03T00:00:00Z"))
    }
}

#[test]
fn an_update_reads_the_items_changed_since_the_fetch_and_keeps_the_others() {
    let previous = fetch(
        &options(Some("2025-10-03T00:00:00Z")),
        &mut recorded(true, &RefCell::new(Vec::new())),
    )
    .unwrap();
    assert_eq!(previous.account.as_ref().unwrap().login, "fetcher");
    let requests = RefCell::new(Vec::new());
    let mut changed = |request: &Value| {
        requests.borrow_mut().push(request.clone());
        let query = request["query"].as_str().unwrap();
        let since = request["variables"]["since"].as_str();
        match query {
            q if q.starts_with("query Schema") => Ok(schema(true)),
            // what changed since the fetch, five minutes before it included:
            // #9, closed, cross-referenced by #20
            q if q.starts_with("query Issues") && since == Some("2026-10-03T08:55:00Z") => Ok(
                json!({"data": {"repository": {"nameWithOwner": "acme/shop", "issues": page(vec![json!({
                    "id": "I_9", "number": 9, "title": "Rates", "state": "CLOSED",
                    "stateReason": "NOT_PLANNED", "createdAt": "2026-01-01T00:00:00Z",
                    "updatedAt": "2026-10-03T12:00:00Z", "closedAt": "2026-10-03T12:00:00Z",
                    "parent": null, "duplicateOf": null, "subIssues": empty(),
                    "blockedBy": empty(), "blocking": page(vec![reference(12, "acme/shop")], None),
                    "closes": empty(), "linked": empty(),
                    "events": page(vec![json!({"__typename": "CrossReferencedEvent",
                        "referencedAt": "2026-10-03T11:00:00Z", "willCloseTarget": false,
                        "source": {"__typename": "Issue", "number": 20,
                                   "repository": {"nameWithOwner": "acme/shop"}}})], None),
                })], None)}}}),
            ),
            q if q.starts_with("query Issues") => Ok(
                json!({"data": {"repository": {"nameWithOwner": "acme/shop", "issues": empty()}}}),
            ),
            // the newest pull request was updated before: the walk stops
            q if q.starts_with("query PullRequests") => Ok(pulls()),
            q => panic!("unexpected request: {q}"),
        }
    };
    let Update::Merged { snapshot, read } =
        update(&next_day(), &mut changed, &previous, "2026-10-01T00:00:00Z").unwrap()
    else {
        panic!("fetched whole")
    };
    assert_eq!(read, 1);
    let nine = snapshot.item(9).unwrap();
    assert_eq!(
        (nine.state, nine.state_reason.as_deref()),
        (ItemState::Closed, Some("not_planned"))
    );
    let item = |n| Ref::Item { item: n };
    assert!(snapshot
        .relations
        .iter()
        .any(|r| r.kind == RelationType::CrossReferenced && r.from == item(20) && r.to == item(9)));
    // the links of #12 and #15 stay: #9, read, gives again the one both
    // ends record
    let unchanged = |s: &archmap_core::work::Snapshot| -> Vec<_> {
        s.relations
            .iter()
            .filter(|r| r.to == item(12) || r.from == item(12))
            .map(|r| (r.kind, r.from.clone(), r.to.clone(), r.at.clone()))
            .collect()
    };
    assert_eq!(unchanged(&snapshot), unchanged(&previous));
    assert_eq!(snapshot.item(15), previous.item(15));
    assert_eq!(snapshot.not_visible, previous.not_visible);
    assert_eq!(snapshot.range, {
        let mut range = previous.range.clone();
        range.issues = 2;
        range
    });
    assert_eq!(snapshot.fetched_at, "2026-10-04T09:00:00Z");
    assert_eq!(
        snapshot.whole_fetched_at.as_deref(),
        Some("2026-10-03T09:00:00Z")
    );
    // the walks start five minutes before the fetch, and the rest is not
    // read again
    let asked: Vec<String> = requests
        .borrow()
        .iter()
        .map(|r| {
            let query = r["query"].as_str().unwrap();
            let name = query.split([' ', '(']).nth(1).unwrap_or("").to_owned();
            format!("{name} {}", r["variables"]["since"].as_str().unwrap_or("-"))
        })
        .collect();
    assert_eq!(
        asked,
        [
            "Schema -",
            "Issues 2026-10-03T08:55:00Z",
            "PullRequests -",
            "Issues 2026-10-04T08:55:00Z",
            "PullRequests -"
        ]
    );
}

#[test]
fn an_update_fetches_whole_when_the_snapshot_was_fetched_otherwise() {
    let previous = fetch(
        &options(Some("2025-10-03T00:00:00Z")),
        &mut recorded(true, &RefCell::new(Vec::new())),
    )
    .unwrap();
    let whole = |options: &FetchOptions,
                 previous: &archmap_core::work::Snapshot,
                 after: &str,
                 viewer: (&str, u64)|
     -> (String, usize) {
        let requests = RefCell::new(0);
        let mut run = |request: &Value| {
            *requests.borrow_mut() += 1;
            let query = request["query"].as_str().unwrap();
            assert!(query.starts_with("query Schema"), "{query}");
            let mut answer = schema(true);
            answer["data"]["viewer"]["login"] = viewer.0.into();
            answer["data"]["viewer"]["databaseId"] = viewer.1.into();
            Ok(answer)
        };
        match update(options, &mut run, previous, after).unwrap() {
            Update::Whole { reason } => (reason, *requests.borrow()),
            Update::Merged { .. } => panic!("merged"),
        }
    };
    // decided before any request
    let old = whole(
        &next_day(),
        &previous,
        "2026-10-03T10:00:00Z",
        ("fetcher", 7),
    );
    assert_eq!(
        old,
        ("its last whole fetch is older than the limit".to_owned(), 0)
    );
    let untitled = FetchOptions {
        titles: false,
        ..next_day()
    };
    assert_eq!(
        whole(&untitled, &previous, "2026-10-01T00:00:00Z", ("fetcher", 7)),
        (
            "the snapshot was fetched with the other title setting".to_owned(),
            0
        )
    );
    let elsewhere = FetchOptions {
        name: "web".into(),
        ..next_day()
    };
    assert_eq!(
        whole(
            &elsewhere,
            &previous,
            "2026-10-01T00:00:00Z",
            ("fetcher", 7)
        ),
        ("the snapshot is of another repository".to_owned(), 0)
    );
    let bounded = FetchOptions {
        max_items: 10,
        ..next_day()
    };
    assert_eq!(
        whole(&bounded, &previous, "2026-10-01T00:00:00Z", ("fetcher", 7)),
        ("the snapshot was fetched with another bound".to_owned(), 0)
    );
    let mut written_before = previous.clone();
    written_before.whole_fetched_at = None;
    assert_eq!(
        whole(
            &next_day(),
            &written_before,
            "2026-10-01T00:00:00Z",
            ("fetcher", 7)
        ),
        ("the snapshot records no whole fetch".to_owned(), 0)
    );
    // decided once the host answers
    let mut anonymous = previous.clone();
    anonymous.account = None;
    assert_eq!(
        whole(
            &next_day(),
            &anonymous,
            "2026-10-01T00:00:00Z",
            ("fetcher", 7)
        ),
        ("the snapshot records no account".to_owned(), 1)
    );
    let mut fewer = previous.clone();
    fewer.unavailable = vec![RelationType::SubIssue];
    assert_eq!(
        whole(&next_day(), &fewer, "2026-10-01T00:00:00Z", ("fetcher", 7)).0,
        "the host offers other link types than when the snapshot was fetched"
    );
    // another account answers: by id, whatever the login
    assert_eq!(
        whole(
            &next_day(),
            &previous,
            "2026-10-01T00:00:00Z",
            ("fetcher", 8)
        ),
        ("the snapshot was fetched as another account".to_owned(), 1)
    );
}

#[test]
fn every_observation_a_fetch_writes_names_the_end_that_recorded_it() {
    let snapshot = fetch(
        &options(Some("2025-10-03T00:00:00Z")),
        &mut recorded(true, &RefCell::new(Vec::new())),
    )
    .unwrap();
    // an update replaces what an item read records only through this
    let observed: Vec<&str> = snapshot
        .relations
        .iter()
        .flat_map(|r| r.observed.iter().map(String::as_str))
        .collect();
    assert!(observed.len() >= 8, "{observed:?}");
    for name in observed {
        assert!(
            archmap_core::work::recorded_by(name).is_some(),
            "no end for {name}"
        );
    }
}

#[test]
fn an_update_that_reaches_the_bound_or_fails_writes_nothing_of_its_own() {
    let bounded = FetchOptions {
        max_items: 1,
        ..options(Some("2025-10-03T00:00:00Z"))
    };
    let previous = fetch(&bounded, &mut recorded(true, &RefCell::new(Vec::new()))).unwrap();
    // two issues changed, one read at most: an update would miss one
    let next = FetchOptions {
        max_items: 1,
        ..next_day()
    };
    let mut many = |request: &Value| {
        let query = request["query"].as_str().unwrap();
        match query {
            q if q.starts_with("query Schema") => Ok(schema(true)),
            q if q.starts_with("query Issues") => {
                Ok(issues(request["variables"]["cursor"].as_str()))
            }
            q if q.starts_with("query More") => Ok(
                json!({"data": {"node": {"subIssues": page(vec![reference(14, "acme/shop")], None)}}}),
            ),
            q => panic!("unexpected request: {q}"),
        }
    };
    match update(&next, &mut many, &previous, "2026-10-01T00:00:00Z").unwrap() {
        Update::Whole { reason } => assert_eq!(
            reason,
            "more items changed since its fetch than the bound reads"
        ),
        Update::Merged { .. } => panic!("merged"),
    }
    // an error fails the update whole
    let mut failing = |request: &Value| match request["query"].as_str().unwrap() {
        q if q.starts_with("query Schema") => Ok(schema(true)),
        _ => Err("HTTP 502".to_owned()),
    };
    let previous = fetch(
        &options(Some("2025-10-03T00:00:00Z")),
        &mut recorded(true, &RefCell::new(Vec::new())),
    )
    .unwrap();
    assert_eq!(
        update(&next_day(), &mut failing, &previous, "2026-10-01T00:00:00Z").unwrap_err(),
        "HTTP 502"
    );
}
