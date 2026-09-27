//! M9-11 真库那一半（门 ⑥）：真库 + **离线云侧替身**（`docs/62` §4.2 的替身纪律 ①）。
//!
//! 未设 `MULTICA_TEST_DATABASE_URL` ⇒ 打印跳过并 `return`；**设了但连不上 ⇒ panic**
//! （不许静默假装绿，与 `routes/cloud/subscriptions/tests/db.rs` 同款）。
//!
//! 拆出来是门 ⑩（单文件 800 行）的要求，先例 = `docs/32` §30 的 **D10**。

use super::support::*;
use super::*;

struct Seed {
    workspace: Uuid,
    owner: Uuid,
    member: Uuid,
    outsider: Uuid,
}

/// 一个 workspace + 三个用户（owner / member / 外人）。
async fn seed(db: &Db) -> Seed {
    async fn new_user(db: &Db, tag: &str, email: &str) -> Uuid {
        sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
            .bind(format!("itest-m911-{tag}"))
            .bind(email.to_string())
            .fetch_one(db.pool())
            .await
            .expect("insert user")
    }
    async fn join(db: &Db, workspace: Uuid, user: Uuid, role: &str) {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(workspace)
            .bind(user)
            .bind(role)
            .execute(db.pool())
            .await
            .expect("insert member");
    }

    let tag = Uuid::new_v4().simple().to_string();
    let workspace: Uuid =
        sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
            .bind(format!("itest-m911-{tag}"))
            .bind(format!("itest-m911-{tag}"))
            .fetch_one(db.pool())
            .await
            .expect("insert workspace");

    let owner = new_user(db, "owner", &format!("itest-m911-owner-{tag}@example.com")).await;
    let member = new_user(
        db,
        "member",
        &format!("itest-m911-member-{tag}@example.com"),
    )
    .await;
    let outsider = new_user(db, "out", &format!("itest-m911-out-{tag}@example.com")).await;
    for (user, role) in [(owner, "owner"), (member, "member")] {
        join(db, workspace, user, role).await;
    }

    Seed {
        workspace,
        owner,
        member,
        outsider,
    }
}

/// 11 条逐条：路由 → 注入的 `X-User-ID` → 云侧路径 → 响应**逐字**透传。
///
/// 同时是「本地 0 张节点池表」的结构性证据（`docs/62` §9.4 双侧实测）：请求体被逐字送到
/// 云侧，而出站查询串 / 头逐字可见。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn all_eleven_routes_proxy_verbatim_and_stamp_the_identity() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()));

    for (method, local, upstream, with_body, stamps_user) in ELEVEN {
        let caller = seed.member;
        let call =
            Call::new(method, local, caller, seed.workspace).maybe_body(body_for(method, local));
        let (status, headers, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::OK, "{method} {local}");

        let expected_body = body_for(method, local).unwrap_or_default();
        assert_eq!(
            bytes,
            json!({
                "ok": upstream,
                "method": method,
                "body_len": expected_body.len(),
            })
            .to_string()
            .as_bytes(),
            "{method} {local} 响应逐字"
        );
        assert_eq!(
            headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "{local}"
        );
        assert_eq!(
            headers.get("x-request-id").and_then(|v| v.to_str().ok()),
            Some(format!("echoed-{}", call.request_id).as_str()),
            "{local}：云侧的 X-Request-ID 回写（上游 writeCloudRuntimeResponse）"
        );

        let outbound = calls(&call.request_id);
        assert_eq!(outbound.len(), 1, "{method} {local}");
        assert_eq!(outbound[0].method, method, "{method} {local}");
        assert_eq!(
            outbound[0].target, upstream,
            "{method} {local} 出站路径逐字"
        );
        // `withUserID`：两条探针**不**盖章，其余 9 条盖章会话身份。
        let caller_text = caller.to_string();
        assert_eq!(
            outbound[0].user_id.as_deref(),
            if stamps_user {
                Some(caller_text.as_str())
            } else {
                None
            },
            "{method} {local} withUserID"
        );
        // `withBody`：7 条带体（逐字、不重排），4 条不带。
        assert_eq!(
            outbound[0].body.as_str(),
            String::from_utf8_lossy(&expected_body)
        );
        // `Content-Type` 只在有体时挂（上游 `if len(req.Body) > 0`）。
        assert_eq!(
            outbound[0].content_type.as_deref(),
            with_body.then_some("application/json"),
            "{method} {local} Content-Type"
        );
        // `Accept` 11 条都有（上游 `httpReq.Header.Set("Accept", …)` 无条件）。
        assert_eq!(
            outbound[0].accept.as_deref(),
            Some("application/json"),
            "{local}"
        );
    }
}

