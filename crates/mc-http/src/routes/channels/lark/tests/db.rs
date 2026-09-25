//! `lark` 真库用例（门 ⑥）：五条路由的配置态端到端行为 + [`store`] 三个端口实现的 SQL。
//!
//! 拆出来是门 ⑩ 的要求（`docs/32` §30 的 **D10**）。未设 `MULTICA_TEST_DATABASE_URL` ⇒
//! 打印跳过并 `return`；**设了但连不上 / 没建表 ⇒ panic**（不许静默假装绿）。

use super::support::*;
use super::*;
use mc_channel::lark::binding::{BindingStore, RedeemOutcome};
use mc_channel::lark::installation::{InstallError, InstallationParams, LarkInstallationStore};
use mc_channel::lark::registration::{CommitInstall, RegistrationStore};
use mc_channel::lark::types::OpenId;

// -----------------------------------------------------------------
// 端口实现（SQL）
// -----------------------------------------------------------------

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn persist_upserts_in_place_and_reserves_the_app_id() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let store = store::PgLarkInstallStore::new(Arc::clone(&state));

    // 第一次：建行。
    let first = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &seed.app_id,
    )
    .await;
    assert_eq!(first.status, "active");
    assert_eq!(first.app_id, seed.app_id);

    // 第二次：同一个 `(workspace, agent)` ⇒ **原地**刷新（`UNIQUE(workspace_id, agent_id)`）。
    let second = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &seed.app_id,
    )
    .await;
    assert_eq!(second.id, first.id, "必须是同一行（不是新行）");
    let rows = store
        .list_by_workspace(Id(seed.workspace_id))
        .await
        .expect("list");
    assert_eq!(rows.len(), 1, "同一 (workspace, agent) 只应有一行");

    // 第三个安装：**同一个** workspace 的**另一个** agent 想用**已经占着**的那个 `app_id`
    // ⇒ `UNIQUE(app_id)` 撞上活主 ⇒ 分类成"同 workspace 的另一个 agent 占着"。
    //
    // （`seed.other_agent_id` 属于**另一个** workspace，那条分支由
    // `commit_install_binds_the_installer_in_the_same_transaction` 与下面的
    // `OwnedByAnotherWorkspace` 断言分开覆盖。）
    let second_agent: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, owner_id) \
         VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
    )
    .bind(seed.workspace_id)
    .bind(format!("itest-m714-rt-c-{}", uuid::Uuid::new_v4()))
    .bind(seed.admin)
    .fetch_one(db.pool())
    .await
    .expect("insert runtime");
    let second_agent: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
         VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
    )
    .bind(seed.workspace_id)
    .bind(format!("itest-m714-agent-c-{}", uuid::Uuid::new_v4()))
    .bind(second_agent)
    .bind(seed.admin)
    .fetch_one(db.pool())
    .await
    .expect("insert agent");

    let service = InstallationService::new(Arc::new(store.clone()), boxed());
    let clash = InstallationParams::new(
        Id(seed.workspace_id),
        Id(second_agent),
        seed.app_id.clone(),
        "itest-app-secret",
        OpenId::new("ou_bot_clash"),
        Id(seed.admin),
    );
    assert_eq!(
        service.upsert(&clash).await,
        Err(InstallError::OwnedBySameWorkspace),
        "活主占着 app_id 时必须分类，而不是抛一句'已占用'"
    );

    // 另一个 workspace 的 agent 拿同一个 `app_id` ⇒ 另一档分类。
    let foreign = InstallationParams::new(
        Id(seed.other_workspace_id),
        Id(seed.other_agent_id),
        seed.app_id.clone(),
        "itest-app-secret",
        OpenId::new("ou_bot_foreign"),
        Id(seed.admin),
    );
    assert_eq!(
        service.upsert(&foreign).await,
        Err(InstallError::OwnedByAnotherWorkspace)
    );
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn revoke_keeps_the_row_and_a_reinstall_reactivates_it() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let store = store::PgLarkInstallStore::new(Arc::clone(&state));
    let row = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &seed.app_id,
    )
    .await;

    assert!(store
        .revoke(Id(seed.workspace_id), row.id)
        .await
        .expect("revoke"));
    // 行**还在**（审计），只是状态翻了。
    let listed = store
        .list_by_workspace(Id(seed.workspace_id))
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, "revoked");
    // 重复撤销 ⇒ `false`（谓词里带 `status = 'active'`）。
    assert!(!store
        .revoke(Id(seed.workspace_id), row.id)
        .await
        .expect("revoke"));

    // 重装把状态翻回 `active`（同一个 `app_id`，撤销行由 upsert 原地复活）。
    let again = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &seed.app_id,
    )
    .await;
    assert_eq!(again.id, row.id);
    assert_eq!(again.status, "active");
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn reads_are_workspace_scoped() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let store = store::PgLarkInstallStore::new(Arc::clone(&state));
    let row = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &seed.app_id,
    )
    .await;

    // 另一个 workspace 猜 id ⇒ `None`（与不存在同结果，不泄露存在性）。
    assert!(store
        .get_in_workspace(row.id, Id(seed.other_workspace_id))
        .await
        .expect("get")
        .is_none());
    assert!(store
        .get_in_workspace(row.id, Id(seed.workspace_id))
        .await
        .expect("get")
        .is_some());
    // 撤销也收窄：另一个 workspace 撤不动它。
    assert!(!store
        .revoke(Id(seed.other_workspace_id), row.id)
        .await
        .expect("revoke"));
    assert_eq!(
        store
            .get_in_workspace(row.id, Id(seed.workspace_id))
            .await
            .expect("get")
            .expect("行在")
            .status,
        "active"
    );
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn the_two_backfill_columns_round_trip() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let store = store::PgLarkInstallStore::new(Arc::clone(&state));
    let row = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &seed.app_id,
    )
    .await;
    assert_eq!(row.bot_union_id, None);
    assert_eq!(row.region, Region::Feishu, "迁移 116 的默认值");

    // 缺 union_id 的活跃行**恰好**是这一行。
    let pending = store.list_active_missing_union_id().await.expect("list");
    assert!(pending.iter().any(|candidate| candidate.id == row.id));
    store
        .set_bot_union_id(row.id, "on_backfilled")
        .await
        .expect("stamp");
    let after = store
        .get_in_workspace(row.id, Id(seed.workspace_id))
        .await
        .expect("get")
        .expect("行在");
    assert_eq!(after.bot_union_id.as_deref(), Some("on_backfilled"));

    // region 回填：这条行（以及任何仍是 `feishu` 的行）被翻成 `lark`。
    let relabelled = store.relabel_region_to_lark().await.expect("relabel");
    assert!(relabelled >= 1);
    let after = store
        .get_in_workspace(row.id, Id(seed.workspace_id))
        .await
        .expect("get")
        .expect("行在");
    assert_eq!(after.region, Region::Lark);
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn commit_install_binds_the_installer_in_the_same_transaction() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let store = store::PgRegistrationStore::new(Arc::clone(&state));

    let params = CommitInstall {
        workspace_id: Id(seed.workspace_id),
        agent_id: Id(seed.agent_id),
        initiator_id: Id(seed.admin),
        app_id: seed.app_id.clone(),
        client_secret: "itest-client-secret".to_string(),
        bot_open_id: OpenId::new("ou_bot_commit"),
        bot_union_id: "on_commit".to_string(),
        region: Region::Lark,
        installer_open_id: OpenId::new("ou_installer"),
    };
    let outcome = store.commit_install(&params).await.expect("commit");
    let mc_channel::lark::installation::PersistOutcome::Stored(row) = outcome else {
        panic!("期望落库成功");
    };
    assert_eq!(row.app_id, seed.app_id);
    assert_eq!(row.region, Region::Lark);
    assert_eq!(row.bot_union_id.as_deref(), Some("on_commit"));

    // 安装者**已经**绑上了（否则第一条入站消息会给他发一张多余的绑定卡）。
    let bound: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT multica_user_id FROM lark_user_binding \
         WHERE installation_id = $1 AND lark_open_id = $2",
    )
    .bind(row.id.0)
    .bind("ou_installer")
    .fetch_optional(db.pool())
    .await
    .expect("query");
    assert_eq!(bound.map(|(id,)| id), Some(seed.admin));

    // 同一个 open_id 换一个用户 ⇒ `AlreadyAssigned`（且事务回滚，不落半成品行）。
    let mut stolen = params.clone();
    stolen.initiator_id = Id(seed.member);
    stolen.agent_id = Id(seed.agent_id);
    let outcome = store.commit_install(&stolen).await.expect("commit");
    assert!(matches!(
        outcome,
        mc_channel::lark::installation::PersistOutcome::Conflict(InstallError::AlreadyAssigned)
    ));
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn redeem_is_transactional_and_member_gated() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let store = store::PgBindingStore::new(Arc::clone(&state));
    let row = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &seed.app_id,
    )
    .await;

    let token = mc_channel::lark::binding::random_binding_token();
    store
        .insert_token(
            &mc_channel::lark::binding::hash_token(&token),
            Id(seed.workspace_id),
            row.id,
            "ou_bound",
            chrono::Utc::now() + mc_channel::lark::types::BINDING_TOKEN_TTL,
        )
        .await
        .expect("insert token");

    // 非成员 ⇒ `NotWorkspaceMember`，且令牌**没有**被烧掉。
    assert_eq!(
        store
            .redeem_and_bind(
                &mc_channel::lark::binding::hash_token(&token),
                Id(seed.outsider)
            )
            .await
            .expect("redeem"),
        RedeemOutcome::NotWorkspaceMember
    );
    // 成员 ⇒ 绑上。
    match store
        .redeem_and_bind(
            &mc_channel::lark::binding::hash_token(&token),
            Id(seed.member),
        )
        .await
        .expect("redeem")
    {
        RedeemOutcome::Bound(bound) => {
            assert_eq!(bound.workspace_id, Id(seed.workspace_id));
            assert_eq!(bound.installation_id, row.id);
            assert_eq!(bound.lark_open_id, "ou_bound");
        }
        other => panic!("期望 Bound，得到 {other:?}"),
    }
    // 第二次 ⇒ 已消费（三合一那一档）。
    assert_eq!(
        store
            .redeem_and_bind(
                &mc_channel::lark::binding::hash_token(&token),
                Id(seed.member)
            )
            .await
            .expect("redeem"),
        RedeemOutcome::TokenInvalid
    );
}

