//! 真库用例（门 ⑥）：四条路由的端到端行为 + `store` 两个端口实现的 SQL。
//!
//! 拆出来是门 ⑩ 的要求（`docs/32` §31 的 D8）。未设 `MULTICA_TEST_DATABASE_URL` ⇒
//! 打印跳过并 `return`；**设了但连不上 / 没建表 ⇒ panic**（不许静默假装绿）。

use super::*;
use mc_channel::wecom::binding::hash_binding_token;
use mc_channel::wecom::credentials::CredentialsResolver;
use mc_channel::wecom::installation::InstallationService;
use mc_channel::wecom::store::{
    InstallationQueries, InstallationStore, PersistInstall, PersistOutcome,
};
use mc_channel::wecom::types::{encode_ciphertext, InstallConfig};

use super::support::*;

// -----------------------------------------------------------------
// 端口实现（SQL）
// -----------------------------------------------------------------

/// **本片专属验收第 1 条（真库版）**：BYO 凭据落库 = `secretbox` 密文，
/// **明文在库里搜不到**；三个读面（含 `store.go` 的三条）与它一致。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn persist_stores_ciphertext_only_and_the_read_ports_agree() {
    let db = fixture!();
    let seed = seed(&db).await;
    let store = store::PgInstallStore::new(db.clone());

    let outcome = store
        .persist(&persist_params(&seed, &seed.bot_id))
        .await
        .expect("persist");
    let PersistOutcome::Stored(installation) = outcome else {
        panic!("首次安装必须落库：{outcome:?}");
    };
    assert_eq!(installation.bot_id, seed.bot_id);
    assert!(installation.is_active());

    // 反例：库里逐字搜明文 —— 搜到即失败。配置列与整行文本都查。
    let config_text: String =
        sqlx::query_scalar("SELECT config::text FROM channel_installation WHERE id = $1")
            .bind(installation.id.0)
            .fetch_one(db.pool())
            .await
            .expect("read config");
    assert!(
        !config_text.contains(PLAINTEXT_SENTINEL),
        "明文入库了：{config_text}"
    );
    assert!(
        config_text.contains(&encode_ciphertext(&installation.secret_encrypted)),
        "密文列不是 base64(secretbox)：{config_text}"
    );
    // 正向：用部署密钥解得回明文。
    let credentials = mc_channel::wecom::credentials::SecretboxCredentialsResolver::new(boxed())
        .credentials(&installation)
        .expect("unseal");
    assert_eq!(credentials.secret.expose(), PLAINTEXT_SENTINEL);

    // 读面一致：路由键 / 主键（`store.go` 的三条）+ 管理列表 + 工作区收窄。
    let by_bot = InstallationQueries::get_by_bot_id(&store, &seed.bot_id)
        .await
        .expect("get_by_bot_id")
        .expect("命中");
    assert_eq!(by_bot.id, installation.id);
    assert_eq!(
        InstallationQueries::get(&store, installation.id)
            .await
            .expect("get")
            .map(|row| row.id),
        Some(installation.id)
    );
    assert!(InstallationQueries::is_workspace_member(
        &store,
        Id(seed.workspace_id),
        Id(seed.admin)
    )
    .await
    .expect("member"));
    assert!(!InstallationQueries::is_workspace_member(
        &store,
        Id(seed.workspace_id),
        Id(seed.outsider)
    )
    .await
    .expect("non member"));

    let listed = InstallationStore::list_by_workspace(&store, Id(seed.workspace_id))
        .await
        .expect("list");
    assert_eq!(listed.len(), 1, "只有本片的这一行（bot id 随机）");
    assert!(
        InstallationStore::get_in_workspace(&store, installation.id, Id(seed.other_workspace_id))
            .await
            .expect("cross workspace")
            .is_none(),
        "跨工作区读 = 不存在"
    );
    assert_eq!(
        InstallationStore::current_for(&store, Id(seed.workspace_id), Id(seed.agent_id))
            .await
            .expect("current")
            .map(|row| row.id),
        Some(installation.id)
    );
    assert_eq!(
        InstallationStore::slot_owner(&store, &seed.bot_id)
            .await
            .expect("slot owner")
            .map(|owner| (owner.revoked, owner.agent_archived)),
        Some((false, false))
    );
}