/// `GET /api/cloud-runtime/nodes` 是 11 条里**唯一**带 query 的：带 query 的透传，
/// 其余 10 条**丢掉**（上游 `withQuery` 只在那一条打开）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn query_strings_are_forwarded_only_where_upstream_turns_that_on() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()));

    for (method, local, upstream, with_query) in [
        ("GET", "/api/cloud-runtime/nodes", "/api/v1/nodes", true),
        ("GET", "/api/cloud-runtime/", "/api/v1/", false),
        ("GET", "/api/cloud-runtime/healthz", "/healthz", false),
        ("GET", "/api/cloud-runtime/readyz", "/readyz", false),
        (
            "POST",
            "/api/cloud-runtime/nodes/exec",
            "/api/v1/nodes/exec",
            false,
        ),
    ] {
        let call = Call::new(method, local, seed.member, seed.workspace)
            .maybe_body(body_for(method, local))
            .query("limit=10&tag=gpu&tag=arm");
        let (status, _, _) = send(&app, &call).await;
        assert_eq!(status, StatusCode::OK, "{method} {local}");
        let expected = if with_query {
            format!("{upstream}?limit=10&tag=gpu&tag=arm")
        } else {
            upstream.to_string()
        };
        assert_eq!(
            calls(&call.request_id)[0].target,
            expected,
            "{method} {local}"
        );
    }
}

/// 体**逐字**转发（7 条）：缩进 / 重复键 / unicode 都原样到达 —— 云侧按字节签这条载荷。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_seven_bodies_are_forwarded_byte_for_byte() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()));
    let raw = "{  \"node_id\":\"n-1\",  \"tags\":[\"a\",\"b\"], \"note\":\"café\"}"
        .as_bytes()
        .to_vec();

    for (method, local, upstream) in SEVEN_BODIES {
        let call = Call::new(method, local, seed.member, seed.workspace).body(raw.clone());
        let (status, _, _) = send(&app, &call).await;
        assert_eq!(status, StatusCode::OK, "{method} {local}");
        let outbound = calls(&call.request_id);
        assert_eq!(outbound.len(), 1, "{method} {local}");
        assert_eq!(outbound[0].target, upstream, "{method} {local}");
        assert_eq!(
            outbound[0].body.as_bytes(),
            raw,
            "{method} {local}：体必须逐字（不 trim / 不重排 / 不补默认字段）"
        );
    }
}

/// 体读取的三道判定 × 7 条（上游 `readCloudRuntimeJSONBody`），且**零出站**。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_three_body_gates_reject_before_any_outbound_call() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()));

    for (method, local, _) in SEVEN_BODIES {
        // ① 空体 ⇒ 400。
        let call = Call::new(method, local, seed.member, seed.workspace).body(Vec::new());
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {local} 空体");
        assert_eq!(error_of(&bytes)["message"], MSG_BODY_REQUIRED, "{local}");
        assert!(calls(&call.request_id).is_empty(), "{local} 不得出站");

        // ② JSON 语法错 ⇒ 400。
        let call = Call::new(method, local, seed.member, seed.workspace).body("{oops");
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{method} {local} 非法 JSON"
        );
        assert_eq!(error_of(&bytes)["message"], MSG_BODY_INVALID, "{local}");
        assert!(calls(&call.request_id).is_empty(), "{local} 不得出站");

        // ③ 超 1 MiB ⇒ 413。
        let call = Call::new(method, local, seed.member, seed.workspace).body(vec![
            b'x';
            MAX_CLOUD_REQUEST_BODY
                + 1
        ]);
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{method} {local} 超限"
        );
        assert_eq!(error_of(&bytes)["message"], MSG_BODY_TOO_LARGE, "{local}");
        assert!(calls(&call.request_id).is_empty(), "{local} 不得出站");
    }
}

