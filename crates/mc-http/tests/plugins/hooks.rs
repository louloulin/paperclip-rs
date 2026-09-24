//! `POST /api/plugin-bridge/v1/hooks/:key` 端到端测试（M6-8 / `LUM-1673`）。
//!
//! 覆盖 `docs/57` §4.1 给 M6-8 记的**唯一**一条注册键，以及它的两个调用面：
//!
//! | 面 | 用例 |
//! |---|---|
//! | 路由（`ui`/`manual`） | 正常路径（出站失败折 502 + 落一行 `plugin_invocation`）、未知 key 404、`event`/`agent`/`schedule` 400、未声明的触发器 403、`issue_id` 越界 404 |
//! | 凭据（会话面） | 匿名 401、插件令牌 401（`session_required`）、非成员 404、缺安装头 400、安装停用 403 |
//! | 降级 | 部署密钥缺失 503 `plugin_disabled`、`plugins_v1` 关闭 403 `plugin_api_disabled` |
//! | 限流 | 窗口内 ≥120 次尝试 ⇒ 507 `insufficient_storage` |
//! | job 数据面 | `dispatch_scheduled_hook` 一格投递：落 `trigger='schedule'` 行 + 推进 `next_run_at` + 换代判无效 |
//!
//! 出站的成功路径**不在这里**：`mcp.example.com` 解析不到（本仓 M6-6 的同一取舍），
//! 「四个出站头逐字 + 签名字节向量 + 三个反例」是纯函数用例，在
//! `src/routes/plugins/hooks_job.rs` 的 `mod tests` 里（不需要库、不需要网络，也不该等到门 ⑥）。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`），由门 ⑥ 用 `-- --ignored` 拉起。
//!
//! ⚠️ 第 ⑤ 节（job 数据面）在 `hooks_job.rs`：门 ⑩ 的单文件 800 行硬上限逼出来的拆分
//! （与 M6-6 把运行时面拆成 `runtime.rs` / `runtime_surface.rs` 同款）。

use super::runtime_support::*;
use super::support::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use uuid::Uuid;

/// 安装头（`policy::session_caller` 认的那个名字，逐字）。
const INSTALLATION_HEADER: &str = "x-multica-plugin-installation";

