//! 七条 `DingTalk` 路由的端到端测试（M7-9 / `LUM-1774`）。
//!
//! 覆盖四件事（`docs/60-M7-PLAN.md` §6.5 的 M7-9 行 + 本片的专属验收）：
//!
//! 1. **未配置语义逐条对齐**（R-M7-3，不许"统一 503"）：列表 / 群清单 = 200 空；
//!    撤销 / 摘群 / BYO / 兑换 = **403** `dingtalk_not_configured`；
//! 2. **agent 级 scope 矩阵**（workspace 成员 × 私有 agent 的 owner，**四条**）：
//!    agent owner（普通 member 角色）放行、别的普通成员 403、workspace admin 放行、非成员 404；
//! 3. **BYO 往返**：贴凭据 → 真替身校验（`accessToken` 那一跳）→ **密文**落库 → 列表可见 →
//!    撤销 204 → 状态翻 `revoked` → 重贴翻回 `active`；跨 agent 抢同一个 `AppKey` = 409；
//! 4. **群清单与摘群 + 绑定兑换**：观察行 ⇒ 分组/排序；`forget` 204 后消失；
//!    兑换的 200 / 410（重复）/ 409（已属他人）/ 403（非成员）。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//! `2026-09-25` 实测口径：**每轮 DROP/CREATE 测试库**（`docs/32` §22 的观察项：同库连跑两次
//! ⑥ 会因 telegram 面的跨用例行竞争报假红）。

mod support;

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{call, STUB_LOCK};
use support::{
    app_key, configured, fixture, serve_token_stub, unconfigured, Fx, APP_SECRET, SECRET_KEY_BASE64,
};