/// 撤销把槽位让出来（`revoked` 行**保留**）；重装同一个 agent 原地刷新（**同一个 id**）；
/// 换到另一个 workspace ⇒ 回收死主后落库（旧行被删）。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn revoke_releases_the_slot_and_a_dead_owner_is_reclaimed() {
    let db = fixture!();
    let seed = seed(&db).await;
    let store = store::PgInstallStore::new(db.clone());

    let PersistOutcome::Stored(first) = store
        .persist(&persist_params(&seed, &seed.bot_id))
        .await
        .expect("persist")
    else {
        panic!("首次安装必须落库");
    };
    // 活跃的槽位**不是**死主 ⇒ 别的 workspace 抢不到（唯一索引会撞，判决落到分类）。
    let taken = store
        .persist(&PersistInstall {
            workspace_id: Id(seed.other_workspace_id),
            agent_id: Id(seed.other_agent_id),
            installer_user_id: Id(seed.admin),
            bot_id: seed.bot_id.clone(),
            config: sealed_config(&seed.bot_id),
            bot_display_name: String::new(),
        })
        .await
        .expect("persist contended");
    assert_eq!(
        taken,
        PersistOutcome::OwnedByAnotherWorkspace,
        "活跃槽位被别的 workspace 占着 ⇒ 先撞唯一索引，再按槽主分类"
    );

    // 撤销 ⇒ 槽位让出（行仍在，`revoked`）。
    assert!(
        InstallationStore::revoke(&store, Id(seed.workspace_id), first.id)
            .await
            .expect("revoke")
    );
    assert_eq!(
        InstallationStore::slot_owner(&store, &seed.bot_id)
            .await
            .expect("slot owner")
            .map(|owner| owner.revoked),
        Some(true)
    );

    // 换到另一个 workspace + 另一个 agent ⇒ 回收死主之后落库。
    let PersistOutcome::Stored(moved) = store
        .persist(&PersistInstall {
            workspace_id: Id(seed.other_workspace_id),
            agent_id: Id(seed.other_agent_id),
            installer_user_id: Id(seed.admin),
            bot_id: seed.bot_id.clone(),
            config: sealed_config(&seed.bot_id),
            bot_display_name: String::new(),
        })
        .await
        .expect("persist after revoke")
    else {
        panic!("撤销之后槽位应可抢");
    };
    assert_eq!(moved.workspace_id, Id(seed.other_workspace_id));
    assert_eq!(moved.agent_id, Id(seed.other_agent_id));
    assert!(
        InstallationStore::get_in_workspace(&store, first.id, Id(seed.workspace_id))
            .await
            .expect("old row")
            .is_none(),
        "撤销的旧行被回收（硬删），它不再占着这个全局路由槽"
    );

    // 重装同一个 agent：原地刷新（**同一个 id**、状态翻回 active）。
    InstallationStore::revoke(&store, Id(seed.other_workspace_id), moved.id)
        .await
        .expect("revoke moved");
    let PersistOutcome::Stored(refreshed) = store
        .persist(&PersistInstall {
            workspace_id: Id(seed.other_workspace_id),
            agent_id: Id(seed.other_agent_id),
            installer_user_id: Id(seed.admin),
            bot_id: seed.bot_id.clone(),
            config: sealed_config(&seed.bot_id),
            bot_display_name: String::new(),
        })
        .await
        .expect("reinstall")
    else {
        panic!("重装必须落库");
    };
    assert_eq!(
        refreshed.id, moved.id,
        "ON CONFLICT (ws, agent, type) ⇒ 同一行"
    );
    assert!(refreshed.is_active());
}

// -----------------------------------------------------------------
// 四条路由（端到端）
// -----------------------------------------------------------------

