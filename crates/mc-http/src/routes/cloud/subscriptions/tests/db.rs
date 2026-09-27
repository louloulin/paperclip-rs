//! M9-2 真库那一半（门 ⑥）：真库 + **离线云侧替身**（`docs/62` §4.2 的替身纪律 ①）。
//!
//! 未设 `MULTICA_TEST_DATABASE_URL` ⇒ 打印跳过并 `return`；**设了但连不上 ⇒ panic**
//! （不许静默假装绿，与 `routes/channels/lark/tests/db.rs` 同款）。
//!
//! 拆出来是门 ⑩（单文件 800 行）的要求，先例 = `docs/32` §30 的 **D10**。

use super::support::*;
use super::*;

struct Seed {
    workspace: Uuid,
    owner: Uuid,
    admin: Uuid,
    member: Uuid,
    outsider: Uuid,
    empty_email: Uuid,
}

/// 一个 workspace + 四个角色用户（owner / admin / member / 外人）+ 一个空邮箱的 owner。
async fn seed(db: &Db) -> Seed {
    async fn new_user(db: &Db, tag: &str, email: &str) -> Uuid {
        sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
            .bind(format!("itest-m92-{tag}"))
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
            .bind(format!("itest-m92-{tag}"))
            .bind(format!("itest-m92-{tag}"))
            .fetch_one(db.pool())
            .await
            .expect("insert workspace");

    let owner = new_user(db, "owner", &format!("itest-m92-owner-{tag}@example.com")).await;
    let admin = new_user(db, "admin", &format!("itest-m92-admin-{tag}@example.com")).await;
    let member = new_user(db, "member", &format!("itest-m92-member-{tag}@example.com")).await;
    let outsider = new_user(db, "out", &format!("itest-m92-out-{tag}@example.com")).await;
    // 上游对空邮箱有专门一条 500（`checkout payer email is unavailable`）。
    //
    // ⚠️ 本地 `"user".email` 有 **UNIQUE** 约束 ⇒「空白邮箱」不能写成固定的 `"   "`
    // （并用例并行跑、每个用例都插一行）⇒ 用**本 workspace 那枚唯一 tag** 映射出一个
    // 32 字符的**全空白**串（空格 / 制表符两种），既满足 `trim().is_empty()`，又在库层面唯一。
    let blank_email: String = tag
        .chars()
        .map(|c| if c < '8' { ' ' } else { '\t' })
        .collect();
    let empty_email = new_user(db, "blank", &blank_email).await;
    for (user, role) in [
        (owner, "owner"),
        (admin, "admin"),
        (member, "member"),
        (empty_email, "owner"),
    ] {
        join(db, workspace, user, role).await;
    }

    Seed {
        workspace,
        owner,
        admin,
        member,
        outsider,
        empty_email,
    }
}

/// 7 条逐条：路由 → 注入的 `X-User-ID` → 云侧路径（含 workspace 段）→ 响应**逐字**透传。
///
/// 同时是「本地 0 张表」的结构性证据（`docs/62` §9.4 双侧实测）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn all_seven_routes_proxy_verbatim_and_stamp_the_identity() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone(), cloud_at(&stub_base()), true);
    let ws = seed.workspace.to_string();

    for (method, local, upstream) in SEVEN {
        // 客户端查询串**不**透传（上游 `proxyCloudSubscription` 没有 `withQuery` 那一支）
        // ⇒ 下面那条 `target` 逐字相等就是它的判据。
        // 头键给上（portal 那条**必需**它；checkout 那条的体键优先，头只影响转发面）。
        let call = Call::new(method, local, seed.owner, seed.workspace)
            .maybe_body(body_for(local))
            .key("wire-1")
            .query("debug=1");
        let (status, headers, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::OK, "{method} {local}");
        let outbound = calls(&call.request_id);
        assert_eq!(outbound.len(), 1, "{method} {local}");
        // 云侧收到的**字节数**逐字回显：checkout 那一笔是**注入体**（不是客户端体），
        // 所以判据取替身实际收到的长度（注入体的形状在专门的用例里断言）。
        assert_eq!(
            bytes,
            json!({
                "ok": upstream.replace("{ws}", &ws),
                "method": method,
                "body_len": outbound[0].body.len(),
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
            "{local}：云侧的 X-Request-ID 回写"
        );
        assert_eq!(outbound[0].method, method);
        assert_eq!(
            outbound[0].target,
            upstream.replace("{ws}", &ws),
            "云侧路径逐字（workspace 段来自本仓解析），且查询串被丢掉"
        );
        assert_eq!(
            outbound[0].user_id.as_deref(),
            Some(seed.owner.to_string().as_str()),
            "注入的 X-User-ID 必须是会话身份"
        );
        // 带体的三条声明 JSON；不带的四条一个 `Content-Type` 都不设。
        assert_eq!(
            outbound[0].content_type.as_deref(),
            call.body.is_some().then_some("application/json"),
            "{method} {local} 的 Content-Type"
        );
    }

    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' \
         AND (table_name LIKE 'cloud_subscription%' OR table_name LIKE 'subscription_seat%')",
    )
    .fetch_one(db.pool())
    .await
    .expect("information_schema");
    assert_eq!(tables, 0, "subscriptions 面本地 0 张表（纯代理）");
}

/// 🔴 `DoD` 第 2/4 条：客户端走私的 `workspace_id` / `target_seats` / 付款人邮箱被丢掉，
/// 权威 workspace 与三个并发字段**逐字**进体。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_authoritative_workspace_is_injected_and_client_values_are_dropped() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db, cloud_at(&stub_base()), true);
    let smuggled = "00000000-0000-0000-0000-000000000002";
    let ws = seed.workspace.to_string();

    // ① checkout：体里的 workspace / customer_email 都是走私值，两个都被覆盖。
    let call = Call::new("POST", CHECKOUT, seed.owner, seed.workspace).body(
        json!({
            "workspace_id": smuggled,
            "interval": "year",
            "idempotency_key": "checkout-1",
            "customer_email": "attacker@example.com",
        })
        .to_string(),
    );
    let (status, _, _) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    let outbound = calls(&call.request_id);
    assert_eq!(
        outbound[0].target,
        "/api/v1/subscriptions/checkout-sessions"
    );
    let forwarded: Value = serde_json::from_str(&outbound[0].body).expect("json");
    assert_eq!(forwarded["workspace_id"], ws);
    assert_eq!(forwarded["interval"], "year");
    assert_eq!(forwarded["idempotency_key"], "checkout-1");
    let payer = forwarded["customer_email"].as_str().unwrap_or_default();
    assert!(payer.starts_with("itest-m92-owner-"), "{payer}");
    assert_ne!(payer, "attacker@example.com", "付款人身份由服务端解析");

    // ② 预览：只转发 `additional_seats`（客户端多传的两个字段都不出现）。
    let call = Call::new("POST", PREVIEW, seed.owner, seed.workspace).body(
        json!({
            "additional_seats": 10001,
            "workspace_id": smuggled,
            "target_seats": 999,
        })
        .to_string(),
    );
    let (status, _, _) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    let outbound = calls(&call.request_id);
    assert_eq!(
        outbound[0].target,
        format!("/api/v1/subscriptions/{ws}/seats/purchase-preview")
    );
    assert_eq!(outbound[0].body, r#"{"additional_seats":10001}"#);
    assert!(!outbound[0].body.contains(smuggled));
    assert!(!outbound[0].body.contains("target_seats"));

    // ③ 购买：三件套逐字 + 币种小写化 + 幂等键换成解析出来的那个。
    let call = Call::new("POST", PURCHASES, seed.owner, seed.workspace).body(
        json!({
            "workspace_id": smuggled,
            "target_seats": 999,
            "additional_seats": 2,
            "expected_current_seats": 5,
            "expected_purchase_version": 41,
            "accepted_proration_amount": 425,
            "currency": "USD",
            "idempotency_key": "seat-1",
        })
        .to_string(),
    );
    let (status, _, _) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    let outbound = calls(&call.request_id);
    assert_eq!(
        outbound[0].target,
        format!("/api/v1/subscriptions/{ws}/seats/purchases")
    );
    let forwarded: Value = serde_json::from_str(&outbound[0].body).expect("json");
    assert_eq!(
        forwarded,
        json!({
            "additional_seats": 2,
            "expected_current_seats": 5,
            "expected_purchase_version": 41,
            "accepted_proration_amount": 425,
            "currency": "usd",
            "idempotency_key": "seat-1",
        })
    );
    assert!(!outbound[0].body.contains("target_seats"));
}

/// 角色矩阵（`DoD` 第 1 条）：读 2 条 member 可读；写 5 条非 `owner|admin` ⇒ 403；
/// 非成员 ⇒ 404。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_role_matrix_holds_for_reads_and_writes() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db, cloud_at(&stub_base()), true);

    for local in [SUMMARY, PRICES] {
        let call = Call::new("GET", local, seed.member, seed.workspace);
        let (status, _, _) = send(&app, &call).await;
        assert_eq!(status, StatusCode::OK, "member 可读 {local}");
        assert_eq!(calls(&call.request_id).len(), 1, "{local}");
    }

    for (method, local, _) in FIVE_WRITES {
        // member 写 ⇒ 403，且不发出站。
        let call = Call::new(method, local, seed.member, seed.workspace)
            .maybe_body(body_for(local))
            .key("matrix-1");
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "member 不得写 {local}");
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );
        assert_eq!(error_of(&bytes)["code"], "forbidden");

        // owner / admin 写 ⇒ 200（幂等键给上：portal 那条**必需**它）。
        for actor in [seed.owner, seed.admin] {
            let call = Call::new(method, local, actor, seed.workspace)
                .maybe_body(body_for(local))
                .key("matrix-1");
            let (status, _, _) = send(&app, &call).await;
            assert_eq!(status, StatusCode::OK, "{method} {local}");
            assert_eq!(calls(&call.request_id).len(), 1, "{local}");
        }
    }

    // 非成员 ⇒ 404（隐藏 workspace 存在性；本仓 member 口径）。
    for local in [SUMMARY, PRICES] {
        let call = Call::new("GET", local, seed.outsider, seed.workspace);
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{local}");
        assert_eq!(error_of(&bytes)["code"], "not_found");
        assert!(calls(&call.request_id).is_empty(), "{local}");
    }
    let call = Call::new("POST", PORTAL, seed.outsider, seed.workspace);
    let (status, _, _) = send(&app, &call).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "非成员写面");
    assert!(calls(&call.request_id).is_empty());
}

