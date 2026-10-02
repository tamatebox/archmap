//! A GitHub fetch from recorded GraphQL answers with invented names: no
//! network, no gh. The transport is a closure that answers each request by
//! its operation and cursor.

use std::cell::RefCell;

use archmap_core::work::{ItemState, Ref, RelationType, SinceRule};
use archmap_scan::work::github::{fetch, FetchOptions};
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
            "closedAt": null, "parent": null, "duplicateOf": null,
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
