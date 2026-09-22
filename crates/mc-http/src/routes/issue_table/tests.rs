//! `issue_table` HTTP 层的单元测试（从 `issue_table.rs` 拆出，R7 单文件 800 行上限；
//! `scripts/file_size_check.py` + 门 ⑩ 执行）。
//!
//! 只覆盖纯函数（DTO 解码 / 校验 / 指纹 / cursor / 分组 key）；需要真库的 HTTP e2e 在
//! `crates/mc-http/tests/issue_table.rs`（`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）。

use axum::body::Bytes;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use mc_core::Id;
use mc_repos::issue_table::{
    TableActor, TableFacetKind, TableFilter, TableGroupKey, TableGroupKind, TableMyRelation,
    TableOrder, TableScope, TableSortDirection, TableSortField, TABLE_DEFAULT_PAGE_SIZE,
    TABLE_MAX_FACETS, TABLE_MAX_PAGE_SIZE,
};

use super::cursor::*;
use super::spec::*;

use super::*;

fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn actor(kind: &str, id: u128) -> TableActor {
    TableActor {
        kind: kind.to_string(),
        id: uuid(id),
    }
}

fn workspace_scope() -> TableScope {
    TableScope::Workspace {
        assignee_types: Vec::new(),
    }
}

fn base_input(limit: i64) -> RowsRequest {
    RowsRequest {
        query: TableQueryDto::default(),
        group: GroupDto {
            kind: "status".to_string(),
            ..GroupDto::default()
        },
        group_key: Some("status:todo".to_string()),
        hierarchy: HierarchyDto::default(),
        parent_id: None,
        page: PageDto {
            limit,
            cursor: None,
        },
    }
}

#[test]
fn page_limit_bounds() {
    assert_eq!(
        normalize_page(&PageDto::default()).unwrap(),
        TABLE_DEFAULT_PAGE_SIZE
    );
    assert!(normalize_page(&PageDto {
        limit: 0,
        cursor: None
    })
    .is_ok());
    assert!(normalize_page(&PageDto {
        limit: TABLE_MAX_PAGE_SIZE,
        cursor: None
    })
    .is_ok());
    assert!(normalize_page(&PageDto {
        limit: TABLE_MAX_PAGE_SIZE + 1,
        cursor: None
    })
    .is_err());
    assert!(normalize_page(&PageDto {
        limit: -1,
        cursor: None
    })
    .is_err());
}

#[test]
fn group_key_round_trip() {
    assert_eq!(
        parse_group_key(TableGroupKind::Status, Some("status:todo")).unwrap(),
        TableGroupKey::Status("todo".into())
    );
    assert_eq!(
        parse_group_key(TableGroupKind::Assignee, Some("assignee:unassigned")).unwrap(),
        TableGroupKey::Assignee(None)
    );
    assert_eq!(
        parse_group_key(
            TableGroupKind::Assignee,
            Some(&format!("assignee:user:{}", uuid(7)))
        )
        .unwrap(),
        TableGroupKey::Assignee(Some(actor("user", 7)))
    );
    assert_eq!(
        parse_group_key(TableGroupKind::Project, Some("project:none")).unwrap(),
        TableGroupKey::Project(None)
    );
    assert_eq!(
        parse_group_key(TableGroupKind::None, None).unwrap(),
        TableGroupKey::None
    );
    assert!(parse_group_key(TableGroupKind::None, Some("status:x")).is_err());
    assert!(parse_group_key(TableGroupKind::Status, None).is_err());
    assert!(parse_group_key(TableGroupKind::Status, Some("assignee:user:x")).is_err());
    assert!(parse_group_key(TableGroupKind::Priority, Some("priority:urgent")).is_ok());
}

#[test]
fn group_value_shape_matches_upstream() {
    let value = group_value("status:todo", TableGroupKind::Status);
    assert_eq!(value["kind"], "status");
    assert_eq!(value["status"], "todo");
    assert!(value["actor"].is_null());

    let value = group_value("assignee:unassigned", TableGroupKind::Assignee);
    assert_eq!(value["kind"], "assignee");
    assert!(value["actor"].is_null());

    let id = uuid(3);
    let value = group_value(&format!("assignee:agent:{id}"), TableGroupKind::Assignee);
    assert_eq!(value["actor"]["type"], "agent");
    assert_eq!(value["actor"]["id"], id.to_string());

    let value = group_value("project:none", TableGroupKind::Project);
    assert_eq!(value["kind"], "project");
    assert!(value.get("project_id").is_none());

    let value = group_value("priority:urgent", TableGroupKind::Priority);
    assert_eq!(value["priority"], "urgent");
}

