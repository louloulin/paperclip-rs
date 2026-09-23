//! `/v1` 契约台账与线格式 DTO 的单测。
//!
//! 上游有一个 `contract_test.go`：把 `Operations` 台账钉在 `openapi.yaml` 上，
//! 好让「路由 / scope / 文档」不能各自漂移。本仓**没有**那份 YAML 资产（写集只有
//! `v1.rs`），所以这里的钉子换成两张更硬的表：
//!
//! 1. `docs/fixtures/m6-declared-routes.tsv` 的 `/v1` 段（**声明路由**逐字，含 `{issue_ref}`
//!    这种上游契约形态）—— `operations_match_the_declared_v1_routes`；
//! 2. `/api/plugin-bridge/v1/*` 段必须与 `/v1` 段**逐条同形**（同批 handler、两个挂载点）
//!    —— 同一个测试的后半段。

use super::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// 读 `docs/fixtures/m6-declared-routes.tsv` 的 `METHOD<TAB>PATH` 数据行。
fn declared_rows() -> Vec<(String, String)> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/fixtures/m6-declared-routes.tsv");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .skip(1) // 表头 `METHOD\tPATH`
        .map(|line| {
            let (method, route) = line.split_once('\t').expect("METHOD<TAB>PATH");
            (method.to_string(), route.to_string())
        })
        .collect()
}

/// `OPERATIONS` 的 `(METHOD, PATH)` 视图，路径走 `full_path()`（带 `/v1` 前缀）。
fn ledger_rows() -> Vec<(String, String)> {
    OPERATIONS
        .iter()
        .map(|operation| (operation.method.to_string(), operation.full_path()))
        .collect()
}

#[test]
fn operations_match_the_declared_v1_routes() {
    let declared = declared_rows();
    let v1: Vec<(String, String)> = declared
        .iter()
        .filter(|(_, route)| route.starts_with("/v1/"))
        .cloned()
        .collect();
    assert_eq!(v1.len(), 9, "M6-7 的 /v1 面就是 9 条");
    assert_eq!(ledger_rows(), v1, "台账与声明路由逐条同序");

    // 同一批 handler 的第二个挂载点：去掉前缀后必须与 /v1 逐条同形。
    let bridge: Vec<(String, String)> = declared
        .iter()
        .filter_map(|(method, route)| {
            route
                .strip_prefix("/api/plugin-bridge")
                .map(|rest| (method.clone(), rest.to_string()))
        })
        .collect();
    // bridge 前缀下**还有** M6-8 的 hook 入站路由：它与 /v1 同前缀，但不同批次（凭据是
    // hook key + HMAC 签名，不是安装令牌）—— 显式挑出来，免得它混进「同形」断言。
    let (hooks, actions): (Vec<_>, Vec<_>) = bridge
        .into_iter()
        .partition(|(_, route)| route.starts_with("/v1/hooks/"));
    assert_eq!(
        hooks,
        vec![("POST".to_string(), "/v1/hooks/{key}".to_string())]
    );
    assert_eq!(actions.len(), 9);
    assert_eq!(
        actions,
        ledger_rows(),
        "bridge 与 /v1 两侧同形（同 DTO、同路径）"
    );
}

#[test]
fn ledger_is_the_upstream_operation_list() {
    let expected = vec![
        ("GET", PATH_CONTEXT),
        ("GET", PATH_ISSUE),
        ("PATCH", PATH_ISSUE),
        ("GET", PATH_ISSUE_COMMENTS),
        ("POST", PATH_ISSUE_COMMENTS),
        ("GET", PATH_STORAGE_SCOPE),
        ("GET", PATH_STORAGE_VALUE),
        ("PUT", PATH_STORAGE_VALUE),
        ("DELETE", PATH_STORAGE_VALUE),
    ];
    let actual: Vec<(&str, &str)> = OPERATIONS
        .iter()
        .map(|operation| (operation.method, operation.path))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(BASE_PATH, "/v1");
}

