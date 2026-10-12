//! Work items and the links a work tracker records between them, as a
//! fetched snapshot holds them: plain data, no I/O. GitHub is the only
//! source today; it is named only in [`Snapshot::source`] and in each
//! relation's [`Relation::observed`].
//!
//! Every relation is observed, never derived: the snapshot keeps what the
//! tracker's API returned, with the field or event it came from. A relation
//! is seen only when the item that records it was fetched, so each type
//! says which ends it is seen from ([`RelationType::seen_from`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The snapshot format's version.
pub const WORK_SCHEMA: u32 = 1;

/// What one fetch read: items updated within a range, and their links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    /// `github`.
    pub source: String,
    pub host: String,
    /// `owner/name`.
    pub repository: String,
    /// When the fetch ran, as the tracker's clock gives times (UTC): the
    /// last one, whole or an update.
    pub fetched_at: String,
    /// When the last whole fetch ran. An update reads only the items
    /// updated since the fetch before it, so what only an item's own
    /// timeline records without changing the item (a cross-reference to
    /// it, a commit that references it) and items deleted or hidden since
    /// are as of this time. `None` in a snapshot written before updates
    /// existed, which was fetched whole at `fetched_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whole_fetched_at: Option<String>,
    /// The account the snapshot was fetched as: an update by another
    /// account fetches whole instead, so one snapshot never holds what two
    /// accounts saw. Never shown in answers; no token is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    pub range: Range,
    /// The relation types fetched; "no link" holds only for these.
    pub relation_types: Vec<RelationType>,
    /// Relation types the host does not offer, so not fetched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<RelationType>,
    /// Titles left out (`--no-titles`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_titles: bool,
    pub items: Vec<Item>,
    pub relations: Vec<Relation>,
    /// What the fetching account could not see (a reference from a private
    /// repository elsewhere, a deleted item): GitHub answers `null` for it.
    #[serde(default, skip_serializing_if = "NotVisible::is_empty")]
    pub not_visible: NotVisible,
}

/// How the fetches of a snapshot have gone, kept apart from it: what an
/// automatic refresh waits on after a failure, and what answers say of the
/// last failed attempt. A success clears the error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshStatus {
    pub schema: u32,
    pub source: String,
    pub host: String,
    /// `owner/name`.
    pub repository: String,
    /// The account the last attempt ran as, when it learned it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    /// When the last attempt began (UTC).
    pub last_attempt_at: String,
    /// Why the last attempt failed; `None` after a success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<RefreshError>,
    /// No automatic attempt before this time (UTC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<String>,
    /// Failed attempts in a row.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub consecutive_failures: usize,
}

/// The status file's format version.
pub const STATUS_SCHEMA: u32 = 1;

/// A failed attempt: what kind of failure, and its message, with no
/// credentials or tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshError {
    pub kind: FailureKind,
    pub message: String,
}

/// What a failed attempt ran into, as far as its message tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The tracker's primary rate limit: points or requests per hour.
    RateLimit,
    /// GitHub's secondary rate limit, against bursts, whose end its
    /// `rate_limit` endpoint does not tell.
    SecondaryRateLimit,
    /// No login, a bad or expired token, or no access.
    Auth,
    /// The network or the tracker's servers.
    Network,
    Other,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::RateLimit => "rate limit",
            FailureKind::SecondaryRateLimit => "secondary rate limit",
            FailureKind::Auth => "authentication",
            FailureKind::Network => "network",
            FailureKind::Other => "error",
        }
    }
}

/// A tracker account: its login and, when the tracker gives it, its
/// numeric id, which stays when the login is renamed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
}

/// Counts of what the tracker answered `null` for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotVisible {
    /// Ends of links, per type.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub links: BTreeMap<RelationType, usize>,
    /// Timeline events of any type asked for.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub events: usize,
    /// Issues and pull requests in a page.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub items: usize,
}