/// 桥面 hook 请求：会话（dev-mode 头）+ 安装头。
///
/// `body` 按值收：夹具的每个调用点都传一个临时 `json!({…})`，取引用只会给十几个调用点各添
/// 一个 `&` 而没有任何收益（`needless_pass_by_value` 在这里是噪声）。
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn hook_req(
    uri: &str,
    workspace_id: Uuid,
    user_id: Uuid,
    installation_id: Uuid,
    body: Value,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header(INSTALLATION_HEADER, installation_id.to_string())
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

pub(crate) fn hook_uri(hook_key: &str) -> String {
    format!("/api/plugin-bridge/v1/hooks/{hook_key}")
}

pub(crate) async fn invocations(
    pool: &sqlx::PgPool,
    installation_id: Uuid,
) -> Vec<(String, String, i32, Option<String>, Option<String>)> {
    sqlx::query_as(
        "SELECT trigger, status, attempt, error, delivery_id FROM plugin_invocation \
         WHERE installation_id = $1 ORDER BY created_at ASC, id ASC",
    )
    .bind(installation_id)
    .fetch_all(pool)
    .await
    .expect("read invocations")
}

/// 本片的额外清场：`plugin_hook_schedule` **没有外键**，共享的 `cleanup`（M6-5 的）
/// 够不着它 ⇒ 不删就会在库里攒孤儿行，下一轮的 `(installation_id, hook_key)` 唯一索引
/// 会把它当冲突报出来。
pub(crate) async fn cleanup_schedules(pool: &sqlx::PgPool, installation_id: Uuid) {
    let _ = sqlx::query("DELETE FROM plugin_hook_schedule WHERE installation_id = $1")
        .bind(installation_id)
        .execute(pool)
        .await;
}

/// 一个装好的插件 + 它的安装 id（夹具三件套的合并写法）。
pub(crate) async fn installed(
    app: &axum::Router,
    _pool: &sqlx::PgPool,
    workspace_id: Uuid,
    owner: Uuid,
    plugin_key: &str,
    contributes: &Value,
) -> (Uuid, Uuid) {
    let manifest = with_net_scope(manifest(plugin_key, "1.0.0", contributes));
    let installation = install_with_net(app, workspace_id, owner, &manifest, &[]).await;
    let installation_id = id_of(&installation);
    (installation_id, version_uuid(&installation))
}

// ---------------------------------------------------------------------------
// ① 路由
// ---------------------------------------------------------------------------

/// `DoD`：**一次调用落一行 `plugin_invocation`**，且出站失败折成插件面的 502。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_manual_hook_call_records_its_invocation_and_reports_the_failure() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (installation_id, version_id) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.hooks",
        &http_hook_manifest("sync"),
    )
    .await;

    let (status, body) = call_raw(
        &app,
        hook_req(
            &hook_uri("sync"),
            workspace_id,
            owner,
            installation_id,
            json!({ "trigger": "manual", "input": { "hello": "world" } }),
        ),
    )
    .await;
    // `mcp.example.com` 解析不到 ⇒ 目的地校验就拒了。上游对 `ValidatePublicHTTPSEndpoint` 的
    // 失败答 **403**（`hook endpoint is not allowed`）：那是「这个目的地不允许」，与
    // 「端点通了但答了 500」（502 `plugin_unavailable`）是两种问题、两种负责人。
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "forbidden");

    let rows = invocations(&pool, installation_id).await;
    assert_eq!(rows.len(), 1, "一次调用必须恰好一行：{rows:?}");
    let (trigger, row_status, attempt, error, delivery_id) = &rows[0];
    assert_eq!(trigger, "manual");
    assert_eq!(
        row_status, "refused",
        "被目的地判据拒掉是 `refused`（上游 `hookFailureStatus` 把 Forbidden 折成它）"
    );
    assert_eq!(*attempt, 1);
    assert_eq!(delivery_id, &None, "非计划触发的 delivery_id 必须是 NULL");
    let error = error.as_deref().expect("失败描述必须落库");
    assert!(
        error.len() <= 500,
        "失败描述有 500 字符上限（表是运维遥测，不是历史）：{error}"
    );
    assert!(
        error.contains("hook endpoint"),
        "只记宿主自己的描述，且不含响应体：{error}"
    );

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_runtime(&pool, installation_id, version_id).await;
}

