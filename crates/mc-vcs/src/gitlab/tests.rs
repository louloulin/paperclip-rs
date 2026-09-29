//! `GitLabProvider` 的单元用例（从 `gitlab.rs` 原样搬出，**0 断言改动**）。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 = `docs/32`
//! §30 的 **D10**。

use super::time::{civil_from_days, days_from_civil};
use super::*;
use http::HeaderValue;

const SECRET: &str = "gl-webhook-token";

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    map
}

/// 事件分类：两个建模事件 + 未建模。
#[test]
fn event_kind_reads_gitlab_event_header() {
    let p = GitLabProvider;
    assert_eq!(
        p.event_kind(&headers(&[("X-Gitlab-Event", "Merge Request Hook")])),
        EventKind::PullRequest
    );
    assert_eq!(
        p.event_kind(&headers(&[("X-Gitlab-Event", "Pipeline Hook")])),
        EventKind::CIStatus
    );
    assert_eq!(
        p.event_kind(&headers(&[("X-Gitlab-Event", "Push Hook")])),
        EventKind::Other
    );
    assert_eq!(p.event_kind(&HeaderMap::new()), EventKind::Other);
}

/// **明文 token 比较：正例 + 反例**（GitLab 方案）。
#[test]
fn plaintext_token_accepts_correct_and_rejects_others() {
    let p = GitLabProvider;
    // 正例。
    assert!(p.verify_signature(SECRET, &headers(&[("X-Gitlab-Token", SECRET)]), b"{}"));

    // 反例：差 1 位字符、长一截、空 token、缺头、空 secret。
    let mut flipped = SECRET.to_string();
    flipped.push('x');
    assert!(!p.verify_signature(SECRET, &headers(&[("X-Gitlab-Token", &flipped)]), b"{}"));
    assert!(!p.verify_signature(
        SECRET,
        &headers(&[("X-Gitlab-Token", "gl-webhook-toke")]),
        b"{}"
    ));
    assert!(!p.verify_signature(SECRET, &headers(&[("X-Gitlab-Token", "")]), b"{}"));
    assert!(!p.verify_signature(SECRET, &HeaderMap::new(), b"{}"));
    assert!(!p.verify_signature("", &headers(&[("X-Gitlab-Token", SECRET)]), b"{}"));
}

/// MR 载荷：owner/name 拆子组、draft 三态来源、state 归一化、时间戳归一到 RFC3339。
#[test]
fn parse_merge_request_maps_gitlab_shape() {
    let p = GitLabProvider;
    let body = br#"{
      "object_kind": "merge_request",
      "user": { "username": "author", "avatar_url": "https://a.test/x.png" },
      "project": { "path_with_namespace": "group/subgroup/repo" },
      "object_attributes": {
        "iid": 12, "title": "Draft: MUL-2", "description": "body",
        "state": "opened", "action": "open",
        "source_branch": "feat/y", "url": "https://gl.test/group/subgroup/repo/-/merge_requests/12",
        "created_at": "2017-09-20 08:31:45 UTC", "updated_at": "2017-09-20 08:32:45 UTC",
        "last_commit": { "id": "cafebabe" }
      }
    }"#;
    let event = p.parse_pull_request(body).expect("parse");
    assert_eq!(event.action, "open");
    assert_eq!(event.repo_owner, "group/subgroup");
    assert_eq!(event.repo_name, "repo");
    assert_eq!(event.number, 12);
    // 标题前缀 `Draft:` 也算草稿（上游的第三段判据）。
    assert_eq!(event.state, "draft");
    assert_eq!(event.branch.as_deref(), Some("feat/y"));
    assert_eq!(event.head_sha, "cafebabe");
    assert_eq!(event.author_login.as_deref(), Some("author"));
    // 时间戳已被 provider 归一化成 RFC3339（不是 GitLab 方言）。
    assert_eq!(event.created_at.as_deref(), Some("2017-09-20T08:31:45Z"));
    assert_eq!(event.updated_at.as_deref(), Some("2017-09-20T08:32:45Z"));
    assert!(!event.is_terminal());

    // `work_in_progress` 是第二段草稿判据；`locked` 读作 open。
    let wip = br#"{"object_attributes":{"state":"locked","work_in_progress":true}}"#;
    let event = p.parse_pull_request(wip).expect("parse");
    assert_eq!(event.state, "draft");
    let open_locked = br#"{"object_attributes":{"state":"locked"}}"#;
    assert_eq!(
        p.parse_pull_request(open_locked).expect("parse").state,
        "open"
    );
}

