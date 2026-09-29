//! inbox 的单元测试（不依赖 DB）。
//!
//! 从 `routes/inbox.rs` 拆出（门 ⑩ 第 8 批）。原先靠 `use super::*` 拿到父模块的
//! import；拆分后父模块只留 `router()` 真正需要的那几项，所以这里**逐个显式 import**
//! （也顺带避开了 `clippy::wildcard_imports`）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::HeaderMap;
use axum::Router;
use chrono::Utc;

use mc_core::Id;
use mc_repos::inbox::{ArchivedInboxFilter, InboxItemRow};

use crate::routes::inbox::query::*;
use crate::routes::inbox::*;
use crate::state::AppState;

fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 装配完整 router + state，读起来比抽 helper 清楚。
async fn mounted_router_builds_without_route_conflict() {
    // 完整 router 装配：任何 path+method 重复都会让 axum 在 `.merge` 时 panic。
    let db = mc_db::Db::connect_lazy("postgres://u:p@127.0.0.1:5432/none", 1, 0).unwrap();
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "test"));
    let state = Arc::new(AppState::new(
        db,
        crate::state::RuntimeHandles {
            actors: mc_core::actor::ActorRegistry::new(),
            adapters: Arc::new(crate::state::AdapterRegistry::default()),
        },
        crate::state::ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            ..Default::default()
        },
        realtime,
        ws,
    ));
    let _app: Router = crate::routes::router(state.clone()).with_state(state);
}

#[test]
fn workspace_resolution_prefers_header_then_query() {
    let ws = Id::new();
    let mut headers = HeaderMap::new();
    headers.insert(WORKSPACE_ID_HEADER, ws.as_string().parse().unwrap());
    assert_eq!(
        resolve_workspace_id(&headers, &q(&[("workspace_id", &Id::new().as_string())])).unwrap(),
        ws
    );
    let empty = HeaderMap::new();
    assert_eq!(
        resolve_workspace_id(&empty, &q(&[("workspace_id", &ws.as_string())])).unwrap(),
        ws
    );
    // 缺失 / 空 / 非法 → 400 "invalid workspace id"（上游 parseUUIDOrBadRequest）。
    for headers in [HeaderMap::new(), {
        let mut h = HeaderMap::new();
        h.insert(WORKSPACE_ID_HEADER, "not-a-uuid".parse().unwrap());
        h
    }] {
        let err = resolve_workspace_id(&headers, &q(&[])).unwrap_err();
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.message(), "validation error: invalid workspace id");
    }
    let empty_header = {
        let mut h = HeaderMap::new();
        h.insert(WORKSPACE_ID_HEADER, "   ".parse().unwrap());
        h
    };
    assert!(resolve_workspace_id(&empty_header, &q(&[("workspace_id", "")])).is_err());
}

#[test]
fn filters_are_sorted_deduped_and_validated() {
    let filter = parse_filter(&q(&[
        ("statuses", "todo,backlog,todo"),
        ("priorities", "high"),
        ("unread_only", "true"),
    ]))
    .unwrap();
    assert_eq!(filter.statuses, vec!["backlog", "todo"]);
    assert_eq!(filter.priorities, vec!["high"]);
    assert!(filter.unread_only);
    assert_eq!(filter.actors, Vec::<String>::new());

    // 空串等于未传（上游 `q.Get(name) != ""`）。
    assert!(parse_filter(&q(&[("statuses", "")]))
        .unwrap()
        .statuses
        .is_empty());

    for (query, message) in [
        (
            q(&[("unread_only", "1")]),
            "validation error: invalid unread_only",
        ),
        (
            q(&[("statuses", "a,,b")]),
            "validation error: empty filter value",
        ),
    ] {
        let err = parse_filter(&query).unwrap_err();
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.message(), message);
    }

    let long = "x".repeat(FILTER_MAX_RAW_LEN + 1);
    assert_eq!(
        parse_filter(&q(&[("statuses", &long)]))
            .unwrap_err()
            .message(),
        "validation error: filter is too long"
    );
    let many = (0..=FILTER_MAX_VALUES)
        .map(|i| format!("s{i}"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        parse_filter(&q(&[("statuses", &many)]))
            .unwrap_err()
            .message(),
        "validation error: too many filter values"
    );
}