impl NotVisible {
    pub fn is_empty(&self) -> bool {
        self.links.is_empty() && self.events == 0 && self.items == 0
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The items a snapshot holds: those updated since a date, newest first, up
/// to a bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    /// Items updated since this time; `None` for every item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_since: Option<String>,
    /// What set the date.
    pub since_rule: SinceRule,
    /// Items read per kind at most.
    pub bound: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reached_bound: bool,
    /// When the bound was reached: the oldest update read, which the range
    /// really starts at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_since: Option<String>,
    pub issues: usize,
    pub pull_requests: usize,
}

/// What set a range's date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SinceRule {
    /// Given to the fetch.
    Given,
    /// The oldest commit the local history read holds.
    OldestCommitRead,
    /// 365 days before the fetch.
    Days365,
    /// No date: every item.
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Issue,
    PullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Open,
    Closed,
    Merged,
}

/// An issue or a pull request. Issues and pull requests share one sequence
/// of numbers in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    pub number: u64,
    /// The tracker's own id.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: ItemState,
    /// For an issue: `completed`, `not_planned`, `duplicate`, as the tracker
    /// says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
    /// A merged pull request's merge commit: the merge commit of a merge,
    /// the squashed commit of a squash, the last rebased commit of a rebase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
    /// A pull request's commits by SHA, in the tracker's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
    /// How many commits the pull request has; more than `commits` lists
    /// when the tracker stops listing (GitHub at 250).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_count: Option<usize>,
    /// The links and events of this item the account could not see, so an
    /// update that reads it again replaces them; the snapshot's
    /// `not_visible` sums them.
    #[serde(default, skip_serializing_if = "NotVisible::is_empty")]
    pub not_visible: NotVisible,
}

/// A link the tracker records, by the field or event it is observed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    /// A parent issue to a sub-issue.
    SubIssue,
    /// An issue to an issue that blocks it.
    BlockedBy,
    /// A pull request to an issue it closes by the tracker's closing
    /// references (keywords).
    Closes,
    /// A pull request to an issue linked to it by hand.
    Linked,
    /// An issue to the commit or pull request that closed it, at a time.
    ClosedBy,
    /// An item to an item it references, as the referenced item's
    /// timeline records it.
    CrossReferenced,
    /// A commit to an item its message references.
    Referenced,
    /// An issue to the issue it duplicates.
    DuplicateOf,
}

/// The ends a relation type is seen from: a relation is in a snapshot only
/// when an item that records it was fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeenFrom {
    Both,
    /// Only the `from` end records it.
    From,
    /// Only the `to` end records it.
    To,
}

impl RelationType {
    pub fn as_str(self) -> &'static str {
        match self {
            RelationType::SubIssue => "sub_issue",
            RelationType::BlockedBy => "blocked_by",
            RelationType::Closes => "closes",
            RelationType::Linked => "linked",
            RelationType::ClosedBy => "closed_by",
            RelationType::CrossReferenced => "cross_referenced",
            RelationType::Referenced => "referenced",
            RelationType::DuplicateOf => "duplicate_of",
        }
    }

    /// Which ends record it, as GitHub's API offers them.
    pub fn seen_from(self) -> SeenFrom {
        match self {
            RelationType::SubIssue
            | RelationType::BlockedBy
            | RelationType::Closes
            | RelationType::Linked => SeenFrom::Both,
            // the issue's timeline, the duplicate's field
            RelationType::ClosedBy | RelationType::DuplicateOf => SeenFrom::From,
            // the referenced item's timeline
            RelationType::CrossReferenced | RelationType::Referenced => SeenFrom::To,
        }
    }
}

/// One end of a relation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Ref {
    /// An item of the snapshot's repository, by number; it may be outside
    /// the fetched range.
    Item { item: u64 },
    /// An item of another repository.
    Foreign { repository: String, number: u64 },
    /// A commit, of the snapshot's repository unless `repository` says.
    Commit {
        commit: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    #[serde(rename = "type")]
    pub kind: RelationType,
    pub from: Ref,
    pub to: Ref,
    /// The fields or events it was observed in, one per side that saw it.
    pub observed: Vec<String>,
    /// When it happened, for a relation the tracker records as an event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// For a cross-reference: the referencing item will close the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_close: Option<bool>,
}