/// 列表：真库的行 + 两个 `true`；非成员 404；撤销 204 且状态翻转；跨工作区撤销 404。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn list_and_revoke_work_end_to_end() {
    let db = fixture!();
    let seed = seed(&db).await;
    let store = store::PgInstallStore::new(db.clone());
    let PersistOutcome::Stored(row) = store
        .persist(&persist_params(&seed, &seed.bot_id))
        .await
        .expect("persist")
    else {
        panic!("落库");
    };
    let app = mount(app_state(db.clone()));

    // member 可见（**不**要求管理员）。
    let uri = format!("/api/workspaces/{}/wecom/installations", seed.workspace_id);
    let (status, body) = call(&app, "GET", &uri, Some(seed.member)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configured"], json!(true));
    assert_eq!(body["install_supported"], json!(true));
    let listed = body["installations"].as_array().expect("array");
    assert_eq!(listed.len(), 1, "{body}");
    assert_eq!(listed[0]["bot_id"], json!(seed.bot_id));
    assert!(
        !body.to_string().contains(PLAINTEXT_SENTINEL),
        "响应回显了明文：{body}"
    );
    assert!(!body.to_string().contains("secret_encrypted"), "{body}");

    // 非成员 ⇒ 404（不是 403：一个外人**不该**学到这个 workspace 存在）。
    let (status, _) = call(&app, "GET", &uri, Some(seed.outsider)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 普通成员撤销 ⇒ 403（上游 router 的 owner/admin 中间件）。
    let revoke_uri = format!(
        "/api/workspaces/{}/wecom/installations/{}",
        seed.workspace_id, row.id
    );
    let (status, body) = call(&app, "DELETE", &revoke_uri, Some(seed.member)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // 跨工作区：同一个 id 从别的 workspace 撤销 ⇒ 404。
    let cross = format!(
        "/api/workspaces/{}/wecom/installations/{}",
        seed.other_workspace_id, row.id
    );
    // 该 workspace 里调用者不是成员 ⇒ 先 404（workspace 就看不见）。
    let (status, _) = call(&app, "DELETE", &cross, Some(seed.admin)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // owner 撤销 ⇒ 204；行保留、状态翻转。
    let (status, body) = call(&app, "DELETE", &revoke_uri, Some(seed.admin)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let status_text: String =
        sqlx::query_scalar("SELECT status FROM channel_installation WHERE id = $1")
            .bind(row.id.0)
            .fetch_one(db.pool())
            .await
            .expect("status");
    assert_eq!(status_text, "revoked");
    assert!(
        InstallationStore::get_in_workspace(&store, row.id, Id(seed.workspace_id))
            .await
            .expect("row")
            .is_some(),
        "撤销是行级语义：行保留供审计"
    );
}

/// BYO：探针的 wire 一半在 M7-16 ⇒ 现在**必然** 503，且**一行都不许写**
/// （证明控制权是安全不变式，没有探针就不能落凭据）。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn byo_answers_503_while_the_probe_transport_is_not_wired() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = mount(app_state(db.clone()));
    let uri = format!(
        "/api/workspaces/{}/wecom/install/byo?agent_id={}",
        seed.workspace_id, seed.agent_id
    );
    let body = json!({ "bot_id": seed.bot_id, "secret": PLAINTEXT_SENTINEL, "bot_name": "itest" });
    let (status, response) =
        call_with_body(&app, "POST", &uri, Some(seed.admin), &body.to_string()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{response}");
    assert_eq!(
        response["error"]["code"],
        json!("wecom_credentials_unverifiable")
    );
    assert!(
        !response.to_string().contains(PLAINTEXT_SENTINEL),
        "错误响应回显了密钥：{response}"
    );
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_installation \
             WHERE channel_type = 'wecom' AND config ->> 'app_id' = $1",
    )
    .bind(&seed.bot_id)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(rows, 0, "凭据没能验证 ⇒ 一行都不许写");

    // 缺 `agent_id` ⇒ 400（调用方补一个字段就能成，与"够不着 WeCom"是两件事）。
    let uri = format!("/api/workspaces/{}/wecom/install/byo", seed.workspace_id);
    let (status, response) =
        call_with_body(&app, "POST", &uri, Some(seed.admin), &body.to_string()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    // 普通成员 ⇒ 403。
    let uri = format!(
        "/api/workspaces/{}/wecom/install/byo?agent_id={}",
        seed.workspace_id, seed.agent_id
    );
    let (status, _) =
        call_with_body(&app, "POST", &uri, Some(seed.member), &body.to_string()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 兑换：真令牌绑到**会话**身份；重放 410；非成员 403 且**不烧令牌**。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn redeem_binds_the_session_user() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = mount(app_state(db.clone()));

    // 令牌的明文每次运行都不同：`channel_binding_token.token_hash` 是主键，写死字面量会让
    // **第二次运行**撞主键（而且与"这个库还能被复用"过不去）。
    let raw = format!("itest-wecom-token-{}-DO-NOT-LOG", uuid::Uuid::new_v4());
    let installation_id = seed_token(&db, &seed, &raw, "wecom", "T-user-1").await;

    // 正常兑换：绑的是**会话**身份（`member`），回的是令牌里的 wecom userid。
    let body = json!({ "token": raw });
    let (status, response) = call_with_body(
        &app,
        "POST",
        "/api/wecom/binding/redeem",
        Some(seed.member),
        &body.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["wecom_user_id"], json!("T-user-1"));
    assert_eq!(
        response["workspace_id"],
        json!(seed.workspace_id.to_string())
    );
    assert_eq!(
        response["installation_id"],
        json!(installation_id.to_string())
    );
    let bound_to: uuid::Uuid = sqlx::query_scalar(
        "SELECT multica_user_id FROM channel_user_binding \
         WHERE installation_id = $1 AND channel_user_id = 'T-user-1'",
    )
    .bind(installation_id)
    .fetch_one(db.pool())
    .await
    .expect("binding row");
    assert_eq!(bound_to, seed.member, "绑定的是会话身份，不是令牌里的");

    // 重放同一条 ⇒ 410（一次性），且不产生第二行。
    let (status, _) = call_with_body(
        &app,
        "POST",
        "/api/wecom/binding/redeem",
        Some(seed.member),
        &body.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    let bindings: i64 =
        sqlx::query_scalar("SELECT count(*) FROM channel_user_binding WHERE installation_id = $1")
            .bind(installation_id)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert_eq!(bindings, 1);

    // 非成员持有一枚**新**令牌 ⇒ 403，且令牌**没被烧**（回滚）。
    let other_raw = format!("itest-wecom-token-{}", uuid::Uuid::new_v4());
    seed_token_on(&db, &seed, &other_raw, "wecom", "T-user-2", installation_id).await;
    let body = json!({ "token": other_raw });
    let (status, response) = call_with_body(
        &app,
        "POST",
        "/api/wecom/binding/redeem",
        Some(seed.outsider),
        &body.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    assert!(
        !token_consumed(&db, &hash_binding_token(&other_raw)).await,
        "非成员的一次尝试不得烧掉令牌"
    );

    // 未知令牌 / 空令牌：410 / 400。
    let (status, _) = call_with_body(
        &app,
        "POST",
        "/api/wecom/binding/redeem",
        Some(seed.member),
        &json!({ "token": format!("never-issued-{}", uuid::Uuid::new_v4()) }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    let (status, _) = call_with_body(
        &app,
        "POST",
        "/api/wecom/binding/redeem",
        Some(seed.member),
        &json!({ "token": "  " }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 令牌表跨 adapter 共享 ⇒ 别的 adapter 的令牌 410，且**没被消费**；明文不进库。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn the_shared_token_table_is_scoped_to_wecom() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = mount(app_state(db.clone()));

    let slack_raw = format!("itest-slack-token-{}", uuid::Uuid::new_v4());
    let wecom_raw = format!("itest-wecom-token-guard-{}", uuid::Uuid::new_v4());
    let slack_hash = hash_binding_token(&slack_raw);
    let installation_id = uuid::Uuid::new_v4();
    seed_token_on(
        &db,
        &seed,
        &slack_raw,
        "slack",
        "U-slack-1",
        installation_id,
    )
    .await;
    seed_token_on(
        &db,
        &seed,
        &wecom_raw,
        "wecom",
        "T-user-guard",
        installation_id,
    )
    .await;

    // 别的 adapter 的令牌：410，且**没被消费**（判据 4：认不出 ⇒ 令牌没动）。
    let body = json!({ "token": slack_raw });
    let (status, response) = call_with_body(
        &app,
        "POST",
        "/api/wecom/binding/redeem",
        Some(seed.member),
        &body.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{response}");
    assert!(
        !token_consumed(&db, &slack_hash).await,
        "别的 adapter 的令牌不得被消费"
    );

    // 明文**不进库**：库里只有哈希。
    let by_plaintext: i64 =
        sqlx::query_scalar("SELECT count(*) FROM channel_binding_token WHERE token_hash = $1")
            .bind(&wecom_raw)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert_eq!(by_plaintext, 0, "明文令牌入库了");
    assert!(token_row_exists(&db, &hash_binding_token(&wecom_raw)).await);
}

/// 落一枚令牌并回它的 `installation_id`（默认用一个随机的安装 id）。
async fn seed_token(
    db: &mc_db::Db,
    seed: &Seed,
    raw: &str,
    token_type: &str,
    channel_user_id: &str,
) -> uuid::Uuid {
    let installation_id = uuid::Uuid::new_v4();
    seed_token_on(db, seed, raw, token_type, channel_user_id, installation_id).await;
    installation_id
}

/// 落一枚令牌到指定的安装上（**只存哈希**：明文参数从不入库）。
async fn seed_token_on(
    db: &mc_db::Db,
    seed: &Seed,
    raw: &str,
    token_type: &str,
    channel_user_id: &str,
    installation_id: uuid::Uuid,
) {
    sqlx::query(
        "INSERT INTO channel_binding_token \
         (token_hash, workspace_id, installation_id, channel_type, channel_user_id, expires_at) \
         VALUES ($1, $2, $3, $4, $5, now() + interval '10 minutes')",
    )
    .bind(hash_binding_token(raw))
    .bind(seed.workspace_id)
    .bind(installation_id)
    .bind(token_type)
    .bind(channel_user_id)
    .execute(db.pool())
    .await
    .expect("insert token");
}

async fn token_consumed(db: &mc_db::Db, token_hash: &str) -> bool {
    let consumed: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT consumed_at FROM channel_binding_token WHERE token_hash = $1")
            .bind(token_hash)
            .fetch_one(db.pool())
            .await
            .expect("token row");
    consumed.is_some()
}

async fn token_row_exists(db: &mc_db::Db, token_hash: &str) -> bool {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM channel_binding_token WHERE token_hash = $1")
            .bind(token_hash)
            .fetch_one(db.pool())
            .await
            .expect("count");
    count == 1
}
/// 装好的服务用**真端口 + 假探针**跑一遍 `upsert`：这条覆盖了"BYO 成功路径"的
/// adapter 侧（路由侧现在只能到 503，见上一条）。
#[tokio::test]
#[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn the_install_service_persists_through_the_real_port() {
    let db = fixture!();
    let seed = seed(&db).await;
    let service = InstallationService::new(
        Arc::new(store::PgInstallStore::new(db.clone())) as Arc<dyn InstallationStore>,
        Arc::new(AcceptingProbe) as Arc<dyn CredentialProbe>,
        boxed(),
    );
    let installed = service
        .upsert(mc_channel::wecom::installation::InstallationParams::new(
            Id(seed.workspace_id),
            Id(seed.agent_id),
            Id(seed.admin),
            &seed.bot_id,
            PLAINTEXT_SENTINEL,
            "itest bot",
        ))
        .await
        .expect("upsert");
    assert_eq!(installed.bot_id, seed.bot_id);
    assert_eq!(
        service
            .credentials(&installed)
            .expect("unseal")
            .secret
            .expose(),
        PLAINTEXT_SENTINEL
    );
    // 落库的形状：四个键，`app_id == bot_id`。
    let config = InstallConfig::from_value(&installed.config).expect("config");
    assert_eq!(config.app_id, seed.bot_id);
    assert_eq!(config.bot_id, seed.bot_id);
    assert_eq!(config.bot_display_name, "itest bot");
    // 同一个 agent 重装 ⇒ 同一个 id（原地刷新）。
    let again = service
        .upsert(mc_channel::wecom::installation::InstallationParams::new(
            Id(seed.workspace_id),
            Id(seed.agent_id),
            Id(seed.admin),
            &seed.bot_id,
            "rotated-secret",
            "",
        ))
        .await
        .expect("reinstall");
    assert_eq!(again.id, installed.id);
    // 留空显示名 ⇒ 承接旧名（同一个 bot）。换 bot 不承接 —— adapter 侧已单测。
    assert_eq!(again.bot_display_name, "itest bot");
    assert_eq!(
        service.credentials(&again).expect("unseal").secret.expose(),
        "rotated-secret",
        "轮换后解得回新密钥"
    );
}