#[test]
fn archive_limit_matches_upstream_bounds() {
    assert_eq!(parse_archived_limit(&q(&[])).unwrap(), 50);
    assert_eq!(parse_archived_limit(&q(&[("limit", "1")])).unwrap(), 1);
    assert_eq!(parse_archived_limit(&q(&[("limit", "100")])).unwrap(), 100);
    for bad in ["0", "101", "abc", "-1"] {
        let err = parse_archived_limit(&q(&[("limit", bad)])).unwrap_err();
        assert_eq!(err.http_status(), 400);
        assert_eq!(
            err.message(),
            "validation error: limit must be between 1 and 100"
        );
    }
}

#[test]
fn list_window_defaults_and_bounds() {
    assert_eq!(parse_list_window(&q(&[])).unwrap(), (200, 0));
    assert_eq!(
        parse_list_window(&q(&[("limit", "5"), ("offset", "10")])).unwrap(),
        (5, 10)
    );
    assert_eq!(
        parse_list_window(&q(&[("limit", "501")]))
            .unwrap_err()
            .message(),
        "validation error: limit must be between 1 and 500"
    );
    assert_eq!(
        parse_list_window(&q(&[("offset", "-1")]))
            .unwrap_err()
            .message(),
        "validation error: offset must be >= 0"
    );
}

#[test]
fn cursor_roundtrip_is_scope_bound() {
    let ws = Id::new();
    let user = Id::new();
    let filter = ArchivedInboxFilter {
        statuses: vec!["todo".into()],
        ..ArchivedInboxFilter::default()
    };
    let tag = archive_scope_tag(ws, user, &filter);
    let row = InboxItemRow {
        id: Id::new().as_uuid(),
        workspace_id: ws.as_uuid(),
        user_id: user.as_uuid(),
        issue_id: None,
        actor_type: "user".into(),
        actor_id: user.as_string(),
        category: "new_comment".into(),
        title: "t".into(),
        body: None,
        read_at: None,
        archived_at: None,
        created_at: Utc::now(),
        issue_status: None,
        issue_priority: None,
    };
    let encoded = encode_cursor(&tag, &row).unwrap();
    let parsed = parse_cursor(&q(&[("cursor", &encoded)]), &tag)
        .unwrap()
        .expect("cursor present");
    assert_eq!(parsed.id, row.id());
    assert_eq!(
        parsed.created_at.timestamp_micros(),
        row.created_at.timestamp_micros()
    );

    // 换了 scope（不同用户 / 不同过滤）就不能续页。
    let other = archive_scope_tag(ws, user, &ArchivedInboxFilter::default());
    assert_eq!(
        parse_cursor(&q(&[("cursor", &encoded)]), &other)
            .unwrap_err()
            .message(),
        "validation error: invalid archive cursor"
    );
    for bad in ["zzzz", "e30", "not json"] {
        assert!(parse_cursor(&q(&[("cursor", bad)]), &tag).is_err());
    }
    // 空串等于未传（上游 `q.Get("cursor") != ""`）→ 首页，不是错误。
    assert!(parse_cursor(&q(&[("cursor", "")]), &tag).unwrap().is_none());
    let too_long = "a".repeat(CURSOR_MAX_LEN + 1);
    assert_eq!(
        parse_cursor(&q(&[("cursor", &too_long)]), &tag)
            .unwrap_err()
            .message(),
        "validation error: invalid archive cursor"
    );
}

#[test]
fn list_body_preview_matches_upstream_character_limit() {
    // 非 new_comment / 无 issue：原样返回。
    let long = "字".repeat(500);
    assert_eq!(
        list_body_preview("new_issue", true, Some(&long)),
        Some(long.clone())
    );
    assert_eq!(
        list_body_preview("new_comment", false, Some(&long)),
        Some(long.clone())
    );
    assert_eq!(list_body_preview("new_comment", true, None), None);

    // 恰好 200 字符：不截断。
    let exact = "a".repeat(200);
    assert_eq!(
        list_body_preview("new_comment", true, Some(&exact)),
        Some(exact.clone())
    );
    // 201 字符：前 199 + 省略号 = 200 字符。
    let over = "a".repeat(201);
    let preview = list_body_preview("new_comment", true, Some(&over)).unwrap();
    assert_eq!(preview.chars().count(), 200);
    assert!(preview.ends_with('…'));
    assert_eq!(preview, format!("{}…", "a".repeat(199)));

    // 多字节字符不会被切坏。
    let multibyte = "字".repeat(201);
    let preview = list_body_preview("new_comment", true, Some(&multibyte)).unwrap();
    assert_eq!(preview.chars().count(), 200);
    assert!(preview.starts_with('字'));
}

#[test]
fn terminal_status_keys_are_the_builtin_ones() {
    assert_eq!(builtin_terminal_status_keys(), ["done", "cancelled"]);
}