/// MR 终态：`merged` / `closed` 与 `action` 的终态集合。
#[test]
fn parse_merge_request_normalizes_terminal_states() {
    let p = GitLabProvider;
    let merged = br#"{"object_attributes":{"state":"merged","action":"merge"}}"#;
    let event = p.parse_pull_request(merged).expect("parse");
    assert_eq!(event.state, "merged");
    assert!(event.is_terminal());

    let closed = br#"{"object_attributes":{"state":"closed","action":"close"}}"#;
    let event = p.parse_pull_request(closed).expect("parse");
    assert_eq!(event.state, "closed");
    assert!(event.is_terminal());
}

/// pipeline 载荷：合成 context、状态三态、`finished_at` → RFC3339。
#[test]
fn parse_pipeline_normalizes_state_and_context() {
    let p = GitLabProvider;
    let body = br#"{
      "object_kind": "pipeline",
      "object_attributes": {
        "sha": "abc123", "status": "failed", "url": "https://gl.test/p/1",
        "created_at": "2026-09-01 00:00:00 UTC", "finished_at": "2026-09-01 00:05:00 UTC"
      }
    }"#;
    let event = p.parse_ci_status(body).expect("parse");
    assert_eq!(event.sha, "abc123");
    assert_eq!(event.context, "gitlab/pipeline");
    assert_eq!(event.state, "failed");
    assert_eq!(event.target_url.as_deref(), Some("https://gl.test/p/1"));
    assert_eq!(event.updated_at.as_deref(), Some("2026-09-01T00:05:00Z"));

    for (wire, normalized) in [
        ("success", "passed"),
        ("skipped", "passed"),
        ("failed", "failed"),
        ("canceled", "failed"),
        ("running", "pending"),
        ("manual", "pending"),
        ("wat", "pending"),
    ] {
        let body = format!(
            r#"{{"object_attributes":{{"sha":"s","status":"{wire}","created_at":"2026-09-01 00:00:00 UTC"}}}}"#
        );
        assert_eq!(
            p.parse_ci_status(body.as_bytes()).expect("parse").state,
            normalized,
            "wire status {wire}"
        );
    }
}