#[test]
fn cursor_round_trip_and_mismatch() {
    let mut wire = CursorWire::new("sha256:abc");
    wire.group_key = Some("status".into());
    wire.row_created_at = "2026-01-02T03:04:05.000006Z".into();
    wire.row_id = uuid(9).to_string();
    wire.sort_is_null = true;
    let encoded = wire.encode();
    let decoded = CursorWire::decode(&encoded).unwrap();
    assert_eq!(decoded.encode(), encoded);
    assert!(decoded.matches("sha256:abc", Some("status"), None).is_ok());
    assert!(matches!(
        decoded.matches("sha256:other", Some("status"), None),
        Err(TableError::CursorMismatch)
    ));
    assert!(matches!(
        decoded.matches("sha256:abc", Some("project"), None),
        Err(TableError::CursorMismatch)
    ));
    let cursor = decoded.into_row_cursor().unwrap();
    assert!(cursor.sort_is_null);
    assert!(cursor.sort_value.is_none());
    assert_eq!(cursor.row_id, Id::from(uuid(9)));
    assert!(CursorWire::decode("not-hex").is_err());
    assert!(CursorWire::decode(&hex::encode(b"{\"v\":9}")).is_err());
}

#[test]
fn group_cursor_requires_three_fields() {
    let mut wire = CursorWire::new("sha256:abc");
    wire.group_order = Some(3);
    assert!(wire.clone().into_group_cursor().is_err());
    wire.group_sort_key = Some("10".into());
    wire.group_cursor_key = Some("todo".into());
    let cursor = wire.into_group_cursor().unwrap();
    assert_eq!(cursor.order, 3);
    assert_eq!(cursor.value, "todo");
}

#[test]
fn fingerprint_is_order_insensitive_and_dimension_sensitive() {
    let workspace = Id::from(uuid(1));
    let order = TableOrder::default();
    let mut filter = TableFilter {
        scope: workspace_scope(),
        statuses: vec!["todo".into(), "in_progress".into()],
        ..TableFilter::default()
    };
    let first = query_fingerprint(workspace, &filter, order, false);
    filter.statuses = vec!["in_progress".into(), "todo".into(), "todo".into()];
    assert_eq!(query_fingerprint(workspace, &filter, order, false), first);
    assert!(first.starts_with("sha256:"));
    filter.statuses = vec!["todo".into()];
    assert_ne!(query_fingerprint(workspace, &filter, order, false), first);
    let sorted = TableFilter {
        scope: workspace_scope(),
        statuses: vec!["todo".into(), "in_progress".into()],
        ..TableFilter::default()
    };
    assert_ne!(
        query_fingerprint(Id::from(uuid(2)), &sorted, order, false),
        first
    );
    let desc = TableOrder {
        field: TableSortField::Position,
        direction: Some(TableSortDirection::Desc),
    };
    assert_ne!(query_fingerprint(workspace, &sorted, desc, false), first);
}

#[test]
fn scope_and_filter_validation() {
    let user = Id::from(uuid(42));
    assert!(matches!(
        build_scope(&ScopeDto::default(), user).unwrap(),
        TableScope::Workspace { .. }
    ));
    assert!(matches!(
        build_scope(
            &ScopeDto {
                kind: "my".into(),
                relation: "involved".into(),
                ..ScopeDto::default()
            },
            user
        )
        .unwrap(),
        TableScope::My {
            relation: TableMyRelation::Involved,
            ..
        }
    ));
    assert!(build_scope(
        &ScopeDto {
            kind: "project".into(),
            ..ScopeDto::default()
        },
        user
    )
    .is_err());
    assert!(build_scope(
        &ScopeDto {
            kind: "nope".into(),
            ..ScopeDto::default()
        },
        user
    )
    .is_err());
    assert!(build_scope(
        &ScopeDto {
            kind: "workspace".into(),
            assignee_types: vec!["robot".into()],
            ..ScopeDto::default()
        },
        user
    )
    .is_err());

    // 空值等价于未提供；非空 → 422。
    let empty = FiltersDto::default();
    assert!(build_filters(&empty, workspace_scope(), "").is_ok());
    let empty_labels = FiltersDto {
        label_ids: Vec::new(),
        properties: Some(JsonValue::Object(serde_json::Map::default())),
        ..FiltersDto::default()
    };
    assert!(build_filters(&empty_labels, workspace_scope(), "").is_ok());
    let labels = FiltersDto {
        label_ids: vec![uuid(5).to_string()],
        ..FiltersDto::default()
    };
    assert!(matches!(
        build_filters(&labels, workspace_scope(), ""),
        Err(TableError::UnsupportedFilter { .. })
    ));
    let working = FiltersDto {
        working_issue_ids: Some(Vec::new()),
        ..FiltersDto::default()
    };
    assert!(matches!(
        build_filters(&working, workspace_scope(), ""),
        Err(TableError::UnsupportedFilter { .. })
    ));
    let bad_status = FiltersDto {
        statuses: vec!["x".repeat(65)],
        ..FiltersDto::default()
    };
    assert!(build_filters(&bad_status, workspace_scope(), "").is_err());
    let bad_priority = FiltersDto {
        priorities: vec!["nope".into()],
        ..FiltersDto::default()
    };
    assert!(build_filters(&bad_priority, workspace_scope(), "").is_err());
    let explicit_empty = FiltersDto {
        assignees: Some(Vec::new()),
        ..FiltersDto::default()
    };
    let (filter, flag) = build_filters(&explicit_empty, workspace_scope(), "").unwrap();
    assert!(flag);
    assert_eq!(filter.assignees, Some(Vec::new()));
}