/// `DoD` 第 3 条：两档上限（255 / 200）+ 缺失 400 + **转发规则**的两处不对称。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_two_idempotency_key_tiers_and_the_forwarding_rules_hold() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db, cloud_at(&stub_base()), true);

    // ① 缺失 ⇒ 400（checkout 体与头都没有；portal 头没有）。
    for (local, body) in [
        (CHECKOUT, Some(json!({"interval": "month"}).to_string())),
        (PORTAL, None),
    ] {
        let call = Call::new("POST", local, seed.owner, seed.workspace)
            .maybe_body(body.map(String::into_bytes));
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{local}");
        assert_eq!(error_of(&bytes)["message"], MSG_KEY_REQUIRED, "{local}");
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );
    }

    // ② 255 那一档：256 字节 ⇒ 400；255 字节 ⇒ 200 且头转发的是 trim 后的那个键。
    //
    // ⚠️ checkout 的体**不能**带 `idempotency_key`（体键优先 ⇒ 头那 256 字节就轮不到判定）。
    let bare_checkout = Some(json!({"interval": "month"}).to_string().into_bytes());
    for (local, body, len, expected) in [
        (
            CHECKOUT,
            bare_checkout.clone(),
            256,
            StatusCode::BAD_REQUEST,
        ),
        (CHECKOUT, bare_checkout, 255, StatusCode::OK),
        (PORTAL, None, 256, StatusCode::BAD_REQUEST),
        (PORTAL, None, 255, StatusCode::OK),
    ] {
        let key = "a".repeat(len);
        let call = Call::new("POST", local, seed.owner, seed.workspace)
            .key(key.clone())
            .maybe_body(body);
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, expected, "{local} key len {len}");
        if expected == StatusCode::BAD_REQUEST {
            assert_eq!(error_of(&bytes)["message"], MSG_KEY_TOO_LONG, "{local}");
            assert!(calls(&call.request_id).is_empty(), "{local}");
        } else {
            let outbound = calls(&call.request_id);
            assert_eq!(outbound[0].idempotency_key.as_deref(), Some(key.as_str()));
        }
    }

    // ③ 第二档：201 字节在 checkout 合法、在座位购买 ⇒ 400「200 bytes」。
    let seat_key = "a".repeat(201);
    let call = Call::new("POST", CHECKOUT, seed.owner, seed.workspace)
        .key(seat_key.clone())
        .maybe_body(body_for(CHECKOUT));
    let (status, _, _) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK, "201 字节在 255 档下合法");

    // 体里给一个**空**的 `idempotency_key` ⇒ 按上游语义落到头键（顺带钉住这条回退）。
    let call = Call::new("POST", PURCHASES, seed.owner, seed.workspace)
        .key(seat_key)
        .body(seat_purchase(""));
    let (status, _, bytes) = send(&app, &call).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "201 字节在 200 档下超限");
    assert_eq!(error_of(&bytes)["message"], MSG_SEAT_KEY_TOO_LONG);
    assert!(calls(&call.request_id).is_empty());

    // ④ 转发规则：checkout 只给**体键** ⇒ 头不转发；只给**头键** ⇒ 头转发且体里也是它。
    let call = Call::new("POST", CHECKOUT, seed.owner, seed.workspace)
        .body(json!({"interval": "month", "idempotency_key": "body-only"}).to_string());
    let (status, _, _) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    let outbound = calls(&call.request_id);
    assert_eq!(outbound[0].idempotency_key, None, "体键不进请求头");
    let forwarded: Value = serde_json::from_str(&outbound[0].body).expect("json");
    assert_eq!(forwarded["idempotency_key"], "body-only");

    let call = Call::new("POST", CHECKOUT, seed.owner, seed.workspace)
        .key("header-only")
        .body(json!({"interval": "month"}).to_string());
    let (status, _, _) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    let outbound = calls(&call.request_id);
    assert_eq!(outbound[0].idempotency_key.as_deref(), Some("header-only"));
    let forwarded: Value = serde_json::from_str(&outbound[0].body).expect("json");
    assert_eq!(forwarded["idempotency_key"], "header-only", "头键也进体");

    // ⑤ 购买：**体键**会走到请求头上（与 checkout 相反）。
    let call =
        Call::new("POST", PURCHASES, seed.owner, seed.workspace).body(seat_purchase("from-body"));
    let (status, _, _) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        calls(&call.request_id)[0].idempotency_key.as_deref(),
        Some("from-body")
    );

    // ⑥ reconcile / preview：上游既不要求、也不转发（客户端给了也不转发）。
    for local in [RECONCILE, PREVIEW] {
        let call = Call::new("POST", local, seed.owner, seed.workspace)
            .key("ignored-key")
            .maybe_body(body_for(local));
        let (status, _, _) = send(&app, &call).await;
        assert_eq!(status, StatusCode::OK, "{local}");
        let outbound = calls(&call.request_id);
        assert_eq!(outbound[0].idempotency_key, None, "{local} 不转发幂等键");
    }
    assert!(calls(&Call::new("POST", RECONCILE, seed.owner, seed.workspace).request_id).is_empty());
}