/// Which end of a relation recorded an observation: the item that was read
/// when the field or event named `observed` gave it. `None` for a name no
/// fetch writes.
pub fn recorded_by(observed: &str) -> Option<RecordedBy> {
    let end = match observed {
        "Issue.subIssues" | "Issue.blockedBy" | "Issue.duplicateOf" | "ClosedEvent.closer" => {
            RecordedBy::From
        }
        "Issue.parent" | "Issue.blocking" | "CrossReferencedEvent" | "ReferencedEvent" => {
            RecordedBy::To
        }
        o if o.starts_with("PullRequest.closingIssuesReferences") => RecordedBy::From,
        o if o.starts_with("Issue.closedByPullRequestsReferences") => RecordedBy::To,
        _ => return None,
    };
    Some(end)
}

/// The end of a relation an observation was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedBy {
    From,
    To,
}

impl Snapshot {
    /// Merge `update`, a read of the items updated since this snapshot's
    /// fetch, into it: each item read replaces the one held, with what it
    /// records; a link of a type both ends record goes when either end was
    /// read (the update gives it again if that end still records it), and
    /// the observations the other items recorded of the other types stay. The range,
    /// the whole fetch's time and what only the whole fetch could say stay
    /// this snapshot's; the counts of what the account could not see are
    /// summed again from the items, apart from items no page could show,
    /// where the larger count stays.
    pub fn merge(&mut self, update: Snapshot) {
        let read: std::collections::BTreeSet<u64> = update.items.iter().map(|i| i.number).collect();
        self.items.retain(|i| !read.contains(&i.number));
        self.items.extend(update.items);
        let recorded_by_read = |relation: &Relation, observed: &str| {
            let end = match recorded_by(observed) {
                Some(RecordedBy::From) => &relation.from,
                Some(RecordedBy::To) => &relation.to,
                None => return false,
            };
            matches!(end, Ref::Item { item } if read.contains(item))
        };
        let read_end = |end: &Ref| matches!(end, Ref::Item { item } if read.contains(item));
        let mut relations = Vec::with_capacity(self.relations.len() + update.relations.len());
        for mut relation in std::mem::take(&mut self.relations) {
            // a type both ends record: the end read answers for it as of the
            // update, and gives it again if it still holds
            if relation.kind.seen_from() == SeenFrom::Both
                && (read_end(&relation.from) || read_end(&relation.to))
            {
                continue;
            }
            let observed = std::mem::take(&mut relation.observed);
            relation.observed = observed
                .into_iter()
                .filter(|o| !recorded_by_read(&relation, o))
                .collect();
            if !relation.observed.is_empty() {
                relations.push(relation);
            }
        }
        relations.extend(update.relations);
        self.relations = relations;
        self.fetched_at = update.fetched_at;
        self.repository = update.repository;
        let walked = self.not_visible.items.max(update.not_visible.items);
        self.normalize();
        self.not_visible = self.items_not_visible();
        self.not_visible.items = walked;
        self.range.issues = self.count(ItemKind::Issue);
        self.range.pull_requests = self.count(ItemKind::PullRequest);
    }

    /// The links and events of its items the account could not see, summed.
    pub fn items_not_visible(&self) -> NotVisible {
        let mut sum = NotVisible::default();
        for item in &self.items {
            for (kind, n) in &item.not_visible.links {
                *sum.links.entry(*kind).or_default() += n;
            }
            sum.events += item.not_visible.events;
        }
        sum
    }

    /// The items of `kind` held.
    pub fn count(&self, kind: ItemKind) -> usize {
        self.items.iter().filter(|i| i.kind == kind).count()
    }

    /// Sort items and relations and merge a relation seen from two sides
    /// into one, so the same answers give the same snapshot. An item read
    /// twice (again, after it changed during the fetch) keeps its last
    /// read.
    pub fn normalize(&mut self) {
        self.items.reverse();
        self.items.sort_by_key(|i| i.number);
        self.items.dedup_by_key(|i| i.number);
        self.relation_types.sort();
        self.relation_types.dedup();
        self.unavailable.sort();
        self.unavailable.dedup();
        self.relations
            .sort_by(|a, b| (a.kind, &a.from, &a.to, &a.at).cmp(&(b.kind, &b.from, &b.to, &b.at)));
        let mut merged: Vec<Relation> = Vec::with_capacity(self.relations.len());
        for relation in self.relations.drain(..) {
            match merged.last_mut() {
                Some(last)
                    if (last.kind, &last.from, &last.to, &last.at)
                        == (relation.kind, &relation.from, &relation.to, &relation.at) =>
                {
                    last.observed.extend(relation.observed);
                    last.will_close = last.will_close.or(relation.will_close);
                }
                _ => merged.push(relation),
            }
        }
        for relation in &mut merged {
            relation.observed.sort();
            relation.observed.dedup();
        }
        self.relations = merged;
    }

