//! `snapshot.rs` 的**纯函数**用例：解析、归一化映射表、三态裁决。
//!
//! **零 I/O、零网络**。跑真 wire 的那几条（`fetch_pr_snapshot` 的分页与三道守卫、
//! head 变更、页数上限、`Retry-After`）与**离线 GraphQL 替身**一起住在
//! `ghsnapshot/refresh/tests.rs` —— 那里的替身同时服务管道用例，所以替身只写一份。

use serde_json::{json, Value};

use super::*;

fn pr_payload(head: &str, rollup: &Value) -> Value {
    json!({
        "repository": {
            "pullRequest": {
                "headRefOid": head,
                "mergeable": "MERGEABLE",
                "mergeStateStatus": "CLEAN",
                "commits": {"nodes": [{"commit": {"statusCheckRollup": rollup}}]},
            }
        }
    })
}

fn rollup_payload(state: &str, nodes: &Value, has_next: bool, cursor: &str) -> Value {
    json!({
        "state": state,
        "contexts": {
            "pageInfo": {"hasNextPage": has_next, "endCursor": cursor},
            "nodes": nodes,
        }
    })
}

/// `DoD` 的「三态」第 1/2 态：`decided` 的逐条映射表（上游 `TestSnapshotDecided` 六例逐字）。
#[test]
fn decided_mapping_table_matches_upstream() {
    let completed = SnapshotCheck {
        name: "ci".into(),
        status: "completed".into(),
        conclusion: Some("success".into()),
        details_url: None,
        is_status_context: false,
    };
    let running = SnapshotCheck {
        status: "in_progress".into(),
        ..completed.clone()
    };
    let cases: [(&str, PrSnapshot, bool); 6] = [
        (
            "clean passed",
            PrSnapshot {
                mergeable: Some("MERGEABLE".into()),
                has_checks: true,
                rollup_state: Some("SUCCESS".into()),
                checks: vec![completed.clone()],
                ..PrSnapshot::default()
            },
            true,
        ),
        (
            "conflicting decided",
            PrSnapshot {
                mergeable: Some("CONFLICTING".into()),
                ..PrSnapshot::default()
            },
            true,
        ),
        (
            "mergeable unknown",
            PrSnapshot {
                mergeable: Some("UNKNOWN".into()),
                has_checks: true,
                rollup_state: Some("SUCCESS".into()),
                ..PrSnapshot::default()
            },
            false,
        ),
        (
            "rollup pending",
            PrSnapshot {
                mergeable: Some("MERGEABLE".into()),
                has_checks: true,
                rollup_state: Some("PENDING".into()),
                ..PrSnapshot::default()
            },
            false,
        ),
        (
            "running context",
            PrSnapshot {
                mergeable: Some("MERGEABLE".into()),
                has_checks: true,
                rollup_state: Some("SUCCESS".into()),
                checks: vec![running],
                ..PrSnapshot::default()
            },
            false,
        ),
        (
            "no checks but mergeable",
            PrSnapshot {
                mergeable: Some("MERGEABLE".into()),
                ..PrSnapshot::default()
            },
            true,
        ),
    ];
    for (name, snapshot, want) in cases {
        assert_eq!(snapshot.decided(), want, "case {name}");
    }
}

/// 三态第 3 态：`statusCheckRollup == null` ⇒ `has_checks == false`，**绝不**当作通过；
/// 且 `mergeable == UNKNOWN` 时仍未决（上游 `TestFetchPRSnapshotNullRollup` 的载荷逐字）。
#[test]
fn null_rollup_means_no_checks_and_never_passed() {
    let payload = json!({
        "repository": {"pullRequest": {
            "headRefOid": "sha9",
            "mergeable": "UNKNOWN",
            "mergeStateStatus": "UNKNOWN",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]},
        }}
    });
    let snapshot = parse_pr_snapshot(&payload).unwrap();
    assert!(!snapshot.has_checks);
    assert_eq!(snapshot.rollup_state, None);
    assert!(snapshot.checks.is_empty());
    assert!(!snapshot.decided());
    assert_eq!(snapshot.pending_reason(), Some("mergeability_unknown"));
}