#[test]
fn operation_policies_match_the_upstream_ledger() {
    // 逐条抄上游 `routes.go` 的台账（方法 + 路径 + 策略四元组 + 凭据/限流集合）。
    let extension_policy = OperationPolicy {
        credentials: PLUGIN_CREDENTIALS,
        scope: "",
        risk: RiskLevel::Read,
        audit: AuditStatus::NotRequired,
        rate_limits: PLUGIN_RATE_LIMITS,
    };
    let write_policy = OperationPolicy {
        credentials: PLUGIN_CREDENTIALS,
        scope: "",
        risk: RiskLevel::ContentWrite,
        audit: AuditStatus::Planned,
        rate_limits: PLUGIN_RATE_LIMITS,
    };
    let shared_policy = |scope, risk, audit| OperationPolicy {
        credentials: SHARED_CREDENTIALS,
        scope,
        risk,
        audit,
        rate_limits: SHARED_RATE_LIMITS,
    };
    let operation = |method, path, contract, policy| Operation {
        method,
        path,
        contract,
        policy,
    };
    let expected = [
        operation(
            "GET",
            PATH_CONTEXT,
            ContractKind::PluginExtension,
            extension_policy,
        ),
        operation(
            "GET",
            PATH_ISSUE,
            ContractKind::SharedResource,
            shared_policy("issues:read", RiskLevel::Read, AuditStatus::Planned),
        ),
        operation(
            "PATCH",
            PATH_ISSUE,
            ContractKind::SharedResource,
            shared_policy(
                "issues:write",
                RiskLevel::ContentWrite,
                AuditStatus::Planned,
            ),
        ),
        operation(
            "GET",
            PATH_ISSUE_COMMENTS,
            ContractKind::SharedResource,
            shared_policy("comments:read", RiskLevel::Read, AuditStatus::Planned),
        ),
        operation(
            "POST",
            PATH_ISSUE_COMMENTS,
            ContractKind::SharedResource,
            shared_policy(
                "comments:write",
                RiskLevel::ContentWrite,
                AuditStatus::Planned,
            ),
        ),
        operation(
            "GET",
            PATH_STORAGE_SCOPE,
            ContractKind::PluginExtension,
            extension_policy,
        ),
        operation(
            "GET",
            PATH_STORAGE_VALUE,
            ContractKind::PluginExtension,
            extension_policy,
        ),
        operation(
            "PUT",
            PATH_STORAGE_VALUE,
            ContractKind::PluginExtension,
            write_policy,
        ),
        operation(
            "DELETE",
            PATH_STORAGE_VALUE,
            ContractKind::PluginExtension,
            write_policy,
        ),
    ];
    assert_eq!(expected.len(), OPERATIONS.len());
    for (got, want) in OPERATIONS.iter().zip(expected.iter()) {
        assert_eq!(got, want, "{} {}", want.method, want.path);
    }
}

#[test]
fn credential_sets_and_rate_limit_tiers_are_the_two_declared_groups() {
    assert_eq!(
        SHARED_CREDENTIALS,
        &[
            CredentialKind::UserOAuth,
            CredentialKind::PersonalAccessToken,
            CredentialKind::PluginInstallation,
            CredentialKind::PluginInvocation,
        ]
    );
    assert_eq!(
        PLUGIN_CREDENTIALS,
        &[
            CredentialKind::PluginInstallation,
            CredentialKind::PluginInvocation,
        ]
    );
    assert_eq!(
        SHARED_RATE_LIMITS,
        &[
            RateLimitProfile::UserDefault,
            RateLimitProfile::PluginStrict
        ]
    );
    assert_eq!(PLUGIN_RATE_LIMITS, &[RateLimitProfile::PluginStrict]);
    // 插件扩展面的两个操作里，只有 storage 的写操作声明了审计（上游台账逐字）。
    let extension_writes: Vec<&Operation> = OPERATIONS
        .iter()
        .filter(|operation| {
            operation.contract == ContractKind::PluginExtension
                && operation.policy.risk != RiskLevel::Read
        })
        .collect();
    assert_eq!(extension_writes.len(), 2);
    for operation in extension_writes {
        assert_eq!(operation.policy.audit, AuditStatus::Planned);
        assert_eq!(operation.policy.credentials, PLUGIN_CREDENTIALS);
    }
}

