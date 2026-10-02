//! Fetch a GitHub snapshot through the GraphQL API, with the gh CLI as the
//! only transport: gh holds the token (`GH_TOKEN`, `GITHUB_TOKEN` or its
//! login) and archmap never sees it. Only an explicit fetch runs this;
//! nothing else in archmap reaches the network.
//!
//! The fetch asks the schema first and leaves out the link types whose
//! fields the host lacks, reads the issues and pull requests updated since a
//! date, newest first up to a bound, pages every nested connection to its
//! end, and fails whole on any error: a snapshot is complete within its
//! range or absent.

use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, Stdio};

use archmap_core::work::{
    Item, ItemKind, ItemState, NotVisible, Range, Ref, Relation, RelationType, SinceRule, Snapshot,
    WORK_SCHEMA,
};
use serde_json::{json, Value};

/// Issues and pull requests per page; nested connections per page.
const ISSUES_PAGE: usize = 50;
const PULLS_PAGE: usize = 25;
const LINKS_PAGE: usize = 20;
const EVENTS_PAGE: usize = 50;
const COMMITS_PAGE: usize = 100;

/// What to fetch.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub host: String,
    pub owner: String,
    pub name: String,
    /// Items updated since this time (RFC 3339), and what set it; `None`
    /// for every item.
    pub since: Option<String>,
    pub since_rule: SinceRule,
    /// Items read per kind at most.
    pub max_items: usize,
    pub titles: bool,
    /// When the fetch starts (RFC 3339, UTC).
    pub now: String,
    /// Items updated since this time are read again after both walks, so
    /// one updated during the fetch, which moves behind the walk's cursor,
    /// is not lost: the start, less a margin for clocks that disagree.
    pub reread_since: String,
}

/// One GraphQL request (`{"query", "variables"}`) to its response's JSON.
pub type Run<'a> = dyn FnMut(&Value) -> Result<Value, String> + 'a;

/// The gh CLI as the transport for `host`: `gh api graphql --input -`, the
/// request on stdin, prompts off. Its error message is passed on; nothing
/// of its environment is.
pub fn gh(host: &str) -> impl FnMut(&Value) -> Result<Value, String> + '_ {
    move |request: &Value| {
        let mut child = Command::new("gh")
            .args(["api", "graphql", "--hostname", host, "--input", "-"])
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => {
                    "the gh CLI is not installed: see https://cli.github.com".to_owned()
                }
                _ => format!("running gh: {error}"),
            })?;
        let body = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .ok_or("gh took no input")?
            .write_all(&body)
            .map_err(|e| format!("writing to gh: {e}"))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("running gh: {e}"))?;
        if !out.status.success() {
            let message = String::from_utf8_lossy(&out.stderr);
            let first = message.lines().next().unwrap_or("").trim();
            return Err(format!(
                "gh failed: {first} (log in with `gh auth login --hostname {host}`, or set \
                 GH_TOKEN)"
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("gh answered no JSON: {e}"))
    }
}