/// `DoD`：未知 key 404；`trigger` 只收 `ui`/`manual`（400）；未声明的触发器 403。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_hook_key_and_the_trigger_are_both_checked() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (installation_id, version_id) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.hooks",
        &http_hook_manifest("sync"),
    )
    .await;

    // 未知 key：manifest 里没有它 ⇒ 404（`:key` 是不透明引用，不是 uuid，别用 Uuid 提取器）。
    let (status, body) = call_raw(
        &app,
        hook_req(
            &hook_uri("nope"),
            workspace_id,
            owner,
            installation_id,
            json!({ "trigger": "manual" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "not_found");

    // `event` / `agent` / `schedule` 都不能从浏览器收下（上游 `trigger must be ui or manual`）。
    for rejected in ["event", "agent", "schedule", "UI"] {
        let (status, body) = call_raw(
            &app,
            hook_req(
                &hook_uri("sync"),
                workspace_id,
                owner,
                installation_id,
                json!({ "trigger": rejected }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}: {body}");
        assert_eq!(error_message(&body), "trigger must be ui or manual");
    }

    // 触发器必须在 manifest 里声明过：换一个只声明 `schedule` 的 hook，`manual` 就是 403。
    let (other_id, other_version) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.cron",
        &json!({ "hooks": [ { "key": "cron", "name": "Cron",
            "description": "scheduled only",
            "triggers": ["schedule"],
            "schedule": { "cron": "*/5 * * * *", "timezone": "UTC" },
            "transport": { "type": "http", "url": "https://mcp.example.com/hook" } } ] }),
    )
    .await;
    let (status, body) = call_raw(
        &app,
        hook_req(
            &hook_uri("cron"),
            workspace_id,
            owner,
            other_id,
            json!({ "trigger": "manual" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "forbidden");
    assert!(
        error_message(&body).contains("does not declare the manual trigger"),
        "{body}"
    );
    assert!(
        invocations(&pool, other_id).await.is_empty(),
        "前置被拒的调用**不该**落 invocation 行（上游在调用之前就返回了）"
    );

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_runtime(&pool, installation_id, version_id).await;
    cleanup_runtime(&pool, other_id, other_version).await;
}

/// `issue_id` 给了就要过第三步授权：范围外/不存在 ⇒ 404（不是 403 —— 不确认 id 存在）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn an_out_of_range_issue_reference_is_404() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (installation_id, version_id) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.hooks",
        &http_hook_manifest("sync"),
    )
    .await;

    let (status, body) = call_raw(
        &app,
        hook_req(
            &hook_uri("sync"),
            workspace_id,
            owner,
            installation_id,
            json!({ "trigger": "manual", "issue_id": Uuid::new_v4().to_string() }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "not_found");
    assert!(invocations(&pool, installation_id).await.is_empty());

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_runtime(&pool, installation_id, version_id).await;
}

// ---------------------------------------------------------------------------
// ② 凭据（会话信任边界）
// ---------------------------------------------------------------------------

/// `DoD`：**401 语义** —— 匿名调用与插件令牌都进不来（桥面只认会话）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_bridge_face_is_session_only() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (installation_id, version_id) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.hooks",
        &http_hook_manifest("sync"),
    )
    .await;

    // 匿名（既没有会话，也没有插件令牌）。
    let anonymous = Request::builder()
        .method("POST")
        .uri(hook_uri("sync"))
        .header(INSTALLATION_HEADER, installation_id.to_string())
        .header("content-type", "application/json")
        .body(Body::from(json!({ "trigger": "manual" }).to_string()))
        .expect("request");
    let (status, body) = call_raw(&app, anonymous).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(error_code(&body), "unauthorized");

    // 插件令牌（`mpi_…`）：桥面**不认**，由 `policy::apply_bridge` 一层挡掉。
    let token_req = Request::builder()
        .method("POST")
        .uri(hook_uri("sync"))
        .header("authorization", "Bearer mpi_deadbeef")
        .header(INSTALLATION_HEADER, installation_id.to_string())
        .header("content-type", "application/json")
        .body(Body::from(json!({ "trigger": "manual" }).to_string()))
        .expect("request");
    let (status, body) = call_raw(&app, token_req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    // 这一层是 `policy::apply_bridge` 的**问题体**（`application/problem+json`），不是插件面的
    // `{"error":{...}}` 信封 —— 所以码在 `code` 字段上，`error_code` 读的是另一种形状。
    assert_eq!(body["code"], "session_required", "{body}");
    assert_eq!(body["status"], 401, "{body}");

    // 非成员：安装行在别的 workspace ⇒ 成员判定失败 ⇒ 404（「非成员不可见」）。
    let stranger_workspace = {
        let (other_ws, other_user) = seed_workspace(&pool, "owner").await;
        (other_ws, other_user)
    };
    let (status, body) = call_raw(
        &app,
        hook_req(
            &hook_uri("sync"),
            stranger_workspace.0,
            stranger_workspace.1,
            installation_id,
            json!({ "trigger": "manual" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "not_found");

    // 缺安装头 ⇒ 400（上游 `plugin installation is required`）。
    let missing_header = Request::builder()
        .method("POST")
        .uri(hook_uri("sync"))
        .header(USER_ID_HEADER, owner.to_string())
        .header("content-type", "application/json")
        .body(Body::from(json!({ "trigger": "manual" }).to_string()))
        .expect("request");
    let (status, body) = call_raw(&app, missing_header).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_message(&body), "plugin installation is required");

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup(&pool, stranger_workspace.0, &[stranger_workspace.1]).await;
    cleanup_runtime(&pool, installation_id, version_id).await;
}

/// 关掉的插件就是关掉的：iframe 里残留的标签页不能继续调。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_disabled_installation_is_403() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (installation_id, version_id) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.hooks",
        &http_hook_manifest("sync"),
    )
    .await;

    sqlx::query("UPDATE plugin_installation SET enabled = FALSE WHERE id = $1")
        .bind(installation_id)
        .execute(&pool)
        .await
        .expect("disable installation");

    let (status, body) = call_raw(
        &app,
        hook_req(
            &hook_uri("sync"),
            workspace_id,
            owner,
            installation_id,
            json!({ "trigger": "manual" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_message(&body), "this Plugin is disabled");

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_runtime(&pool, installation_id, version_id).await;
}

// ---------------------------------------------------------------------------
// ③ 降级（两条都是**明确错误码**，不是 panic、不是放行）
// ---------------------------------------------------------------------------

/// 部署密钥缺失 ⇒ 503 `plugin_disabled`（`docs/32` §9.8 的 `M6D-1`：四处同款口径）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_missing_deployment_key_fails_closed() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let seeded = app(db.clone());
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (installation_id, version_id) = installed(
        &seeded,
        &pool,
        workspace_id,
        owner,
        "com.example.hooks",
        &http_hook_manifest("sync"),
    )
    .await;

    // 同一个库、同一个安装，只把部署密钥拿掉。
    let without_key = app_without_deployment_key(db);
    let (status, body) = call_raw(
        &without_key,
        hook_req(
            &hook_uri("sync"),
            workspace_id,
            owner,
            installation_id,
            json!({ "trigger": "manual" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(error_code(&body), "plugin_disabled");
    assert!(
        error_message(&body).contains("hooks are disabled"),
        "文案与上游 `hookSigningKey` 逐字：{body}"
    );
    // 记录仍要落一行（上游 `InvokeHook` 无论成败都记），但**必须**是 `failed` 且带
    // hooks-disabled 的文案 —— 「降级」不是「静默通过」。
    let rows = invocations(&pool, installation_id).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1, "failed");
    assert!(
        rows[0]
            .3
            .as_deref()
            .unwrap_or_default()
            .contains("hooks are disabled"),
        "{rows:?}"
    );

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_runtime(&pool, installation_id, version_id).await;
}

/// `plugins_v1` 显式登记为 `false` ⇒ 403 `plugin_api_disabled`（与 M6-5/M6-7 同一口径）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_disabled_feature_flag_blocks_the_hook_face() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let seeded = app(db.clone());
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (installation_id, version_id) = installed(
        &seeded,
        &pool,
        workspace_id,
        owner,
        "com.example.hooks",
        &http_hook_manifest("sync"),
    )
    .await;

    let off = app_with_plugins_v1_disabled(db);
    let (status, body) = call_raw(
        &off,
        hook_req(
            &hook_uri("sync"),
            workspace_id,
            owner,
            installation_id,
            json!({ "trigger": "manual" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "plugin_api_disabled");

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_runtime(&pool, installation_id, version_id).await;
}

// ---------------------------------------------------------------------------
// ④ 限流（120 次/分/钩子，按**尝试**计）
// ---------------------------------------------------------------------------

/// `DoD`：每 hook 每分钟 120 次尝试，超出 ⇒ 507 `insufficient_storage`（`PluginErrorQuota`）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_per_hook_rate_limit_trips_at_120_attempts() {
    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app(db);
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (installation_id, version_id) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.hooks",
        &http_hook_manifest("sync"),
    )
    .await;
    let installation_uuid = installation_id;

    // 一个窗口内已经烧掉 120 次尝试（一条 SQL 造齐）。
    sqlx::query(
        "INSERT INTO plugin_invocation \
           (installation_id, workspace_id, hook_key, trigger, status, attempt, latency_ms, created_at) \
         SELECT $1, $2, 'sync', 'manual', 'failed', 1, 5, now() FROM generate_series(1, 120)",
    )
    .bind(installation_uuid)
    .bind(workspace_id)
    .execute(&pool)
    .await
    .expect("seed attempts");

    let (status, body) = call_raw(
        &app,
        hook_req(
            &hook_uri("sync"),
            workspace_id,
            owner,
            installation_id,
            json!({ "trigger": "manual" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE, "{body}");
    assert_eq!(error_code(&body), "insufficient_storage");
    assert!(
        error_message(&body).contains("120 calls per minute"),
        "{body}"
    );
    assert_eq!(
        invocations(&pool, installation_uuid).await.len(),
        120,
        "被限流的那一次**不该**再落一行（上游在调用前就返回了）"
    );

    // 另一个 hook 不受影响（计数按 (installation, hook_key) 分桶）。
    let (other_id, other_version) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.other",
        &http_hook_manifest("other"),
    )
    .await;
    let (status, _) = call_raw(
        &app,
        hook_req(
            &hook_uri("other"),
            workspace_id,
            owner,
            other_id,
            json!({ "trigger": "manual" }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "另一个钩子的桶是空的 ⇒ 不被限流，走到目的地判据那一步"
    );

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_runtime(&pool, installation_uuid, version_id).await;
    cleanup_runtime(&pool, other_id, other_version).await;
}