// -----------------------------------------------------------------
// 五条路由（配置态）
// -----------------------------------------------------------------

/// 配置态的整站装置（同一个库里跑，`app_state` 装了 lark 落库密钥）。
fn configured_app(state: &Arc<AppState>) -> Router {
    let router = crate::routes::router(Arc::clone(state));
    router.with_state(Arc::clone(state))
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn the_list_route_is_member_visible_and_hides_the_ciphertext() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let row = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &seed.app_id,
    )
    .await;
    let app = configured_app(&state);

    // 普通成员（不是管理员）也看得见 —— Integrations 页不能对人空白。
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/workspaces/{}/lark/installations", seed.workspace_id),
        Some(seed.member),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configured"], json!(true));
    let rendered = body.to_string();
    assert!(!rendered.contains("itest-app-secret"), "{rendered}");
    assert!(!rendered.contains("app_secret"), "{rendered}");
    let installations = body["installations"].as_array().expect("array");
    assert!(installations
        .iter()
        .any(|item| item["id"] == json!(row.id.to_string())));

    // 非成员 ⇒ 404（不泄露这个 workspace 存在）。
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/workspaces/{}/lark/installations", seed.workspace_id),
        Some(seed.outsider),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn revoke_is_agent_owner_or_admin_and_returns_204() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let app = configured_app(&state);
    let store = store::PgLarkInstallStore::new(Arc::clone(&state));

    // 普通成员（既不是 agent owner 也不是管理员）⇒ 403。
    let row = persist_installation(
        &state,
        &seed,
        seed.workspace_id,
        seed.agent_id,
        &new_app_id(),
    )
    .await;
    let (status, body) = call(
        &app,
        "DELETE",
        &format!(
            "/api/workspaces/{}/lark/installations/{}",
            seed.workspace_id, row.id
        ),
        Some(seed.member),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // workspace owner/admin ⇒ 204，且行还在（状态翻成 revoked）。
    let (status, _) = call(
        &app,
        "DELETE",
        &format!(
            "/api/workspaces/{}/lark/installations/{}",
            seed.workspace_id, row.id
        ),
        Some(seed.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let listed = store
        .list_by_workspace(Id(seed.workspace_id))
        .await
        .expect("list");
    assert!(listed
        .iter()
        .any(|candidate| candidate.id == row.id && candidate.status == "revoked"));

    // 另一个 workspace 的行 ⇒ 404（workspace 收窄）。
    let foreign = persist_installation(
        &state,
        &seed,
        seed.other_workspace_id,
        seed.other_agent_id,
        &new_app_id(),
    )
    .await;
    let (status, _) = call(
        &app,
        "DELETE",
        &format!(
            "/api/workspaces/{}/lark/installations/{}",
            seed.workspace_id, foreign.id
        ),
        Some(seed.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn begin_install_authorizes_agent_owner_and_admins_and_gates_the_region() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let app = configured_app(&state);
    let ws = seed.workspace_id;

    // agent_id 缺 ⇒ 400（在够 Lark 之前）。
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/lark/install/begin"),
        Some(seed.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 未知 agent ⇒ 404。
    let (status, _) = call(
        &app,
        "POST",
        &format!(
            "/api/workspaces/{ws}/lark/install/begin?agent_id={}",
            uuid::Uuid::new_v4()
        ),
        Some(seed.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // region 拼错 ⇒ 400（**不**静默归一到飞书）。
    let (status, body) = call(
        &app,
        "POST",
        &format!(
            "/api/workspaces/{ws}/lark/install/begin?agent_id={}&region=slack",
            seed.agent_id
        ),
        Some(seed.admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 普通成员且不是该 agent 的 owner ⇒ 403（`MUL-4213` 的授权层）。
    let (status, body) = call(
        &app,
        "POST",
        &format!(
            "/api/workspaces/{ws}/lark/install/begin?agent_id={}",
            seed.agent_id
        ),
        Some(seed.member),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
#[ignore = "needs MULTICA_TEST_DATABASE_URL"]
async fn status_route_is_scoped_to_the_initiator_or_an_admin() {
    let db = fixture!();
    let seed = seed(&db).await;
    let state = app_state(db.clone());
    let app = configured_app(&state);
    let ws = seed.workspace_id;

    // 未知会话 ⇒ 404（**不**泄露存在性）。
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/lark/install/sess_missing/status"),
        Some(seed.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 直接在共享表里登记一个会话（设备流要真够得着 Lark，这里只测读侧授权）。
    let sessions: Arc<dyn mc_channel::lark::registration::InstallSessionStore> =
        Arc::new(mc_channel::lark::registration::MemoryInstallSessionStore::new());
    let session_id = mc_channel::lark::registration::random_session_id();
    sessions
        .create(
            mc_channel::lark::registration::InstallSessionState {
                id: session_id.clone(),
                workspace_id: Id(ws),
                initiator_id: Id(seed.member),
                status: mc_channel::lark::registration::SessionStatus::Pending,
                installation_id: None,
                error_reason: String::new(),
                error_message: String::new(),
                expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
            },
            std::time::Duration::from_mins(30),
        )
        .await
        .expect("create session");
    // 注意：路由层自己造的是**另一个** store 实例 ⇒ 这条会话对路由不可见，于是任何调用者
    // 都得到 404。这正是"会话状态必须是共享的"那条教训的本地形态（`docs/32` §30 的 D2）：
    // 单进程实现下，**同一个进程内**不同装配点之间也不共享 ⇒ 生产装配只有一个装配点。
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/lark/install/{session_id}/status"),
        Some(seed.member),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 每个用例一个随机 `app_id`（`UNIQUE(app_id)` 是全局的）。
fn new_app_id() -> String {
    format!("cli_itest_{}", uuid::Uuid::new_v4().simple())
}