/// Fetch a snapshot.
pub fn fetch(options: &FetchOptions, run: &mut Run) -> Result<Snapshot, String> {
    let fields = schema(run)?;
    let has = |owner: &str, field: &str| fields.contains(&(owner.to_owned(), field.to_owned()));
    let closing = has("Issue", "closedByPullRequestsReferences.userLinkedOnly")
        && has("PullRequest", "closingIssuesReferences.userLinkedOnly");
    let offered: Vec<(RelationType, bool)> = vec![
        (
            RelationType::SubIssue,
            has("Issue", "subIssues") && has("Issue", "parent"),
        ),
        (
            RelationType::BlockedBy,
            has("Issue", "blockedBy") && has("Issue", "blocking"),
        ),
        (RelationType::Closes, closing),
        (RelationType::Linked, closing),
        (RelationType::ClosedBy, true),
        (RelationType::CrossReferenced, true),
        (RelationType::Referenced, true),
        (RelationType::DuplicateOf, has("Issue", "duplicateOf")),
    ];
    let types: BTreeSet<RelationType> = offered
        .iter()
        .filter(|(_, ok)| *ok)
        .map(|(t, _)| *t)
        .collect();
    let unavailable: Vec<RelationType> = offered
        .iter()
        .filter(|(_, ok)| !*ok)
        .map(|(t, _)| *t)
        .collect();

    let mut fetch = Fetch {
        options,
        types,
        repository: format!("{}/{}", options.owner, options.name),
        items: Vec::new(),
        relations: Vec::new(),
        not_visible: NotVisible::default(),
    };
    let since = options.since.as_deref();
    let issues_bounded = fetch.issues(run, since, options.max_items)?;
    let pulls_bounded = fetch.pulls(run, since, options.max_items)?;
    // what changed while the walks ran, read again: the last read stays
    let again = Some(options.reread_since.as_str());
    fetch.issues(run, again, usize::MAX)?;
    fetch.pulls(run, again, usize::MAX)?;
    let reached_bound = issues_bounded.is_some() || pulls_bounded.is_some();
    let effective_since = [issues_bounded, pulls_bounded].into_iter().flatten().max();
    let mut snapshot = Snapshot {
        schema: WORK_SCHEMA,
        source: "github".to_owned(),
        host: options.host.clone(),
        repository: fetch.repository.clone(),
        fetched_at: options.now.clone(),
        range: Range {
            updated_since: options.since.clone(),
            since_rule: options.since_rule,
            bound: options.max_items,
            reached_bound,
            effective_since,
            issues: 0,
            pull_requests: 0,
        },
        relation_types: fetch.types.iter().copied().collect(),
        unavailable,
        no_titles: !options.titles,
        items: fetch.items,
        relations: fetch.relations,
        not_visible: fetch.not_visible,
    };
    snapshot.normalize();
    let count = |kind| snapshot.items.iter().filter(|i| i.kind == kind).count();
    snapshot.range.issues = count(ItemKind::Issue);
    snapshot.range.pull_requests = count(ItemKind::PullRequest);
    Ok(snapshot)
}

/// The fields of `Issue` and `PullRequest` the host offers, as `(type,
/// field)`, and `(type, field.argument)` for their arguments.
fn schema(run: &mut Run) -> Result<BTreeSet<(String, String)>, String> {
    let request = json!({
        "query": "query Schema { issue: __type(name: \"Issue\") { fields { name args { name } } } \
                  pull: __type(name: \"PullRequest\") { fields { name args { name } } } }"
    });
    let answer = data(run(&request)?)?;
    let mut found = BTreeSet::new();
    for (key, owner) in [("issue", "Issue"), ("pull", "PullRequest")] {
        for field in answer[key]["fields"].as_array().into_iter().flatten() {
            let Some(name) = field["name"].as_str() else {
                continue;
            };
            found.insert((owner.to_owned(), name.to_owned()));
            for arg in field["args"].as_array().into_iter().flatten() {
                if let Some(arg) = arg["name"].as_str() {
                    found.insert((owner.to_owned(), format!("{name}.{arg}")));
                }
            }
        }
    }
    Ok(found)
}

/// A response's `data`, or its errors as one message, each with where in
/// the answer it arose: one item the account cannot read fails the fetch,
/// and the path says which.
fn data(response: Value) -> Result<Value, String> {
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        let messages: Vec<String> = errors
            .iter()
            .map(|e| {
                let message = e["message"].as_str().unwrap_or("an error");
                let path: Vec<String> = e["path"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| match p {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                match path.is_empty() {
                    true => message.to_owned(),
                    false => format!("{message} (at {})", path.join(".")),
                }
            })
            .collect();
        return Err(format!("GitHub answered: {}", messages.join("; ")));
    }
    response
        .get("data")
        .cloned()
        .ok_or_else(|| "GitHub answered without data".to_owned())
}

struct Fetch<'o> {
    options: &'o FetchOptions,
    types: BTreeSet<RelationType>,
    repository: String,
    items: Vec<Item>,
    relations: Vec<Relation>,
    not_visible: NotVisible,
}

/// A nested connection to page on from a node, with what its nodes are.
struct Nested {
    /// The connection as the query writes it, alias first
    /// (`closes: closedByPullRequestsReferences`).
    field: &'static str,
    /// Its arguments beyond `first` and `after`.
    args: &'static str,
    /// The selection of one node.
    selection: &'static str,
}

