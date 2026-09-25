//! `lark` 真库用例的共用件（写者 M7-14）：连接、`AppState` 字面量、种子、装置。
//!
//! 与 `mc-http/tests/channels/support.rs`（slack/telegram）及 `wecom/tests/support.rs` 同手法；
//! 拆出来同时是门 ⑩ 的 800 行硬限要求（`docs/32` §30 的 **D10**）。

use super::*;

pub(super) async fn pool() -> Option<mc_db::Db> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    Some(
        mc_db::Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}")),
    )
}

macro_rules! fixture {
    () => {
        match pool().await {
            Some(db) => db,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}
// 跨模块可见（`db.rs` 用 `use super::support::*;` 取它）：`macro_rules!` 的文本作用域
// 不跨模块，所以要把名字再导出一次。
pub(super) use fixture;

pub(super) struct Seed {
    pub(super) workspace_id: uuid::Uuid,
    pub(super) other_workspace_id: uuid::Uuid,
    pub(super) agent_id: uuid::Uuid,
    pub(super) other_agent_id: uuid::Uuid,
    pub(super) admin: uuid::Uuid,
    pub(super) member: uuid::Uuid,
    pub(super) outsider: uuid::Uuid,
    /// `UNIQUE(app_id)` 是**全局**的 ⇒ 每个用例一个随机 `app_id`，用例之间不会互相占槽。
    pub(super) app_id: String,
}

/// 造一个 workspace + 三个用户（owner / member / 外人）+ 两个 agent（各一个 workspace）。
pub(super) async fn seed(db: &mc_db::Db) -> Seed {
    async fn new_workspace(db: &mc_db::Db, tag: &str) -> uuid::Uuid {
        sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
            .bind(format!("itest-m714-{tag}"))
            .bind(format!("itest-m714-{tag}-{}", uuid::Uuid::new_v4()))
            .fetch_one(db.pool())
            .await
            .expect("insert workspace")
    }
    async fn new_member(db: &mc_db::Db, workspace_id: uuid::Uuid, role: &str) -> uuid::Uuid {
        let user_id: uuid::Uuid =
            sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
                .bind(format!("itest-m714-{role}"))
                .bind(format!("itest-m714-{}@example.com", uuid::Uuid::new_v4()))
                .fetch_one(db.pool())
                .await
                .expect("insert user");
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(workspace_id)
            .bind(user_id)
            .bind(role)
            .execute(db.pool())
            .await
            .expect("insert member");
        user_id
    }
    async fn new_agent(
        db: &mc_db::Db,
        workspace_id: uuid::Uuid,
        owner_id: uuid::Uuid,
        tag: &str,
    ) -> uuid::Uuid {
        let runtime_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, owner_id) \
                 VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
        )
        .bind(workspace_id)
        .bind(format!("itest-m714-rt-{tag}-{}", uuid::Uuid::new_v4()))
        .bind(owner_id)
        .fetch_one(db.pool())
        .await
        .expect("insert agent_runtime");
        sqlx::query_scalar(
            "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
                 VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
        )
        .bind(workspace_id)
        .bind(format!("itest-m714-agent-{tag}-{}", uuid::Uuid::new_v4()))
        .bind(runtime_id)
        .bind(owner_id)
        .fetch_one(db.pool())
        .await
        .expect("insert agent")
    }

    let workspace_id = new_workspace(db, "a").await;
    let other_workspace_id = new_workspace(db, "b").await;
    let admin = new_member(db, workspace_id, "owner").await;
    let member = new_member(db, workspace_id, "member").await;
    let outsider: uuid::Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m714-out', $1) RETURNING id"#,
    )
    .bind(format!(
        "itest-m714-out-{}@example.com",
        uuid::Uuid::new_v4()
    ))
    .fetch_one(db.pool())
    .await
    .expect("insert outsider");

    let agent_id = new_agent(db, workspace_id, admin, "a").await;
    let other_agent_id = new_agent(db, other_workspace_id, admin, "b").await;

    Seed {
        workspace_id,
        other_workspace_id,
        agent_id,
        other_agent_id,
        admin,
        member,
        outsider,
        app_id: format!("cli_itest_{}", uuid::Uuid::new_v4().simple()),
    }
}

/// 本片测试用的封装盒（与 `app_state` 注入的部署密钥同源）。
pub(super) fn boxed() -> mc_secrets::secretbox::SecretBox {
    mc_secrets::secretbox::SecretBox::new(&[7_u8; 32]).expect("32 字节密钥")
}

/// 装了 lark 落库密钥的整站状态。
pub(super) fn app_state(db: mc_db::Db) -> Arc<AppState> {
    state_with_db(
        ChannelKeys::from_env_with(|name| {
            (name == "MULTICA_LARK_SECRET_KEY").then(|| SECRET_KEY_BASE64.to_string())
        }),
        db,
    )
}

/// 直接经端口落一条安装（绕开设备流 —— 它要真的够得着 Lark）。
pub(super) async fn persist_installation(
    state: &Arc<AppState>,
    seed: &Seed,
    workspace_id: uuid::Uuid,
    agent_id: uuid::Uuid,
    app_id: &str,
) -> mc_channel::lark::installation::Installation {
    use mc_channel::lark::installation::InstallationParams;
    use mc_channel::lark::types::OpenId;

    let service = InstallationService::new(
        Arc::new(super::super::store::PgLarkInstallStore::new(Arc::clone(
            state,
        ))),
        boxed(),
    );
    let params = InstallationParams::new(
        Id(workspace_id),
        Id(agent_id),
        app_id,
        "itest-app-secret",
        OpenId::new(format!("ou_bot_{}", uuid::Uuid::new_v4().simple())),
        Id(seed.admin),
    );
    service.upsert(&params).await.expect("upsert installation")
}