/// 非 JSON / 数组 / 类型不匹配 ⇒ `Malformed`（**不** panic）；`null` 是 no-op。
#[test]
fn parse_rejects_malformed_payloads() {
    let p = GitLabProvider;
    assert!(matches!(
        p.parse_pull_request(b"{oops"),
        Err(VcsError::Malformed(_))
    ));
    assert!(matches!(
        p.parse_ci_status(b"[]"),
        Err(VcsError::Malformed(_))
    ));
    assert!(matches!(
        p.parse_pull_request(b"[1]"),
        Err(VcsError::Malformed(_))
    ));
    assert!(matches!(
        p.parse_ci_status(br#"{"object_attributes":{"sha":123}}"#),
        Err(VcsError::Malformed(_))
    ));
    // `null` 放行（Go 的 no-op 语义）。
    let from_null = p.parse_ci_status(b"null").expect("null is a no-op");
    assert_eq!(from_null.sha, "");
}

/// 时间戳归一化的**逐条**对照（含 Go `RFC3339Nano` 的尾零裁剪与偏移换算）。
#[test]
fn normalize_gitlab_time_matches_go_rfc3339nano() {
    for (raw, expected) in [
        // GitLab 实际发的 MST 形态（UTC）。
        ("2017-09-20 08:31:45 UTC", "2017-09-20T08:31:45Z"),
        // 小数秒：尾零被裁掉（Go 的 RFC3339Nano 行为）。
        ("2017-09-20 08:31:45.123000 UTC", "2017-09-20T08:31:45.123Z"),
        // RFC3339 自身（`T` 分隔 + `Z`）。
        ("2026-09-01T00:00:00Z", "2026-09-01T00:00:00Z"),
        // 数字偏移：`-0700` 与 `+05:30` 都换算到 UTC。
        ("2026-09-01 00:00:00 -0700", "2026-09-01T07:00:00Z"),
        ("2026-09-01T05:30:00+05:30", "2026-09-01T00:00:00Z"),
        ("2026-01-01 00:30:00 -0100", "2026-01-01T01:30:00Z"),
        // 闰日与纪元下界（公历算法最容易错的两处）。
        ("2024-02-29 12:00:00 UTC", "2024-02-29T12:00:00Z"),
        ("1970-01-01 00:00:00 UTC", "1970-01-01T00:00:00Z"),
        ("1969-12-31 23:59:59 UTC", "1969-12-31T23:59:59Z"),
        // 纳秒精度保留（单调守卫靠它排序同一秒内的两个事件）。
        (
            "2026-09-01 00:00:00.000000123 UTC",
            "2026-09-01T00:00:00.000000123Z",
        ),
    ] {
        assert_eq!(
            normalize_gitlab_time(raw).as_deref(),
            Some(expected),
            "raw = {raw:?}"
        );
    }
}

/// 认不出的输入 ⇒ `None`（= 上游的 `""`，handler 回落摄入时间）。
#[test]
fn normalize_gitlab_time_rejects_unknown_layouts() {
    for raw in [
        "",
        "not a time",
        "2026-09-01",
        "2026-09-01 00:00:00 America/New_York",
        "2026-13-01 00:00:00 UTC",
        "2026-09-01 25:00:00 UTC",
        "2026-09-01 00:00:00 +25:00",
        "2026-09-01T00:00:00.",
    ] {
        assert_eq!(normalize_gitlab_time(raw), None, "raw = {raw:?}");
    }
}

/// `days_from_civil` / `civil_from_days` 互逆（含闰年前后与世纪边界）。
#[test]
fn civil_date_helpers_round_trip() {
    for days in [-100_000i64, -719_468, -1, 0, 1, 11_016, 20_000, 100_000] {
        let (year, month, day) = civil_from_days(days);
        assert_eq!(days_from_civil(year, month, day), days, "days = {days}");
    }
    // 锚点。
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    assert_eq!(civil_from_days(0), (1970, 1, 1));
}

/// `split_namespace` 的三种形态。
#[test]
fn split_namespace_keeps_subgroups_in_owner() {
    assert_eq!(
        split_namespace("group/subgroup/repo"),
        ("group/subgroup".to_string(), "repo".to_string())
    );
    assert_eq!(
        split_namespace("group/repo"),
        ("group".to_string(), "repo".to_string())
    );
    assert_eq!(split_namespace("repo"), (String::new(), "repo".to_string()));
    assert_eq!(
        split_namespace("/repo/"),
        (String::new(), "repo".to_string())
    );
}

/// registry 的**三个检验**之二 + 之三：注册后 `gitlab` 可解析，且未注册的 kind
/// 报**可区分**的错误（不是 panic、不是静默 `None`）。
#[test]
fn register_makes_gitlab_resolvable_and_unknown_kind_errors() {
    let mut registry = Registry::new();
    register(&mut registry);
    assert_eq!(registry.kinds(), vec![VcsProviderKind::GitLab]);
    assert_eq!(
        registry
            .get(VcsProviderKind::GitLab)
            .expect("gitlab")
            .kind(),
        VcsProviderKind::GitLab
    );

    // 只注册 gitlab ⇒ forgejo 未注册，`get` 必须给出 UnknownProvider（带 kind）。
    let err = registry
        .get(VcsProviderKind::Forgejo)
        .err()
        .expect("forgejo 未注册");
    assert!(matches!(
        err,
        crate::registry::RegistryError::UnknownProvider(VcsProviderKind::Forgejo)
    ));
    assert_eq!(
        err.to_string(),
        "vcs: no provider registered for kind `forgejo`"
    );
}