/// 归一化映射表逐条：`normalize_run_status` 的三值（含 `default` 兜住的四种「还在跑」）。
#[test]
fn normalize_run_status_table() {
    assert_eq!(normalize_run_status("COMPLETED"), "completed");
    assert_eq!(normalize_run_status("completed"), "completed");
    assert_eq!(normalize_run_status("IN_PROGRESS"), "in_progress");
    for still_running in ["QUEUED", "WAITING", "PENDING", "REQUESTED", "", "weird"] {
        assert_eq!(
            normalize_run_status(still_running),
            "queued",
            "{still_running}"
        );
    }
}

/// 归一化映射表逐条：`normalize_status_state` 的五值（`EXPECTED` 与未知同判）。
#[test]
fn normalize_status_state_table() {
    assert_eq!(normalize_status_state("SUCCESS"), ("completed", "success"));
    assert_eq!(normalize_status_state("FAILURE"), ("completed", "failure"));
    assert_eq!(normalize_status_state("ERROR"), ("completed", "error"));
    assert_eq!(normalize_status_state("PENDING"), ("in_progress", ""));
    assert_eq!(normalize_status_state("EXPECTED"), ("queued", ""));
    assert_eq!(normalize_status_state("who-knows"), ("queued", ""));
}

/// `normalize_node`：两种联合体成员折成同一形状；未知 `__typename` 与非对象节点被跳过。
#[test]
fn normalize_node_flattens_both_union_members() {
    let run = normalize_node(&json!({
        "__typename": "CheckRun", "name": "backend", "status": "COMPLETED",
        "conclusion": "FAILURE", "detailsUrl": "u1"
    }))
    .unwrap();
    assert_eq!(run.name, "backend");
    assert_eq!(run.status, "completed");
    assert_eq!(run.conclusion.as_deref(), Some("failure"));
    assert_eq!(run.details_url.as_deref(), Some("u1"));
    assert!(!run.is_status_context);

    let status = normalize_node(&json!({
        "__typename": "StatusContext", "context": "vercel", "state": "SUCCESS",
        "targetUrl": "u4"
    }))
    .unwrap();
    assert_eq!(status.name, "vercel");
    assert_eq!(status.status, "completed");
    assert_eq!(status.conclusion.as_deref(), Some("success"));
    assert!(status.is_status_context);

    // `conclusion: null`（还在跑）⇒ `None`；空 URL ⇒ `None`。
    let running = normalize_node(&json!({
        "__typename": "CheckRun", "name": "e2e", "status": "IN_PROGRESS",
        "conclusion": null, "detailsUrl": ""
    }))
    .unwrap();
    assert_eq!(running.conclusion, None);
    assert_eq!(running.details_url, None);

    assert!(normalize_node(&json!({"__typename": "SomethingElse"})).is_none());
    assert!(normalize_node(&json!({"no_typename": true})).is_none());
    assert!(normalize_node(&json!("not-an-object")).is_none());
}

/// 单页解析：`mergeable` / `mergeStateStatus` / `rollup.state` 逐字保留（**不**小写化 ——
/// 小写化是响应层 `lowerTextPtr` 的事）；`pageInfo` 原样交给分页循环。
#[test]
fn first_page_keeps_raw_enums_and_page_info() {
    let payload = pr_payload(
        "sha1",
        &rollup_payload(
            "FAILURE",
            &json!([
                {"__typename": "CheckRun", "name": "a", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "name": "b", "status": "QUEUED", "conclusion": null},
                {"__typename": "Ignored", "name": "c"},
            ]),
            true,
            "CUR2",
        ),
    );
    let (snapshot, page) = parse_pr_snapshot_page(&payload).unwrap();
    assert_eq!(snapshot.head_sha, "sha1");
    assert_eq!(snapshot.mergeable.as_deref(), Some("MERGEABLE"));
    assert_eq!(snapshot.merge_state_status.as_deref(), Some("CLEAN"));
    assert_eq!(snapshot.rollup_state.as_deref(), Some("FAILURE"));
    assert!(snapshot.has_checks);
    assert_eq!(snapshot.checks.len(), 2, "未知 __typename 只被跳过");
    assert_eq!(snapshot.checks[0].status, "completed");
    assert_eq!(snapshot.checks[1].status, "queued");
    assert!(page.has_next_page);
    assert_eq!(page.end_cursor.as_deref(), Some("CUR2"));
    assert!(!snapshot.decided());
    assert_eq!(snapshot.pending_reason(), Some("checks_running"));
}