/// 角色矩阵：**owner 与 member 都能调**（上游这一簇是 member 面），外人 ⇒ **404**。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_member_gate_admits_members_and_hides_the_workspace_from_outsiders() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()));

    for (method, local, _, _, _) in ELEVEN {
        // ① owner ⇒ 200（owner 是 member 的超集）。
        for caller in [seed.owner, seed.member] {
            let call = Call::new(method, local, caller, seed.workspace)
                .maybe_body(body_for(method, local));
            let (status, _, _) = send(&app, &call).await;
            assert_eq!(status, StatusCode::OK, "{method} {local} as member");
            assert_eq!(calls(&call.request_id).len(), 1, "{local} 应当出站一次");
        }

        // ② 外人 ⇒ 404 `workspace`（隐藏资源存在性，与全仓 member 口径一致）+ **零出站**。
        let call = Call::new(method, local, seed.outsider, seed.workspace)
            .maybe_body(body_for(method, local));
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {local} as outsider"
        );
        // 本仓的 404 信封：`code = "not_found"`，资源名在 message 里
        // （`Error::NotFound { resource: "workspace" }` ⇒ `not found: workspace`）。
        // 与 §47 的 M9-2 同款断言（它也踩过这个 code 名）。
        assert_eq!(error_of(&bytes)["code"], "not_found", "{local}");
        assert!(
            error_of(&bytes)["message"]
                .as_str()
                .unwrap_or_default()
                .contains("workspace"),
            "{local}：404 必须指向 workspace（隐藏资源存在性）"
        );
        assert!(
            calls(&call.request_id).is_empty(),
            "{local}：非成员不得产生出站请求"
        );
    }
}

/// 🔴 机器凭据 + **真成员** ⇒ 200 且照常出站（上游这一簇**没有** `RequireHumanActor`）。
///
/// 这是 `tests.rs` 里那条反向判据的**加强版**：懒库那一半只证「没被 403 拦」，
/// 这一半证「真的走完了整条链」。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn machine_credentials_still_reach_the_fleet_when_they_are_real_members() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()));

    for (method, local, upstream, _, _) in ELEVEN {
        for actor in ["task_token", "cloud_pat"] {
            let call = Call::new(method, local, seed.member, seed.workspace)
                .maybe_body(body_for(method, local))
                .machine(actor);
            let (status, _, bytes) = send(&app, &call).await;
            assert_eq!(status, StatusCode::OK, "{actor}: {method} {local}");
            assert!(
                !String::from_utf8_lossy(&bytes).contains(HUMAN_ACTOR_REQUIRED_MESSAGE),
                "{actor}: {local} 不得被本片拦"
            );
            assert_eq!(
                calls(&call.request_id)[0].target,
                upstream,
                "{actor}: {local}"
            );
        }
    }
}

