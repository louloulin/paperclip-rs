#![forbid(unsafe_code)]

//! Issue reference pure helpers \u2014 1:1 port of paperclip/server/src/services/issue-references.ts
//!
//! R724: zero-DB helpers for source sorting, related-work sorting, and summary
//! diffing.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

/// Source-kind display order (Node SOURCE_KIND_ORDER).
pub const SOURCE_KIND_ORDER: &[&str] = &["title", "description", "comment", "document"];

/// Source kind precedence weight (lower = earlier).
pub fn source_kind_rank(kind: &str) -> i32 {
    match kind {
        "title" => 0,
        "description" => 1,
        "comment" => 2,
        "document" => 3,
        _ => 99,
    }
}

/// Sort two IssueReferenceSource records.
///
/// Node parity: sortSources \u2014 first by source kind order, then by label,
/// finally by source record id.
pub fn sort_sources(a: &Value, b: &Value) -> std::cmp::Ordering {
    let rank_a = source_kind_rank(a.get("kind").and_then(Value::as_str).unwrap_or(""));
    let rank_b = source_kind_rank(b.get("kind").and_then(Value::as_str).unwrap_or(""));
    if rank_a != rank_b { return rank_a.cmp(&rank_b); }
    let la = a.get("label").and_then(Value::as_str).unwrap_or("").to_string();
    let lb = b.get("label").and_then(Value::as_str).unwrap_or("").to_string();
    let lcmp = la.cmp(&lb);
    if lcmp != std::cmp::Ordering::Equal { return lcmp; }
    let ra = a.get("sourceRecordId").and_then(Value::as_str).unwrap_or("").to_string();
    let rb = b.get("sourceRecordId").and_then(Value::as_str).unwrap_or("").to_string();
    ra.cmp(&rb)
}