    /// The item numbered `number`, when the snapshot holds it.
    pub fn item(&self, number: u64) -> Option<&Item> {
        self.items
            .binary_search_by_key(&number, |i| i.number)
            .ok()
            .map(|i| &self.items[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relation(kind: RelationType, from: u64, to: u64, observed: &str) -> Relation {
        Relation {
            kind,
            from: Ref::Item { item: from },
            to: Ref::Item { item: to },
            observed: vec![observed.into()],
            at: None,
            will_close: None,
        }
    }

    fn snapshot(relations: Vec<Relation>) -> Snapshot {
        Snapshot {
            schema: WORK_SCHEMA,
            source: "github".into(),
            host: "github.com".into(),
            repository: "acme/shop".into(),
            fetched_at: "2026-10-03T00:00:00Z".into(),
            whole_fetched_at: Some("2026-10-03T00:00:00Z".into()),
            account: None,
            range: Range {
                updated_since: None,
                since_rule: SinceRule::All,
                bound: 5000,
                reached_bound: false,
                effective_since: None,
                issues: 0,
                pull_requests: 0,
            },
            relation_types: vec![RelationType::Closes],
            unavailable: Vec::new(),
            no_titles: false,
            items: Vec::new(),
            relations,
            not_visible: NotVisible::default(),
        }
    }

    #[test]
    fn a_relation_seen_from_both_ends_is_one_with_both_observations() {
        let mut s = snapshot(vec![
            relation(
                RelationType::Closes,
                15,
                12,
                "Issue.closedByPullRequestsReferences",
            ),
            relation(
                RelationType::Closes,
                15,
                12,
                "PullRequest.closingIssuesReferences",
            ),
            relation(
                RelationType::Linked,
                15,
                12,
                "PullRequest.closingIssuesReferences",
            ),
        ]);
        s.normalize();
        assert_eq!(s.relations.len(), 2);
        assert_eq!(
            s.relations[0].observed,
            [
                "Issue.closedByPullRequestsReferences",
                "PullRequest.closingIssuesReferences"
            ]
        );
    }

    #[test]
    fn ends_serialize_by_their_fields() {
        let ends = [
            Ref::Item { item: 3 },
            Ref::Foreign {
                repository: "acme/web".into(),
                number: 4,
            },
            Ref::Commit {
                commit: "a1".into(),
                repository: None,
            },
        ];
        let json = serde_json::to_string(&ends).unwrap();
        assert_eq!(
            json,
            r#"[{"item":3},{"repository":"acme/web","number":4},{"commit":"a1"}]"#
        );
        let back: Vec<Ref> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ends);
    }

    #[test]
    fn an_item_read_again_keeps_its_last_read() {
        let item = |state: ItemState| Item {
            kind: ItemKind::Issue,
            number: 7,
            id: "I_7".into(),
            title: None,
            state,
            state_reason: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            closed_at: None,
            merged_at: None,
            merge_commit: None,
            commits: Vec::new(),
            commit_count: None,
            not_visible: NotVisible::default(),
        };
        let mut s = snapshot(Vec::new());
        s.items = vec![item(ItemState::Open), item(ItemState::Closed)];
        s.normalize();
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].state, ItemState::Closed);
    }