/// 本地校验：非法请求 ⇒ 400 且**不发出站**（上游逐字的那几条文本 + 体上限的三条）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn local_validation_rejects_before_any_outbound_request() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db, cloud_at(&stub_base()), true);

    // 本地校验：全部 400 且**不**发出站。
    for (local, body, message) in [
        (
            CHECKOUT,
            json!({"interval": "weekly", "idempotency_key": "k"}),
            MSG_INTERVAL_INVALID,
        ),
        (
            CHECKOUT,
            json!({"interval": 5, "idempotency_key": "k"}),
            MSG_BODY_INVALID,
        ),
        (
            CHECKOUT,
            json!({"interval": "month", "idempotency_key": 5}),
            MSG_BODY_INVALID,
        ),
        (
            PREVIEW,
            json!({"additional_seats": 0}),
            MSG_SEATS_NOT_POSITIVE,
        ),
        (
            PREVIEW,
            json!({"additional_seats": "two"}),
            MSG_BODY_INVALID,
        ),
        (PREVIEW, json!({}), MSG_BODY_INVALID),
        (
            PURCHASES,
            json!({
                "additional_seats": 2, "expected_current_seats": 0,
                "expected_purchase_version": 1, "accepted_proration_amount": 0,
                "currency": "usd", "idempotency_key": "k",
            }),
            MSG_SEAT_PURCHASE_INVALID,
        ),
        (
            PURCHASES,
            json!({
                "additional_seats": 2, "expected_current_seats": 5,
                "expected_purchase_version": 1, "accepted_proration_amount": 0,
                "currency": "u$d", "idempotency_key": "k",
            }),
            MSG_SEAT_PURCHASE_INVALID,
        ),
    ] {
        let call = Call::new("POST", local, seed.owner, seed.workspace).body(body.to_string());
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{local} {body}");
        assert_eq!(error_of(&bytes)["message"], message, "{local} {body}");
        assert!(
            calls(&call.request_id).is_empty(),
            "{local} 不得产生出站请求"
        );
    }
    // 空体 / 非法 JSON / 超限体 ⇒ 上游 `readCloudRuntimeJSONBody` 的三条。
    for (body, expected, message) in [
        ("", StatusCode::BAD_REQUEST, MSG_BODY_REQUIRED),
        ("   ", StatusCode::BAD_REQUEST, MSG_BODY_REQUIRED),
        ("{oops", StatusCode::BAD_REQUEST, MSG_BODY_INVALID),
        (
            &"x".repeat(MAX_CLOUD_REQUEST_BODY + 1),
            StatusCode::PAYLOAD_TOO_LARGE,
            MSG_BODY_TOO_LARGE,
        ),
    ] {
        let call = Call::new("POST", CHECKOUT, seed.owner, seed.workspace).body(body.as_bytes());
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, expected, "{message}");
        assert_eq!(error_of(&bytes)["message"], message);
        assert!(calls(&call.request_id).is_empty(), "{message} ⇒ 不得发出站");
    }
}