/// 云侧的 4xx / 5xx **不是错误**：`status` 与体**逐字**回写（上游 `doInner` 的语义）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn cloud_side_errors_are_passed_through_verbatim() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()));

    for (variant, expected, payload) in [
        ("status402", StatusCode::PAYMENT_REQUIRED, PAYLOAD_402),
        ("status500", StatusCode::INTERNAL_SERVER_ERROR, PAYLOAD_500),
    ] {
        let call = Call::new(
            "GET",
            "/api/cloud-runtime/nodes",
            seed.member,
            seed.workspace,
        )
        .variant(variant);
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, expected, "{variant}");
        assert_eq!(bytes, payload.as_bytes(), "{variant}：云侧体逐字");
    }

    // 体不是合法 JSON ⇒ **原样**转发（本地偏离上游的"包进错误信封"，见模块头）。
    let call = Call::new(
        "GET",
        "/api/cloud-runtime/nodes",
        seed.member,
        seed.workspace,
    )
    .variant("notjson");
    let (status, _, bytes) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"not-json-marker:/api/v1/nodes");

    // 全空白体 ⇒ 无体（上游 `len(body) == 0` 那一支）。
    let call = Call::new(
        "GET",
        "/api/cloud-runtime/nodes",
        seed.member,
        seed.workspace,
    )
    .variant("blank");
    let (status, headers, bytes) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    assert!(bytes.is_empty(), "全空白体 ⇒ 无体");
    assert_eq!(headers.get(CONTENT_TYPE), None, "无体 ⇒ 不补 Content-Type");
}

/// > 1 MiB 的**响应**体 ⇒ 502 且**不回显**云侧体（`docs/62` §2.4 判据 ③）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn an_oversized_cloud_response_becomes_502_without_echoing_it() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()));

    let call = Call::new(
        "GET",
        "/api/cloud-runtime/nodes",
        seed.member,
        seed.workspace,
    )
    .variant("huge");
    let (status, _, bytes) = send(&app, &call).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_of(&bytes)["code"], CODE_FAILED);
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains(HUGE_MARKER), "不得回显云侧体：{text}");
}

/// 未配置 ⇒ 403 `cloud_runtime_not_configured`，且**零出站**（上游 `writeFeatureDisabled`）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn an_unconfigured_cloud_url_is_403_for_all_eleven_routes() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_disabled());

    for (method, local, _, _, _) in ELEVEN {
        let call = Call::new(method, local, seed.member, seed.workspace)
            .maybe_body(body_for(method, local));
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {local}");
        assert_eq!(error_of(&bytes)["code"], CODE_NOT_CONFIGURED, "{local}");
        assert_eq!(error_of(&bytes)["message"], MSG_NOT_CONFIGURED, "{local}");
        assert!(
            calls(&call.request_id).is_empty(),
            "{local}：未配置 ⇒ 零出站"
        );
    }
}

/// 配了但非法 ⇒ **500** `cloud_runtime_misconfigured`（上游 `ErrInvalidBaseURL` 那一行）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn a_misconfigured_cloud_url_is_500_for_all_eleven_routes() {
    let db = fixture!();
    let seed = seed(&db).await;
    // userinfo 在基址里 ⇒ `mc_cloud::config::validate` 拒收 ⇒ 客户端建不起来。
    let app = test_app(db.clone(), cloud_at("http://user:pw@127.0.0.1:1"));

    for (method, local, _, _, _) in ELEVEN {
        let call = Call::new(method, local, seed.member, seed.workspace)
            .maybe_body(body_for(method, local));
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{method} {local}"
        );
        assert_eq!(error_of(&bytes)["code"], CODE_MISCONFIGURED, "{local}");
        assert_eq!(error_of(&bytes)["message"], MSG_MISCONFIGURED, "{local}");
        assert!(calls(&call.request_id).is_empty(), "{local}：非法 ⇒ 零出站");
    }
}

/// 连接被拒 ⇒ **502** `upstream_error`（`docs/62` §2.6 的最后一行），且错误体静态。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn an_unreachable_fleet_is_502_for_all_eleven_routes() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&dead_base()));

    for (method, local, _, _, _) in ELEVEN {
        let call = Call::new(method, local, seed.member, seed.workspace)
            .maybe_body(body_for(method, local));
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{method} {local}");
        assert_eq!(error_of(&bytes)["code"], CODE_FAILED, "{local}");
        assert_eq!(error_of(&bytes)["message"], MSG_FAILED, "{local}");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("127.0.0.1"),
            "错误体不得回显出站 URL：{text}"
        );
    }
}