const REF: &str = "number repository { nameWithOwner }";
const EVENT: &str = "__typename \
    ... on CrossReferencedEvent { referencedAt willCloseTarget source { __typename \
      ... on Issue { number repository { nameWithOwner } } \
      ... on PullRequest { number repository { nameWithOwner } } } } \
    ... on ReferencedEvent { createdAt commit { oid } commitRepository { nameWithOwner } } \
    ... on ClosedEvent { createdAt closer { __typename ... on Commit { oid repository { nameWithOwner } } \
      ... on PullRequest { number repository { nameWithOwner } } } }";

impl Fetch<'_> {
    fn has(&self, kind: RelationType) -> bool {
        self.types.contains(&kind)
    }

    /// The nested connections an issue's page asks for.
    fn issue_connections(&self) -> Vec<Nested> {
        let mut nested = Vec::new();
        if self.has(RelationType::SubIssue) {
            nested.push(Nested {
                field: "subIssues",
                args: "",
                selection: REF,
            });
        }
        if self.has(RelationType::BlockedBy) {
            nested.push(Nested {
                field: "blockedBy",
                args: "",
                selection: REF,
            });
            nested.push(Nested {
                field: "blocking",
                args: "",
                selection: REF,
            });
        }
        if self.has(RelationType::Closes) {
            nested.push(Nested {
                field: "closes: closedByPullRequestsReferences",
                args: ", includeClosedPrs: true, excludeUserLinked: true",
                selection: REF,
            });
            nested.push(Nested {
                field: "linked: closedByPullRequestsReferences",
                args: ", includeClosedPrs: true, userLinkedOnly: true",
                selection: REF,
            });
        }
        nested.push(Nested {
            field: "events: timelineItems",
            args: ", itemTypes: [CROSS_REFERENCED_EVENT, REFERENCED_EVENT, CLOSED_EVENT]",
            selection: EVENT,
        });
        nested
    }

    fn pull_connections(&self) -> Vec<Nested> {
        let mut nested = vec![Nested {
            field: "commits",
            args: "",
            selection: "commit { oid }",
        }];
        if self.has(RelationType::Closes) {
            nested.push(Nested {
                field: "closes: closingIssuesReferences",
                args: ", excludeUserLinked: true",
                selection: REF,
            });
            nested.push(Nested {
                field: "linked: closingIssuesReferences",
                args: ", userLinkedOnly: true",
                selection: REF,
            });
        }
        nested.push(Nested {
            field: "events: timelineItems",
            args: ", itemTypes: [CROSS_REFERENCED_EVENT, REFERENCED_EVENT]",
            selection: EVENT,
        });
        nested
    }

    /// Read the issues updated since `since`, at most `bound`; when the
    /// bound stopped the read, the oldest update read.
    fn issues(
        &mut self,
        run: &mut Run,
        since: Option<&str>,
        bound: usize,
    ) -> Result<Option<String>, String> {
        let mut scalars = String::from(
            "id number title state stateReason(enableDuplicate: true) createdAt updatedAt closedAt",
        );
        if self.has(RelationType::SubIssue) {
            scalars.push_str(&format!(" parent {{ {REF} }}"));
        }
        if self.has(RelationType::DuplicateOf) {
            scalars.push_str(&format!(" duplicateOf {{ {REF} }}"));
        }
        let nested = self.issue_connections();
        let query = format!(
            "query Issues($owner: String!, $name: String!, $since: DateTime, $cursor: String) {{ \
             repository(owner: $owner, name: $name) {{ nameWithOwner \
             issues(first: {ISSUES_PAGE}, after: $cursor, orderBy: {{field: UPDATED_AT, \
             direction: DESC}}, filterBy: {{since: $since}}) {{ pageInfo {{ hasNextPage endCursor }} \
             nodes {{ {scalars} {} }} }} }} }}",
            connections(&nested, false)
        );
        let mut cursor: Option<String> = None;
        let mut count = 0;
        loop {
            let variables = json!({
                "owner": self.options.owner,
                "name": self.options.name,
                "since": since,
                "cursor": cursor,
            });
            let answer = data(run(&json!({"query": query, "variables": variables}))?)?;
            let repository = &answer["repository"];
            if let Some(name) = repository["nameWithOwner"].as_str() {
                self.repository = name.to_owned();
            }
            let page = &repository["issues"];
            for node in page["nodes"].as_array().into_iter().flatten() {
                if node.is_null() || node["number"].as_u64().is_none() {
                    self.not_visible.items += 1;
                    continue;
                }
                if count == bound {
                    // one more exists: the bound stops the read here
                    let oldest = self
                        .items
                        .iter()
                        .filter(|i| i.kind == ItemKind::Issue)
                        .map(|i| i.updated_at.clone())
                        .min();
                    return Ok(oldest);
                }
                let node = self.complete(run, node, &nested, "Issue")?;
                self.issue(&node);
                count += 1;
            }
            match next(page) {
                Some(next) => cursor = Some(next),
                None => return Ok(None),
            }
        }
    }

    /// Read the pull requests, newest update first, up to the first one
    /// updated before `since` and at most `bound`; when the bound stopped
    /// the read, the oldest update read.
    fn pulls(
        &mut self,
        run: &mut Run,
        since: Option<&str>,
        bound: usize,
    ) -> Result<Option<String>, String> {
        let nested = self.pull_connections();
        let query = format!(
            "query PullRequests($owner: String!, $name: String!, $cursor: String) {{ \
             repository(owner: $owner, name: $name) {{ \
             pullRequests(first: {PULLS_PAGE}, after: $cursor, orderBy: {{field: UPDATED_AT, \
             direction: DESC}}) {{ pageInfo {{ hasNextPage endCursor }} \
             nodes {{ id number title state createdAt updatedAt closedAt mergedAt \
             mergeCommit {{ oid }} {} }} }} }} }}",
            connections(&nested, true)
        );
        let mut cursor: Option<String> = None;
        let mut count = 0;
        loop {
            let variables = json!({
                "owner": self.options.owner,
                "name": self.options.name,
                "cursor": cursor,
            });
            let answer = data(run(&json!({"query": query, "variables": variables}))?)?;
            let page = &answer["repository"]["pullRequests"];
            for node in page["nodes"].as_array().into_iter().flatten() {
                if node.is_null() || node["number"].as_u64().is_none() {
                    self.not_visible.items += 1;
                    continue;
                }
                let updated = node["updatedAt"].as_str().unwrap_or("");
                if since.is_some_and(|since| updated < since) {
                    return Ok(None);
                }
                if count == bound {
                    let oldest = self
                        .items
                        .iter()
                        .filter(|i| i.kind == ItemKind::PullRequest)
                        .map(|i| i.updated_at.clone())
                        .min();
                    return Ok(oldest);
                }
                let node = self.complete(run, node, &nested, "PullRequest")?;
                self.pull(&node);
                count += 1;
            }
            match next(page) {
                Some(next) => cursor = Some(next),
                None => return Ok(None),
            }
        }
    }

    /// `node` with every nested connection paged to its end.
    fn complete(
        &self,
        run: &mut Run,
        node: &Value,
        nested: &[Nested],
        kind: &str,
    ) -> Result<Value, String> {
        let mut node = node.clone();
        for connection in nested {
            let key = connection.field.split(':').next().unwrap_or("").trim();
            let mut cursor = next(&node[key]);
            while let Some(after) = cursor {
                let query = format!(
                    "query More($id: ID!, $cursor: String) {{ node(id: $id) {{ ... on {kind} {{ \
                     {}(first: {}, after: $cursor{}) {{ pageInfo {{ hasNextPage endCursor }} \
                     nodes {{ {} }} }} }} }} }}",
                    connection.field,
                    page_size(connection.field),
                    connection.args,
                    connection.selection
                );
                let variables = json!({"id": node["id"], "cursor": after});
                let answer = data(run(&json!({"query": query, "variables": variables}))?)?;
                let more = &answer["node"][key];
                let extra = more["nodes"].as_array().cloned().unwrap_or_default();
                if let Some(nodes) = node[key]["nodes"].as_array_mut() {
                    nodes.extend(extra);
                }
                cursor = next(more);
            }
        }
        Ok(node)
    }

    fn reference(&mut self, value: &Value, kind: RelationType) -> Option<Ref> {
        let (Some(number), Some(repository)) = (
            value["number"].as_u64(),
            value["repository"]["nameWithOwner"].as_str(),
        ) else {
            *self.not_visible.links.entry(kind).or_default() += 1;
            return None;
        };
        Some(match repository.eq_ignore_ascii_case(&self.repository) {
            true => Ref::Item { item: number },
            false => Ref::Foreign {
                repository: repository.to_owned(),
                number,
            },
        })
    }

    fn commit(&self, oid: &str, repository: Option<&str>) -> Ref {
        Ref::Commit {
            commit: oid.to_owned(),
            repository: repository
                .filter(|r| !r.eq_ignore_ascii_case(&self.repository))
                .map(str::to_owned),
        }
    }

    fn link(&mut self, kind: RelationType, from: Ref, to: Ref, observed: &str) -> &mut Relation {
        self.relations.push(Relation {
            kind,
            from,
            to,
            observed: vec![observed.to_owned()],
            at: None,
            will_close: None,
        });
        self.relations.last_mut().expect("just pushed")
    }

    /// Each node of `connection` as a reference, the unreadable ones
    /// counted for `kind`.
    fn refs(&mut self, connection: &Value, kind: RelationType) -> Vec<Ref> {
        connection["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|node| self.reference(node, kind))
            .collect()
    }

    fn issue(&mut self, node: &Value) {
        // a page skips a node without a number, counting it
        let number = node["number"].as_u64().unwrap_or_default();
        let this = Ref::Item { item: number };
        let closed = node["state"].as_str() == Some("CLOSED");
        self.items.push(Item {
            kind: ItemKind::Issue,
            number,
            id: string(&node["id"]),
            title: self.title(node),
            state: match closed {
                true => ItemState::Closed,
                false => ItemState::Open,
            },
            state_reason: node["stateReason"]
                .as_str()
                .filter(|_| closed)
                .map(str::to_ascii_lowercase),
            created_at: string(&node["createdAt"]),
            updated_at: string(&node["updatedAt"]),
            closed_at: node["closedAt"].as_str().map(str::to_owned),
            merged_at: None,
            merge_commit: None,
            commits: Vec::new(),
            commit_count: None,
        });
        if self.has(RelationType::SubIssue) {
            if !node["parent"].is_null() {
                if let Some(parent) = self.reference(&node["parent"], RelationType::SubIssue) {
                    self.link(RelationType::SubIssue, parent, this.clone(), "Issue.parent");
                }
            }
            for child in self.refs(&node["subIssues"], RelationType::SubIssue) {
                self.link(
                    RelationType::SubIssue,
                    this.clone(),
                    child,
                    "Issue.subIssues",
                );
            }
        }
        if self.has(RelationType::BlockedBy) {
            for blocker in self.refs(&node["blockedBy"], RelationType::BlockedBy) {
                self.link(
                    RelationType::BlockedBy,
                    this.clone(),
                    blocker,
                    "Issue.blockedBy",
                );
            }
            for blocked in self.refs(&node["blocking"], RelationType::BlockedBy) {
                self.link(
                    RelationType::BlockedBy,
                    blocked,
                    this.clone(),
                    "Issue.blocking",
                );
            }
        }
        if self.has(RelationType::DuplicateOf) && !node["duplicateOf"].is_null() {
            if let Some(canonical) = self.reference(&node["duplicateOf"], RelationType::DuplicateOf)
            {
                self.link(
                    RelationType::DuplicateOf,
                    this.clone(),
                    canonical,
                    "Issue.duplicateOf",
                );
            }
        }
        if self.has(RelationType::Closes) {
            for pull in self.refs(&node["closes"], RelationType::Closes) {
                self.link(
                    RelationType::Closes,
                    pull,
                    this.clone(),
                    "Issue.closedByPullRequestsReferences(excludeUserLinked)",
                );
            }
            for pull in self.refs(&node["linked"], RelationType::Linked) {
                self.link(
                    RelationType::Linked,
                    pull,
                    this.clone(),
                    "Issue.closedByPullRequestsReferences(userLinkedOnly)",
                );
            }
        }
        self.events(&node["events"], &this);
    }

    fn pull(&mut self, node: &Value) {
        // a page skips a node without a number, counting it
        let number = node["number"].as_u64().unwrap_or_default();
        let this = Ref::Item { item: number };
        let commits: Vec<String> = node["commits"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["commit"]["oid"].as_str().map(str::to_owned))
            .collect();
        self.items.push(Item {
            kind: ItemKind::PullRequest,
            number,
            id: string(&node["id"]),
            title: self.title(node),
            state: match node["state"].as_str() {
                Some("MERGED") => ItemState::Merged,
                Some("CLOSED") => ItemState::Closed,
                _ => ItemState::Open,
            },
            state_reason: None,
            created_at: string(&node["createdAt"]),
            updated_at: string(&node["updatedAt"]),
            closed_at: node["closedAt"].as_str().map(str::to_owned),
            merged_at: node["mergedAt"].as_str().map(str::to_owned),
            merge_commit: node["mergeCommit"]["oid"].as_str().map(str::to_owned),
            commit_count: node["commits"]["totalCount"].as_u64().map(|n| n as usize),
            commits,
        });
        if self.has(RelationType::Closes) {
            for issue in self.refs(&node["closes"], RelationType::Closes) {
                self.link(
                    RelationType::Closes,
                    this.clone(),
                    issue,
                    "PullRequest.closingIssuesReferences(excludeUserLinked)",
                );
            }
            for issue in self.refs(&node["linked"], RelationType::Linked) {
                self.link(
                    RelationType::Linked,
                    this.clone(),
                    issue,
                    "PullRequest.closingIssuesReferences(userLinkedOnly)",
                );
            }
        }
        self.events(&node["events"], &this);
    }

    /// The links an item's timeline records: cross-references to it,
    /// commits that reference it, and, for an issue, what closed it.
    fn events(&mut self, events: &Value, this: &Ref) {
        for event in events["nodes"].as_array().cloned().unwrap_or_default() {
            match event["__typename"].as_str() {
                Some("CrossReferencedEvent") => {
                    let Some(source) =
                        self.reference(&event["source"], RelationType::CrossReferenced)
                    else {
                        continue;
                    };
                    let link = self.link(
                        RelationType::CrossReferenced,
                        source,
                        this.clone(),
                        "CrossReferencedEvent",
                    );
                    link.at = event["referencedAt"].as_str().map(str::to_owned);
                    link.will_close = event["willCloseTarget"].as_bool();
                }
                Some("ReferencedEvent") => {
                    let Some(oid) = event["commit"]["oid"].as_str() else {
                        *self
                            .not_visible
                            .links
                            .entry(RelationType::Referenced)
                            .or_default() += 1;
                        continue;
                    };
                    let commit =
                        self.commit(oid, event["commitRepository"]["nameWithOwner"].as_str());
                    let link = self.link(
                        RelationType::Referenced,
                        commit,
                        this.clone(),
                        "ReferencedEvent",
                    );
                    link.at = event["createdAt"].as_str().map(str::to_owned);
                }
                Some("ClosedEvent") => {
                    let closer = &event["closer"];
                    let to = match closer["__typename"].as_str() {
                        Some("Commit") => closer["oid"].as_str().map(|oid| {
                            self.commit(oid, closer["repository"]["nameWithOwner"].as_str())
                        }),
                        Some("PullRequest") => self.reference(closer, RelationType::ClosedBy),
                        // closed by hand, or by a project
                        _ => None,
                    };
                    if let Some(to) = to {
                        let link = self.link(
                            RelationType::ClosedBy,
                            this.clone(),
                            to,
                            "ClosedEvent.closer",
                        );
                        link.at = event["createdAt"].as_str().map(str::to_owned);
                    }
                }
                // a node the account cannot see, of any type asked for
                _ => self.not_visible.events += 1,
            }
        }
    }

    fn title(&self, node: &Value) -> Option<String> {
        self.options
            .titles
            .then(|| node["title"].as_str().map(str::to_owned))
            .flatten()
    }
}

/// The nested connections of a page's node, each with its page info.
fn connections(nested: &[Nested], pulls: bool) -> String {
    nested
        .iter()
        .map(|c| {
            let count = match (pulls, c.field) {
                (true, "commits") => " totalCount",
                _ => "",
            };
            format!(
                "{}(first: {}{}) {{{count} pageInfo {{ hasNextPage endCursor }} nodes {{ {} }} }}",
                c.field,
                page_size(c.field),
                c.args,
                c.selection
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn page_size(field: &str) -> usize {
    match field {
        "commits" => COMMITS_PAGE,
        f if f.starts_with("events") => EVENTS_PAGE,
        _ => LINKS_PAGE,
    }
}

/// The cursor of a connection's next page, when it has one.
fn next(connection: &Value) -> Option<String> {
    let info = &connection["pageInfo"];
    match info["hasNextPage"].as_bool() {
        Some(true) => info["endCursor"].as_str().map(str::to_owned),
        _ => None,
    }
}

fn string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}