/// 云侧响应与错误路径：4xx/5xx 与奇怪体**逐字**透传；502 / 超限体 / 未配置 / 非法配置
/// 的错误体**不回显**基址与云侧体；付款人邮箱缺失 ⇒ 上游那条 500。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn cloud_responses_are_passed_through_and_error_paths_never_echo() {
    let db = fixture!();
    let seed = seed(&db).await;
    let ws = seed.workspace.to_string();
    let app = test_app(db.clone(), cloud_at(&stub_base()), true);

    // ① 云侧 4xx/5xx 与奇怪体**逐字**透传。
    for (variant, expected, payload) in [
        ("status402", StatusCode::PAYMENT_REQUIRED, PAYLOAD_402),
        ("status500", StatusCode::INTERNAL_SERVER_ERROR, PAYLOAD_500),
    ] {
        let call = Call::new("GET", SUMMARY, seed.member, seed.workspace).variant(variant);
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, expected, "{variant}");
        assert_eq!(bytes, payload.as_bytes(), "{variant} 体逐字");
    }
    let call = Call::new("GET", SUMMARY, seed.member, seed.workspace).variant("notjson");
    let (status, headers, bytes) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        format!("not-json-marker:/api/v1/subscriptions/{ws}/summary")
    );
    assert!(headers.get(CONTENT_TYPE).is_none(), "非 JSON ⇒ 不声明 JSON");
    let call = Call::new("GET", PRICES, seed.member, seed.workspace).variant("blank");
    let (status, _, bytes) = send(&app, &call).await;
    assert_eq!(status, StatusCode::OK);
    assert!(bytes.is_empty(), "全空白体 ⇒ 无体");

    // ③ 502 / 超限体：错误体不回显基址、也不回显云侧体（`docs/62` §2.4 判据 ③）。
    let dead = dead_base();
    let app_dead = test_app(db.clone(), cloud_at(&dead), true);
    let call = Call::new("GET", SUMMARY, seed.member, seed.workspace);
    let (status, _, bytes) = send(&app_dead, &call).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_of(&bytes)["code"], CODE_FAILED);
    assert!(
        !String::from_utf8_lossy(&bytes).contains(&dead),
        "不得回显基址"
    );

    let call = Call::new("GET", SUMMARY, seed.member, seed.workspace).variant("huge");
    let (status, _, bytes) = send(&app, &call).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        !String::from_utf8_lossy(&bytes).contains(HUGE_MARKER),
        "不得回显云侧体"
    );

    // ④ 未配置 ⇒ 403 且不发出站；配置非法 ⇒ 500。
    for (cloud, expected, code) in [
        (cloud_disabled(), StatusCode::FORBIDDEN, CODE_NOT_CONFIGURED),
        (
            CloudConfig::from_env_with(|name| {
                (name == mc_cloud::CLOUD_URL_ENV).then(|| "https://u:p@cloud.test".to_string())
            }),
            StatusCode::INTERNAL_SERVER_ERROR,
            CODE_MISCONFIGURED,
        ),
    ] {
        let app = test_app(db.clone(), cloud, true);
        let call = Call::new("GET", SUMMARY, seed.member, seed.workspace);
        let (status, _, bytes) = send(&app, &call).await;
        assert_eq!(status, expected);
        assert_eq!(error_of(&bytes)["code"], code);
        assert!(calls(&call.request_id).is_empty(), "不得产生出站请求");
    }

    // ⑤ 付款人邮箱为空 ⇒ 上游那条 500，且**不**发出站。
    let call = Call::new("POST", CHECKOUT, seed.empty_email, seed.workspace)
        .body(json!({"interval": "month", "idempotency_key": "k"}).to_string());
    let (status, _, bytes) = send(&app, &call).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error_of(&bytes)["message"], MSG_PAYER_EMAIL_UNAVAILABLE);
    assert!(calls(&call.request_id).is_empty(), "不得产生出站请求");
}