#[test]
fn operation_for_looks_up_by_method_and_path() {
    let found = operation_for("GET", PATH_CONTEXT).expect("context operation");
    assert_eq!(found.contract, ContractKind::PluginExtension);
    assert_eq!(
        operation_for("get", PATH_CONTEXT).map(|op| op.method),
        Some("GET")
    );
    assert_eq!(
        operation_for("PATCH", PATH_ISSUE).map(|op| op.path),
        Some(PATH_ISSUE)
    );
    // 两种形态都认：M6-7 的 route 表用完整路径，台账用相对 BASE_PATH 的路径。
    assert_eq!(
        operation_for("GET", "/v1/context").map(|op| op.path),
        Some(PATH_CONTEXT)
    );
    assert_eq!(
        operation_for("DELETE", "/v1/storage/{scope}/{key}").map(|op| op.path),
        Some(PATH_STORAGE_VALUE)
    );
    assert!(operation_for("POST", PATH_CONTEXT).is_none());
    assert!(operation_for("GET", "/v2/context").is_none());
    assert!(operation_for("GET", "/api/plugin-bridge/v1/context").is_none());
}

#[test]
fn enum_wire_forms_are_the_upstream_literals() {
    let cases: Vec<(&str, String)> = vec![
        (
            CredentialKind::UserOAuth.as_str(),
            json!(CredentialKind::UserOAuth).to_string(),
        ),
        (
            CredentialKind::PersonalAccessToken.as_str(),
            json!(CredentialKind::PersonalAccessToken).to_string(),
        ),
        (
            CredentialKind::PluginInstallation.as_str(),
            json!(CredentialKind::PluginInstallation).to_string(),
        ),
        (
            CredentialKind::PluginInvocation.as_str(),
            json!(CredentialKind::PluginInvocation).to_string(),
        ),
        (
            ActorKind::Member.as_str(),
            json!(ActorKind::Member).to_string(),
        ),
        (
            ActorKind::Plugin.as_str(),
            json!(ActorKind::Plugin).to_string(),
        ),
        (RiskLevel::Read.as_str(), json!(RiskLevel::Read).to_string()),
        (
            RiskLevel::ContentWrite.as_str(),
            json!(RiskLevel::ContentWrite).to_string(),
        ),
        (RiskLevel::High.as_str(), json!(RiskLevel::High).to_string()),
        (
            RateLimitProfile::UserDefault.as_str(),
            json!(RateLimitProfile::UserDefault).to_string(),
        ),
        (
            RateLimitProfile::PluginStrict.as_str(),
            json!(RateLimitProfile::PluginStrict).to_string(),
        ),
        (
            AuditStatus::NotRequired.as_str(),
            json!(AuditStatus::NotRequired).to_string(),
        ),
        (
            AuditStatus::Planned.as_str(),
            json!(AuditStatus::Planned).to_string(),
        ),
        (
            AuditStatus::Enforced.as_str(),
            json!(AuditStatus::Enforced).to_string(),
        ),
        (
            ContractKind::SharedResource.as_str(),
            json!(ContractKind::SharedResource).to_string(),
        ),
        (
            ContractKind::PluginExtension.as_str(),
            json!(ContractKind::PluginExtension).to_string(),
        ),
    ];
    for (literal, wire) in cases {
        assert_eq!(
            wire,
            format!("\"{literal}\""),
            "as_str 与 serde 形态必须一致"
        );
    }
}

#[test]
fn actor_serializes_with_the_declared_kinds() {
    let actor = Actor {
        kind: ActorKind::Plugin,
        subject_id: "inst_1".into(),
        workspace_id: "ws_1".into(),
        credential: CredentialKind::PluginInvocation,
    };
    assert_eq!(
        serde_json::to_value(&actor).expect("serialize actor"),
        json!({
            "kind": "plugin",
            "subject_id": "inst_1",
            "workspace_id": "ws_1",
            "credential": "plugin_invocation",
        })
    );
}