// =====================================================================
// ① 未配置语义（真库在、密钥不在 ⇒ 一条 SQL 都不该跑）
// =====================================================================

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_unconfigured_semantics_are_per_endpoint_against_a_real_database() {
    let fx = fixture!(unconfigured());
    let user = fx.seed.admin;
    let app_key = app_key();
    let installation = Uuid::new_v4();

    let (status, body) = call(&fx.app, "GET", &fx.installations_uri(), Some(user), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installations"], json!([]));
    assert_eq!(body["configured"], json!(false));
    assert_eq!(body["install_supported"], json!(false));

    let (status, body) = call(&fx.app, "GET", &fx.groups_uri(), Some(user), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!([]));
    assert_eq!(body["group_discovery_supported"], json!(true));

    let revoke = format!("{}/{installation}", fx.installations_uri());
    let (status, body) = call(&fx.app, "DELETE", &revoke, Some(user), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], json!("dingtalk_not_configured"));

    let (status, body) = call(
        &fx.app,
        "DELETE",
        &fx.forget_uri(installation, "cid-1"),
        Some(user),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], json!("dingtalk_not_configured"));

    let (status, body) = call(
        &fx.app,
        "POST",
        &fx.byo_uri(fx.seed.agent_id),
        Some(user),
        Some(json!({ "client_id": app_key, "client_secret": APP_SECRET })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], json!("dingtalk_not_configured"));

    let (status, body) = call(
        &fx.app,
        "POST",
        "/api/dingtalk/binding/redeem",
        Some(user),
        Some(json!({ "token": "whatever" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], json!("dingtalk_not_configured"));

    // 反向验收：退役路由**必须** 404（上游 `integration_test.go:786` 主动断言它）。
    let (status, _) = call(
        &fx.app,
        "GET",
        &format!(
            "/api/workspaces/{}/dingtalk/group-routes",
            fx.seed.workspace_id
        ),
        Some(user),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    fx.teardown().await;
}

// =====================================================================
// ② agent 级 scope 矩阵（workspace 成员 × 私有 agent 的 owner）
// =====================================================================

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_agent_scope_matrix_keeps_a_private_agent_closed() {
    let fx = fixture!(configured());
    // agent 的 owner 是一个**普通成员**（`member` 角色），不是 admin。
    let private_agent = fx.new_agent(fx.seed.member, "private").await;
    let uri = fx.agent_groups_uri(private_agent);

    // ① agent owner（普通 member 角色）⇒ 放行；未配置 ⇒ 200 + 空清单。
    let (status, body) = call(&fx.app, "GET", &uri, Some(fx.seed.member), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!([]));
    assert_eq!(body["group_discovery_supported"], json!(true));

    // ② 同 workspace 的**另一个**普通成员 ⇒ 私有 agent 看不到 ⇒ 403。
    let (status, body) = call(&fx.app, "GET", &uri, Some(fx.seed.guest), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.to_string().contains("access to this agent"),
        "上游文案逐字：{body}"
    );

    // ③ workspace admin（owner 角色）⇒ `can_manage` 放行（admin 能管别人的 agent）。
    let (status, _) = call(&fx.app, "GET", &uri, Some(fx.seed.admin), None).await;
    assert_eq!(status, StatusCode::OK);

    // ④ 非成员（outsider）⇒ workspace 不是我的 ⇒ 404。
    let (status, _) = call(&fx.app, "GET", &uri, Some(fx.seed.outsider), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 反例的一半：`public_to` + workspace 目标 ⇒ 普通成员也看得到（矩阵的另一格）。
    let public_agent = fx.new_agent(fx.seed.admin, "public_to").await;
    sqlx::query(
        "INSERT INTO agent_invocation_target(agent_id, target_type, target_id) \
         VALUES ($1, 'workspace', $2)",
    )
    .bind(public_agent)
    .bind(fx.seed.workspace_id)
    .execute(&fx.pool)
    .await
    .expect("invocation target");
    let (status, _) = fx
        .request("GET", &fx.agent_groups_uri(public_agent), fx.seed.guest)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "public_to + workspace 目标对成员可见"
    );

    fx.teardown().await;
}

// =====================================================================
// ③ BYO 往返（含密文落库与冲突分类）
// =====================================================================

/// 贴一次 BYO 凭据（`admin` 打；`agent_id` 由调用方给）。
async fn byo_install(fx: &Fx, agent_id: Uuid, app_key: &str) -> (StatusCode, serde_json::Value) {
    call(
        &fx.app,
        "POST",
        &fx.byo_uri(agent_id),
        Some(fx.seed.admin),
        Some(json!({ "client_id": app_key, "client_secret": APP_SECRET })),
    )
    .await
}

/// 落库的 `config` 是 `secretbox` 密文（**反例：明文入库即失败**）。
async fn assert_ciphertext(fx: &Fx, installation_id: Uuid, app_key: &str) {
    let config: serde_json::Value =
        sqlx::query_scalar("SELECT config FROM channel_installation WHERE id = $1")
            .bind(installation_id)
            .fetch_one(&fx.pool)
            .await
            .expect("config");
    let text = config.to_string();
    assert!(!text.contains(APP_SECRET), "明文不得入库：{text}");
    assert_eq!(config["app_id"], json!(app_key));
    assert_eq!(config["robot_code"], json!(app_key));
    let sealed = mc_channel::dingtalk::config::decode_ciphertext(
        config["app_secret_encrypted"].as_str().expect("ciphertext"),
    )
    .expect("base64");
    let boxed = mc_secrets::secretbox::load_key_with("MULTICA_DINGTALK_SECRET_KEY", |name| {
        if name == "MULTICA_DINGTALK_SECRET_KEY" {
            Some(SECRET_KEY_BASE64.to_string())
        } else {
            None
        }
    })
    .expect("box");
    assert_eq!(
        boxed.open(&sealed).expect("open"),
        APP_SECRET.as_bytes(),
        "只有部署密钥能还原它"
    );
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_byo_round_trip_stores_ciphertext_and_flips_status() {
    let fx = fixture!(configured());
    let app_key = app_key();
    let _guard = STUB_LOCK.lock().await;
    mc_channel::dingtalk::outbound::openapi::set_api_base(serve_token_stub(true).await);

    // 贴凭据 ⇒ 200 + 安装行。
    let (status, body) = byo_install(&fx, fx.seed.agent_id, &app_key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let installation_id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");
    assert_eq!(body["status"], json!("active"));
    assert_eq!(body["agent_available"], json!(true));
    // 响应的字段集与上游一致（**没有** config / app_id）。
    assert!(body.get("config").is_none(), "{body}");
    assert_ciphertext(&fx, installation_id, &app_key).await;

    // 列表可见：admin 看到那一条（`config` 不进响应）。
    let (status, body) = fx
        .request("GET", &fx.installations_uri(), fx.seed.admin)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], json!(true));
    assert_eq!(body["installations"].as_array().expect("array").len(), 1);
    assert!(!body.to_string().contains("app_secret"), "{body}");

    // 普通成员**看不到别人的私有 agent** 的安装（上游 `dingtalkAgentVisibility` 的过滤）；
    // `configured` 仍然为真（那是部署密钥的事，与可见性无关）。
    let (status, body) = fx
        .request("GET", &fx.installations_uri(), fx.seed.member)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], json!(true));
    assert_eq!(
        body["installations"].as_array().expect("array").len(),
        0,
        "可见性过滤：{body}"
    );

    // 撤销 ⇒ 204，状态翻 `revoked`（**行保留**供审计）。
    let revoke = format!("{}/{installation_id}", fx.installations_uri());
    let (status, _) = call(&fx.app, "DELETE", &revoke, Some(fx.seed.admin), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(fx.installation_status(installation_id).await, "revoked");

    // 重贴 ⇒ 状态翻回 `active`（同一个 agent / 同一个 `AppKey` 原地更新，行 id 不变）。
    let (status, _) = byo_install(&fx, fx.seed.agent_id, &app_key).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fx.installation_status(installation_id).await, "active");

    mc_channel::dingtalk::outbound::openapi::reset_api_base();
    fx.teardown().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_byo_refusals_are_classified() {
    let fx = fixture!(configured());
    let app_key = app_key();
    let _guard = STUB_LOCK.lock().await;
    mc_channel::dingtalk::outbound::openapi::set_api_base(serve_token_stub(true).await);

    let (status, body) = byo_install(&fx, fx.seed.agent_id, &app_key).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 同一个 `AppKey` 贴到**另一个** agent ⇒ 409（唯一索引 `(dingtalk, app_id)` + 分类）。
    let other_agent = fx.new_agent(fx.seed.admin, "private").await;
    let (status, body) = byo_install(&fx, other_agent, &app_key).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("dingtalk_robot_owned_by_same_workspace")
    );

    // 空凭据 ⇒ 400（**不**碰网络）。
    let (status, body) = call(
        &fx.app,
        "POST",
        &fx.byo_uri(fx.seed.agent_id),
        Some(fx.seed.admin),
        Some(json!({ "client_id": "", "client_secret": APP_SECRET })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 非 admin / 非 agent owner 的成员贴不到**别人的** agent 上 ⇒ 403。
    let (status, _) = call(
        &fx.app,
        "POST",
        &fx.byo_uri(fx.seed.agent_id),
        Some(fx.seed.guest),
        Some(json!({ "client_id": app_key, "client_secret": APP_SECRET })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 认不出的 agent ⇒ 404（边界上的所有权前置校验）。
    let (status, body) = byo_install(&fx, Uuid::new_v4(), &app_key).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 平台**拒了**凭据 ⇒ 400（**不是** 500：这是用户错误）。
    mc_channel::dingtalk::outbound::openapi::set_api_base(serve_token_stub(false).await);
    let fresh_agent = fx.new_agent(fx.seed.admin, "private").await;
    let (status, body) = byo_install(
        &fx,
        fresh_agent,
        &format!("rejected-{}", Uuid::new_v4().simple()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("dingtalk_credential_validation_failed")
    );

    mc_channel::dingtalk::outbound::openapi::reset_api_base();
    fx.teardown().await;
}

// =====================================================================
// ④ 群清单 / 摘群 / 绑定兑换
// =====================================================================

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_group_inventory_forgets_one_group_and_keeps_the_rest() {
    let fx = fixture!(configured());
    let installation_id = fx.install_active().await;
    fx.observe(installation_id, "cid-b", "Beta", true).await;
    fx.observe(installation_id, "cid-a", "Alpha", true).await;
    fx.observe(installation_id, "cid-old", "Zeta", false).await;
    sqlx::query(
        "INSERT INTO dingtalk_bot_identity(workspace_id, installation_id, bot_name) \
         VALUES ($1, $2, 'My DingTalk Bot')",
    )
    .bind(fx.seed.workspace_id)
    .bind(installation_id)
    .execute(&fx.pool)
    .await
    .expect("identity");

    // 活跃清单：两个群，按标题升序；空标题那条不出现。
    let (status, body) = call(&fx.app, "GET", &fx.groups_uri(), Some(fx.seed.admin), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let groups = body["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 2, "{body}");
    assert_eq!(groups[0]["conversation_id"], json!("cid-a"));
    assert_eq!(groups[1]["conversation_id"], json!("cid-b"));
    assert_eq!(groups[0]["bots"][0]["bot_name"], json!("My DingTalk Bot"));
    assert_eq!(groups[0]["bots"][0]["mention_count"], json!(3));
    assert_eq!(
        body["bot_identities"][installation_id.to_string()]["bot_name"],
        json!("My DingTalk Bot")
    );
    assert!(body.get("next_offset").is_none());

    // 非活跃清单：必须带 `installation_id`（缺 ⇒ 400），带上就只看到那一个群。
    let (status, body) = call(
        &fx.app,
        "GET",
        &format!("{}?activity=inactive", fx.groups_uri()),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = call(
        &fx.app,
        "GET",
        &format!(
            "{}?activity=inactive&installation_id={installation_id}",
            fx.groups_uri()
        ),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let groups = body["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 1, "{body}");
    assert_eq!(groups[0]["conversation_id"], json!("cid-old"));
    assert_eq!(
        body["inactive_group_counts"][installation_id.to_string()],
        json!(1)
    );

    // 摘群 ⇒ 204，之后活跃清单里没有它（另一个群**保留**）。
    let (status, _) = call(
        &fx.app,
        "DELETE",
        &fx.forget_uri(installation_id, "cid-a"),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(&fx.app, "GET", &fx.groups_uri(), Some(fx.seed.admin), None).await;
    assert_eq!(status, StatusCode::OK);
    let groups = body["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 1, "{body}");
    assert_eq!(groups[0]["conversation_id"], json!("cid-b"));

    // 同一条再摘 ⇒ 404（上游 `pgx.ErrNoRows` 那一支）。
    let (status, _) = call(
        &fx.app,
        "DELETE",
        &fx.forget_uri(installation_id, "cid-a"),
        Some(fx.seed.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 普通成员**不能**摘群（owner/admin only）。
    let (status, _) = call(
        &fx.app,
        "DELETE",
        &fx.forget_uri(installation_id, "cid-b"),
        Some(fx.seed.guest),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    fx.teardown().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_agent_level_inventory_only_shows_its_own_agent() {
    let fx = fixture!(configured());
    let mine = fx.install_active_for(fx.seed.agent_id).await;
    let other_agent = fx.new_agent(fx.seed.admin, "private").await;
    let theirs = fx.install_active_for(other_agent).await;
    fx.observe(mine, "cid-mine", "Mine", true).await;
    fx.observe(theirs, "cid-theirs", "Theirs", true).await;

    let (status, body) = fx
        .request("GET", &fx.agent_groups_uri(fx.seed.agent_id), fx.seed.admin)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let groups = body["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 1, "只回这一个 agent 的群：{body}");
    assert_eq!(groups[0]["conversation_id"], json!("cid-mine"));
    assert_eq!(
        groups[0]["bots"][0]["agent_id"],
        json!(fx.seed.agent_id.to_string())
    );

    fx.teardown().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn the_binding_redeem_has_four_distinct_verdicts() {
    let fx = fixture!(configured());
    let installation_id = fx.install_active().await;

    // 非成员兑换 ⇒ 403，且**不烧掉**令牌。
    let staff_id = format!("staff-{}", Uuid::new_v4().simple());
    fx.mint_token("token-non-member", installation_id, &staff_id)
        .await;
    let (status, body) = fx
        .request_body(
            "POST",
            "/api/dingtalk/binding/redeem",
            fx.seed.outsider,
            json!({ "token": "token-non-member" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], json!("dingtalk_binding_not_member"));
    let consumed: Option<String> = sqlx::query_scalar(
        "SELECT consumed_at::text FROM channel_binding_token WHERE token_hash = $1",
    )
    .bind(mc_channel::dingtalk::binding::hash_binding_token(
        "token-non-member",
    ))
    .fetch_one(&fx.pool)
    .await
    .expect("token row");
    assert!(consumed.is_none(), "非成员的尝试不该烧掉令牌");

    // 成员兑换 ⇒ 200，带上令牌里的 staff id。
    let (status, body) = fx
        .request_body(
            "POST",
            "/api/dingtalk/binding/redeem",
            fx.seed.member,
            json!({ "token": "token-non-member" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dingtalk_user_id"], json!(staff_id));
    assert_eq!(body["installation_id"], json!(installation_id.to_string()));

    // 同一枚令牌再兑一次 ⇒ 410（`consumed_at` 的 CAS 已经占住）。
    let (status, body) = fx
        .request_body(
            "POST",
            "/api/dingtalk/binding/redeem",
            fx.seed.member,
            json!({ "token": "token-non-member" }),
        )
        .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("dingtalk_binding_token_invalid")
    );

    // 同一个 staff id 铸第二枚、换个人兑 ⇒ 409（转移必须走显式解绑）。
    fx.mint_token("token-second", installation_id, &staff_id)
        .await;
    let (status, body) = fx
        .request_body(
            "POST",
            "/api/dingtalk/binding/redeem",
            fx.seed.guest,
            json!({ "token": "token-second" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("dingtalk_binding_already_assigned")
    );

    // 空令牌 ⇒ 400。
    let (status, _) = call(
        &fx.app,
        "POST",
        "/api/dingtalk/binding/redeem",
        Some(fx.seed.member),
        Some(json!({ "token": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 绑定行确实建了（幂等：重复兑换不插第二行）。
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_user_binding WHERE installation_id = $1 AND channel_user_id = $2",
    )
    .bind(installation_id)
    .bind(&staff_id)
    .fetch_one(&fx.pool)
    .await
    .expect("count");
    assert_eq!(rows, 1);

    fx.teardown().await;
}