/// Sort two IssueRelatedWorkItem records.
///
/// Node parity: sortRelatedWork \u2014 first by mention count descending, then by
/// identifier/title ascending.
pub fn sort_related_work(a: &Value, b: &Value) -> std::cmp::Ordering {
    let ma = a.get("mentionCount").and_then(Value::as_i64).unwrap_or(0);
    let mb = b.get("mentionCount").and_then(Value::as_i64).unwrap_or(0);
    if ma != mb { return mb.cmp(&ma); }
    let issue_a = a.get("issue");
    let issue_b = b.get("issue");
    let la = issue_a.and_then(|i| i.get("identifier").and_then(Value::as_str))
        .or_else(|| issue_a.and_then(|i| i.get("title").and_then(Value::as_str)))
        .unwrap_or("").to_string();
    let lb = issue_b.and_then(|i| i.get("identifier").and_then(Value::as_str))
        .or_else(|| issue_b.and_then(|i| i.get("title").and_then(Value::as_str)))
        .unwrap_or("").to_string();
    la.cmp(&lb)
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IssueRelatedWorkSummary {
    pub outbound: Vec<Value>,
    pub inbound: Vec<Value>,
}

/// Construct an empty IssueRelatedWorkSummary.
pub fn empty_summary() -> IssueRelatedWorkSummary {
    IssueRelatedWorkSummary { outbound: vec![], inbound: vec![] }
}

/// Diff two IssueRelatedWorkSummary states.
///
/// Node parity: diffIssueSummaries \u2014 returns added/removed/current by id.
pub fn diff_issue_summaries(
    before: &IssueRelatedWorkSummary,
    after: &IssueRelatedWorkSummary,
) -> IssueSummaryDiff {
    let before_by_id = issue_id_map(&before.outbound);
    let after_by_id = issue_id_map(&after.outbound);
    let added = after.outbound.iter()
        .filter(|i| i.get("issue").and_then(|x| x.get("id")).and_then(Value::as_str)
            .map(|id| !before_by_id.contains_key(id)).unwrap_or(false))
        .filter_map(|i| i.get("issue").cloned())
        .collect();
    let removed = before.outbound.iter()
        .filter(|i| i.get("issue").and_then(|x| x.get("id")).and_then(Value::as_str)
            .map(|id| !after_by_id.contains_key(id)).unwrap_or(false))
        .filter_map(|i| i.get("issue").cloned())
        .collect();
    let current = after.outbound.iter().filter_map(|i| i.get("issue").cloned()).collect();
    IssueSummaryDiff { added, removed, current }
}

fn issue_id_map(items: &[Value]) -> BTreeMap<String, ()> {
    let mut m = BTreeMap::new();
    for i in items {
        if let Some(id) = i.get("issue").and_then(|x| x.get("id")).and_then(Value::as_str) {
            m.insert(id.to_string(), ());
        }
    }
    m
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IssueSummaryDiff {
    pub added: Vec<Value>,
    pub removed: Vec<Value>,
    pub current: Vec<Value>,
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_kind_rank_orders() {
        assert!(source_kind_rank("title") < source_kind_rank("description"));
        assert!(source_kind_rank("description") < source_kind_rank("comment"));
        assert!(source_kind_rank("comment") < source_kind_rank("document"));
        assert_eq!(source_kind_rank("unknown"), 99);
    }

    #[test]
    fn sort_sources_by_kind_then_label() {
        let a = json!({"kind": "title", "label": "Main"});
        let b = json!({"kind": "comment", "label": "A"});
        assert_eq!(sort_sources(&a, &b), std::cmp::Ordering::Less);
    }

    #[test]
    fn sort_sources_same_kind_falls_through_to_label() {
        let a = json!({"kind": "title", "label": "Alpha"});
        let b = json!({"kind": "title", "label": "Beta"});
        assert_eq!(sort_sources(&a, &b), std::cmp::Ordering::Less);
    }

    #[test]
    fn sort_sources_same_kind_and_label_falls_through_to_source_record_id() {
        let a = json!({"kind": "title", "label": "X", "sourceRecordId": "a-1"});
        let b = json!({"kind": "title", "label": "X", "sourceRecordId": "b-2"});
        assert_eq!(sort_sources(&a, &b), std::cmp::Ordering::Less);
    }

    #[test]
    fn sort_related_work_by_mention_count_desc() {
        let a = json!({"mentionCount": 5, "issue": {"identifier": "X-1", "title": "X"}});
        let b = json!({"mentionCount": 10, "issue": {"identifier": "Y-1", "title": "Y"}});
        assert_eq!(sort_related_work(&a, &b), std::cmp::Ordering::Greater);
    }

    #[test]
    fn sort_related_work_same_count_by_identifier() {
        let a = json!({"mentionCount": 1, "issue": {"identifier": "X-1", "title": "X title"}});
        let b = json!({"mentionCount": 1, "issue": {"identifier": "Y-1", "title": "Y title"}});
        assert_eq!(sort_related_work(&a, &b), std::cmp::Ordering::Less);
    }

    #[test]
    fn sort_related_work_falls_back_to_title() {
        let a = json!({"mentionCount": 1, "issue": {"title": "Alpha"}});
        let b = json!({"mentionCount": 1, "issue": {"title": "Beta"}});
        assert_eq!(sort_related_work(&a, &b), std::cmp::Ordering::Less);
    }

    #[test]
    fn empty_summary_has_no_outbound_or_inbound() {
        let s = empty_summary();
        assert!(s.outbound.is_empty());
        assert!(s.inbound.is_empty());
    }

    #[test]
    fn diff_detects_added_removed_current() {
        let before = IssueRelatedWorkSummary {
            outbound: vec![json!({"issue": {"id": "i1", "title": "kept"}}), json!({"issue": {"id": "i2", "title": "removed"}})],
            inbound: vec![],
        };
        let after = IssueRelatedWorkSummary {
            outbound: vec![json!({"issue": {"id": "i1", "title": "kept"}}), json!({"issue": {"id": "i3", "title": "added"}})],
            inbound: vec![],
        };
        let diff = diff_issue_summaries(&before, &after);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].get("id").unwrap(), "i3");
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].get("id").unwrap(), "i2");
        assert_eq!(diff.current.len(), 2);
    }
}