#[test]
fn code_for_status_matches_the_upstream_switch() {
    let cases = [
        (400, "invalid_request"),
        (401, "unauthorized"),
        (402, "payment_required"),
        (403, "forbidden"),
        (404, "not_found"),
        (409, "conflict"),
        (422, "incompatible"),
        (429, "rate_limited"),
        (507, "quota_exceeded"),
        (503, "service_unavailable"),
        (502, "upstream_unavailable"),
        // 上游 default 分支：405 / 413 / 500 都落 internal_error（405 的码由调用方显式给）。
        (405, "internal_error"),
        (413, "internal_error"),
        (500, "internal_error"),
        (418, "internal_error"),
    ];
    for (status, code) in cases {
        assert_eq!(code_for_status(status), code, "status {status}");
    }
}

#[test]
fn status_title_covers_the_contract_statuses() {
    let cases = [
        (400, "Bad Request"),
        (401, "Unauthorized"),
        (403, "Forbidden"),
        (404, "Not Found"),
        (405, "Method Not Allowed"),
        (409, "Conflict"),
        (422, "Unprocessable Entity"),
        (429, "Too Many Requests"),
        (502, "Bad Gateway"),
        (503, "Service Unavailable"),
        (507, "Insufficient Storage"),
        // 未登记的状态码：上游 `http.StatusText` 回空 ⇒ `Request failed`。
        (599, "Request failed"),
        (0, "Request failed"),
    ];
    for (status, title) in cases {
        assert_eq!(status_title(status), title, "status {status}");
    }
}

#[test]
fn problem_detail_shape_is_rfc_9457_with_the_legacy_alias() {
    let problem = problem_detail(403, "", "plugin api disabled", "req_1");
    let value = serde_json::to_value(&problem).expect("serialize problem");
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut expected = vec![
        "type",
        "title",
        "status",
        "code",
        "detail",
        "request_id",
        "error",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected, "空 errors 不出现（omitempty）");
    assert_eq!(value["type"], "urn:multica:problem:forbidden");
    assert_eq!(value["title"], "Forbidden");
    assert_eq!(value["status"], 403);
    assert_eq!(value["code"], "forbidden");
    assert_eq!(value["detail"], "plugin api disabled");
    assert_eq!(value["request_id"], "req_1");
    assert_eq!(
        value["error"], "plugin api disabled",
        "兼容别名恒等于 detail"
    );

    let with_errors = ProblemDetail {
        errors: vec![FieldError {
            field: "title".into(),
            code: "required".into(),
            message: "title is required".into(),
        }],
        ..problem
    };
    let value = serde_json::to_value(&with_errors).expect("serialize problem");
    assert_eq!(
        value["errors"],
        json!([{"field": "title", "code": "required", "message": "title is required"}])
    );
    assert_eq!(value["error"], value["detail"]);
}

#[test]
fn problem_helpers_pin_the_canonical_bodies() {
    let not_found = not_found_problem("req_2");
    assert_eq!(not_found.status, 404);
    assert_eq!(not_found.code, "not_found");
    assert_eq!(not_found.title, "Not Found");
    assert_eq!(not_found.detail, "resource not found");
    assert_eq!(not_found.problem_type, "urn:multica:problem:not_found");

    let method = method_not_allowed_problem("req_2");
    assert_eq!(method.status, 405);
    assert_eq!(
        method.code, "method_not_allowed",
        "显式给的码，不走 CodeForStatus"
    );
    assert_eq!(
        method.problem_type,
        "urn:multica:problem:method_not_allowed"
    );
    assert_eq!(method.detail, "method not allowed");

    // 空 code ⇒ 按状态码派生。
    let derived = problem_detail(429, "", "slow down", "req_3");
    assert_eq!(derived.code, "rate_limited");
    assert_eq!(derived.problem_type, "urn:multica:problem:rate_limited");
    assert_eq!(PROBLEM_CONTENT_TYPE, "application/problem+json");
    assert_eq!(HEADER_REQUEST_ID, "X-Request-Id");
}