#[test]
fn group_spec_rejects_unsupported_dimensions() {
    assert!(matches!(
        build_group_spec(
            &GroupDto {
                kind: "none".into(),
                ..GroupDto::default()
            },
            false
        ),
        Err(TableError::Api(_))
    ));
    assert!(build_group_spec(
        &GroupDto {
            kind: "none".into(),
            ..GroupDto::default()
        },
        true
    )
    .is_ok());
    for kind in [
        "label",
        "parent",
        "property",
        "compound",
        "status_category",
        "",
    ] {
        assert!(
            matches!(
                build_group_spec(
                    &GroupDto {
                        kind: kind.into(),
                        ..GroupDto::default()
                    },
                    true
                ),
                Err(TableError::Unsupported { .. })
            ),
            "kind {kind} should be unsupported"
        );
    }
    assert!(matches!(
        build_group_spec(
            &GroupDto {
                kind: "status".into(),
                property_id: Some(uuid(4).to_string()),
                ..GroupDto::default()
            },
            true
        ),
        Err(TableError::Unsupported { .. })
    ));
}

#[test]
fn facet_kinds_and_limits() {
    let ok = build_facets(&[
        FacetSpecDto {
            kind: "status".into(),
            property_id: None,
        },
        FacetSpecDto {
            kind: "priority".into(),
            property_id: None,
        },
    ])
    .unwrap();
    assert_eq!(ok, vec![TableFacetKind::Status, TableFacetKind::Priority]);
    assert!(matches!(
        build_facets(&[FacetSpecDto {
            kind: "working_agents".into(),
            property_id: None,
        }]),
        Err(TableError::Unsupported { .. })
    ));
    let too_many: Vec<FacetSpecDto> = (0..=TABLE_MAX_FACETS)
        .map(|_| FacetSpecDto {
            kind: "status".into(),
            property_id: None,
        })
        .collect();
    assert!(matches!(build_facets(&too_many), Err(TableError::Api(_))));
}

#[test]
fn body_decoding_rejects_unknown_fields() {
    let ok = decode_body::<GroupsRequest>(&Bytes::from_static(
        br#"{"query":{"scope":{"kind":"workspace"}},"group":{"kind":"status"},"page":{"limit":10}}"#,
    ));
    assert!(ok.is_ok());
    let unknown = decode_body::<GroupsRequest>(&Bytes::from_static(
        br#"{"query":{},"group":{"kind":"status"},"wat":1}"#,
    ));
    assert!(unknown.is_err());
    let trailing = decode_body::<GroupsRequest>(&Bytes::from_static(
        br#"{"query":{},"group":{"kind":"status"}} trailing"#,
    ));
    assert!(trailing.is_err());
    let oversized = decode_body::<GroupsRequest>(&Bytes::from(vec![b' '; MAX_BODY_BYTES + 1]));
    assert!(oversized.is_err());
}

#[test]
fn rows_input_validates_parent_and_group_key() {
    let workspace = Id::from(uuid(1));
    let user = Id::from(uuid(2));
    let mut request = base_input(10);
    // 无 `hierarchy.enabled` 时 `parent_id` → 400（上游同）。
    request.parent_id = Some(uuid(3).to_string());
    assert!(matches!(
        build_rows_input(workspace, user, &request),
        Err(TableError::Api(_))
    ));
    request.hierarchy = HierarchyDto { enabled: true };
    assert!(build_rows_input(workspace, user, &request).is_ok());
    // `group.kind=none` 时带 `group_key` → 400。
    let mut request = base_input(10);
    request.group = GroupDto {
        kind: "none".into(),
        ..GroupDto::default()
    };
    assert!(matches!(
        build_rows_input(workspace, user, &request),
        Err(TableError::Api(_))
    ));
    request.group_key = None;
    let input = build_rows_input(workspace, user, &request).unwrap();
    assert_eq!(input.group_key, TableGroupKey::None);
    assert_eq!(input.group.kind, TableGroupKind::None);
}