    #[test]
    fn an_update_replaces_what_the_items_it_read_record_and_keeps_the_rest() {
        let issue = |number: u64, events: usize| Item {
            kind: ItemKind::Issue,
            number,
            id: format!("I_{number}"),
            title: None,
            state: ItemState::Open,
            state_reason: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            closed_at: None,
            merged_at: None,
            merge_commit: None,
            commits: Vec::new(),
            commit_count: None,
            not_visible: NotVisible {
                events,
                ..NotVisible::default()
            },
        };
        let issue_side = "Issue.closedByPullRequestsReferences(excludeUserLinked)";
        let pull_side = "PullRequest.closingIssuesReferences(excludeUserLinked)";
        // #1 is closed by #5, which both record; #9 cross-references #1 and #2
        let mut before = snapshot(vec![
            relation(RelationType::Closes, 5, 1, issue_side),
            relation(RelationType::Closes, 5, 1, pull_side),
            relation(RelationType::CrossReferenced, 9, 1, "CrossReferencedEvent"),
            relation(RelationType::CrossReferenced, 9, 2, "CrossReferencedEvent"),
        ]);
        before.items = vec![issue(1, 2), issue(2, 1)];
        before.normalize();
        before.not_visible = before.items_not_visible();
        // #1 read again: still closed by #5, cross-referenced by #7 now
        let mut update = snapshot(vec![
            relation(RelationType::Closes, 5, 1, issue_side),
            relation(RelationType::CrossReferenced, 7, 1, "CrossReferencedEvent"),
        ]);
        update.fetched_at = "2026-10-04T00:00:00Z".into();
        update.whole_fetched_at = None;
        update.items = vec![issue(1, 0)];
        let mut after = before.clone();
        after.merge(update);
        let links: Vec<(RelationType, Ref, Ref, Vec<String>)> = after
            .relations
            .iter()
            .map(|r| (r.kind, r.from.clone(), r.to.clone(), r.observed.clone()))
            .collect();
        let item = |n| Ref::Item { item: n };
        assert_eq!(
            links,
            [
                // both ends record it: #1, read, gives it again
                (
                    RelationType::Closes,
                    item(5),
                    item(1),
                    vec![issue_side.to_owned()]
                ),
                (
                    RelationType::CrossReferenced,
                    item(7),
                    item(1),
                    vec!["CrossReferencedEvent".to_owned()]
                ),
                (
                    RelationType::CrossReferenced,
                    item(9),
                    item(2),
                    vec!["CrossReferencedEvent".to_owned()]
                ),
            ]
        );
        assert_eq!(after.not_visible.events, 1);
        assert_eq!(after.fetched_at, "2026-10-04T00:00:00Z");
        assert_eq!(
            after.whole_fetched_at.as_deref(),
            Some("2026-10-03T00:00:00Z")
        );
        assert_eq!(after.range.issues, 2);
        // merging the same update again gives the same snapshot
        let mut again = after.clone();
        let mut same = snapshot(vec![
            relation(RelationType::Closes, 5, 1, issue_side),
            relation(RelationType::CrossReferenced, 7, 1, "CrossReferencedEvent"),
        ]);
        same.fetched_at = "2026-10-04T00:00:00Z".into();
        same.items = vec![issue(1, 0)];
        again.merge(same);
        assert_eq!(again, after);
    }

    #[test]
    fn a_link_both_ends_record_goes_when_the_end_read_no_longer_records_it() {
        let item = |kind: ItemKind, number: u64| Item {
            kind,
            number,
            id: format!("X_{number}"),
            title: None,
            state: ItemState::Open,
            state_reason: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            closed_at: None,
            merged_at: None,
            merge_commit: None,
            commits: Vec::new(),
            commit_count: None,
            not_visible: NotVisible::default(),
        };
        let mut before = snapshot(vec![
            relation(
                RelationType::Closes,
                5,
                1,
                "Issue.closedByPullRequestsReferences(excludeUserLinked)",
            ),
            relation(
                RelationType::Closes,
                5,
                1,
                "PullRequest.closingIssuesReferences(excludeUserLinked)",
            ),
        ]);
        before.items = vec![item(ItemKind::Issue, 1), item(ItemKind::PullRequest, 5)];
        before.normalize();
        // #5 no longer closes #1; #1 did not change, so it was not read
        let mut update = snapshot(Vec::new());
        update.items = vec![item(ItemKind::PullRequest, 5)];
        before.merge(update);
        assert!(before.relations.is_empty(), "{:?}", before.relations);
    }
}