#[test]
fn axum_path_converts_braces_to_colons() {
    let cases = [
        (PATH_CONTEXT, "/v1/context"),
        (PATH_ISSUE, "/v1/issues/:issue_ref"),
        (PATH_ISSUE_COMMENTS, "/v1/issues/:issue_ref/comments"),
        (PATH_STORAGE_SCOPE, "/v1/storage/:scope"),
        (PATH_STORAGE_VALUE, "/v1/storage/:scope/:key"),
    ];
    for (contract, axum) in cases {
        assert_eq!(axum_path(&format!("{BASE_PATH}{contract}")), axum);
        assert!(!axum.contains('{'), "不能留下花括号形态：{axum}");
    }
    // 台账条目的两种形态：契约形态（相对 BASE_PATH）+ axum 挂载形态。
    assert_eq!(OPERATIONS[1].full_path(), "/v1/issues/{issue_ref}");
    assert_eq!(OPERATIONS[1].axum_path(), "/v1/issues/:issue_ref");
    // 未闭合的花括号：原样保留（宁可挂载时报路径不存在，也不要吞掉一段）。
    assert_eq!(axum_path("/v1/{oops"), "/v1/{oops");
}

#[test]
fn issue_dto_keeps_nulls_and_omits_the_empty_status_category() {
    let issue = Issue {
        id: "issue_1".into(),
        workspace_id: "ws_1".into(),
        number: 7,
        identifier: "MUL-7".into(),
        title: "t".into(),
        description: None,
        status: "todo".into(),
        status_category: String::new(),
        priority: "medium".into(),
        assignee_type: None,
        assignee_id: None,
        creator_type: "member".into(),
        creator_id: "user_1".into(),
        parent_issue_id: None,
        project_id: None,
        position: 1.5,
        stage: None,
        start_date: None,
        due_date: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        revision: 3,
        last_activity_at: None,
        metadata: BTreeMap::new(),
        properties: BTreeMap::new(),
    };
    let value = serde_json::to_value(&issue).expect("serialize issue");
    let object = value.as_object().expect("object");
    assert!(!object.contains_key("status_category"), "omitempty");
    assert_eq!(object.len(), 24);
    assert_eq!(
        value["description"],
        Value::Null,
        "Go 的 *string 空值序列化成 null"
    );
    assert_eq!(value["metadata"], json!({}), "nil map 的零值形态（偏差 2）");
    assert_eq!(value["properties"], json!({}));
    assert_eq!(value["revision"], 3);

    let with_category = Issue {
        status_category: "in_progress".into(),
        ..issue
    };
    let value = serde_json::to_value(&with_category).expect("serialize issue");
    assert_eq!(value["status_category"], "in_progress");
    assert_eq!(value.as_object().expect("object").len(), 25);
}

#[test]
fn comment_dto_omits_the_empty_parent_and_tombstone_stamp() {
    let live = Comment {
        id: "cmt_1".into(),
        author_type: "member".into(),
        author_id: "user_1".into(),
        content: "hi".into(),
        comment_type: "comment".into(),
        parent_id: String::new(),
        created_at: "2026-01-01T00:00:00Z".into(),
        deleted_at: String::new(),
    };
    let value = serde_json::to_value(&live).expect("serialize comment");
    assert_eq!(
        value,
        json!({
            "id": "cmt_1",
            "author_type": "member",
            "author_id": "user_1",
            "content": "hi",
            "type": "comment",
            "created_at": "2026-01-01T00:00:00Z",
        })
    );

    // 墓碑：有回复的评论被删时保留行、内容清空、盖 deleted_at。
    let tombstone = Comment {
        content: String::new(),
        deleted_at: "2026-01-02T00:00:00Z".into(),
        ..live
    };
    let value = serde_json::to_value(&tombstone).expect("serialize comment");
    assert_eq!(value["content"], "");
    assert_eq!(value["deleted_at"], "2026-01-02T00:00:00Z");
    assert!(value.as_object().expect("object").contains_key("content"));
}