/// 缺失的 `pullRequest` / 非对象载荷 ⇒ `Malformed`，且**不回显**载荷内容。
#[test]
fn malformed_payloads_are_rejected_without_echoing_body() {
    let missing = json!({"repository": {"pullRequest": null}});
    let error = parse_pr_snapshot(&missing).unwrap_err();
    assert!(
        error.to_string().contains("pull request not found"),
        "{error}"
    );

    let not_json = json!("secret-payload-do-not-echo");
    let error = parse_pr_snapshot(&not_json).unwrap_err();
    assert!(
        error.to_string().contains("malformed pull request data"),
        "{error}"
    );
    assert!(!error.to_string().contains("secret-payload"));
}

/// 空串与缺失字段在 wire 结构里都按「无值」处理（`Option` 全套默认 ⇒ 落库写 `NULL`）。
#[test]
fn wire_structs_default_every_missing_field() {
    let payload = json!({"repository": {"pullRequest": {}}});
    let snapshot = parse_pr_snapshot(&payload).unwrap();
    assert_eq!(snapshot.head_sha, "");
    assert_eq!(snapshot.mergeable, None);
    assert!(!snapshot.has_checks);
    assert!(!snapshot.decided());
    assert_eq!(snapshot.pending_reason(), Some("mergeability_unknown"));

    let empty_strings = json!({"repository": {"pullRequest": {
        "headRefOid": "", "mergeable": "", "mergeStateStatus": "CLEAN"
    }}});
    let snapshot = parse_pr_snapshot(&empty_strings).unwrap();
    assert_eq!(snapshot.mergeable, None, "空串在落库口径里是 NULL");
    assert_eq!(snapshot.merge_state_status.as_deref(), Some("CLEAN"));
}

/// `rollup` **只看 `commits.nodes[0]`**（上游 `rollup()` 逐字：`Nodes[0]`，不是最后一个；
/// 查询是 `commits(last:1)` ⇒ 正常只有一项）。多节点时其余一概忽略。
#[test]
fn rollup_only_reads_the_first_commit_node() {
    let payload = json!({
        "repository": {"pullRequest": {
            "headRefOid": "sha1",
            "mergeable": "MERGEABLE",
            "commits": {"nodes": [
                {"commit": {"statusCheckRollup": {"state": "FAILURE", "contexts": {
                    "pageInfo": {"hasNextPage": false, "endCursor": ""},
                    "nodes": [{"__typename": "CheckRun", "name": "old", "status": "COMPLETED"}]}}}},
                {"commit": {"statusCheckRollup": {"state": "SUCCESS", "contexts": {
                    "pageInfo": {"hasNextPage": false, "endCursor": ""},
                    "nodes": [{"__typename": "CheckRun", "name": "new", "status": "COMPLETED"}]}}}}
            ]}
        }}
    });
    let snapshot = parse_pr_snapshot(&payload).unwrap();
    assert_eq!(snapshot.rollup_state.as_deref(), Some("FAILURE"));
    assert_eq!(snapshot.checks.len(), 1);
    assert_eq!(snapshot.checks[0].name, "old");
    assert!(snapshot.decided(), "FAILURE 是终态 rollup ⇒ 已决");
}