#[test]
fn request_dtos_omit_unset_fields_and_default_missing_containers() {
    let patch = PatchIssueRequest {
        expected_revision: Some(4),
        title: Some("new".into()),
        description: None,
    };
    assert_eq!(
        serde_json::to_value(&patch).expect("serialize patch"),
        json!({"expected_revision": 4, "title": "new"})
    );
    let decoded: PatchIssueRequest =
        serde_json::from_value(json!({"title": "new"})).expect("deserialize patch");
    assert_eq!(
        decoded,
        PatchIssueRequest {
            expected_revision: None,
            title: Some("new".into()),
            description: None,
        }
    );

    let create: CreateCommentRequest =
        serde_json::from_value(json!({"content": "hi"})).expect("deserialize create");
    assert_eq!(create.parent_id, None);
    assert_eq!(
        serde_json::to_value(&create).expect("serialize create"),
        json!({"content": "hi"})
    );

    // Context：缺失的 map / slice 走 default（Go 的 nil 零值），actor 必填。
    let context: Context = serde_json::from_value(json!({
        "workspace": {"id": "ws_1", "name": "W", "slug": "w"},
        "actor": "plugin",
    }))
    .expect("deserialize context");
    assert_eq!(context.user, None);
    assert!(context.config.is_empty());
    assert!(context.granted_net_domains.is_empty());

    let full = Context {
        workspace: ContextWorkspace {
            id: "ws_1".into(),
            name: "W".into(),
            slug: "w".into(),
        },
        user: Some(ContextUser {
            id: "user_1".into(),
            name: "U".into(),
        }),
        issue: Some(ContextIssue {
            id: "issue_1".into(),
            identifier: "MUL-1".into(),
            title: "t".into(),
        }),
        config: BTreeMap::from([("key".to_string(), json!("value"))]),
        granted_net_domains: vec!["api.example.com".into()],
        actor: "plugin".into(),
    };
    let value = serde_json::to_value(&full).expect("serialize context");
    assert_eq!(value["config"]["key"], "value");
    assert_eq!(value["granted_net_domains"], json!(["api.example.com"]));
    assert_eq!(value["issue"]["identifier"], "MUL-1");
}

#[test]
fn storage_dtos_round_trip() {
    let list = StorageKeyListResponse {
        keys: vec![StorageKey {
            key: "a/b".into(),
            size_bytes: 12,
            updated_at: "2026-01-01T00:00:00Z".into(),
        }],
    };
    let value = serde_json::to_value(&list).expect("serialize keys");
    assert_eq!(
        value,
        json!({"keys": [{"key": "a/b", "size_bytes": 12, "updated_at": "2026-01-01T00:00:00Z"}]})
    );
    let decoded: StorageKeyListResponse = serde_json::from_value(value).expect("deserialize keys");
    assert_eq!(decoded, list);

    let put: PutStorageValueRequest =
        serde_json::from_value(json!({"value": "v"})).expect("deserialize put");
    assert_eq!(put.value, "v");
    let got = serde_json::to_value(StorageValueResponse { value: "v".into() })
        .expect("serialize value response");
    assert_eq!(got, json!({"value": "v"}));

    let page = PageInfo {
        next_cursor: "opaque".into(),
    };
    assert_eq!(
        serde_json::to_value(&page).expect("serialize page"),
        json!({"next_cursor": "opaque"})
    );
    assert_eq!(
        serde_json::to_value(PageInfo::default()).expect("serialize page"),
        json!({}),
        "上游 next_cursor 带 omitempty"
    );
}

#[test]
fn idempotency_and_paging_limits_are_the_upstream_values() {
    assert_eq!(HEADER_IDEMPOTENCY_KEY, "Idempotency-Key");
    assert_eq!(HEADER_IF_MATCH, "If-Match");
    assert_eq!(MAX_IDEMPOTENCY_BYTES, 255);
    assert_eq!(DEFAULT_PAGE_SIZE, 50);
    assert_eq!(MAX_PAGE_SIZE, 200);
}
