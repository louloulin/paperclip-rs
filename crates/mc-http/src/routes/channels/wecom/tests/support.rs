//! `wecom` 真库用例的共用件（写者 M7-15）：连接、`AppState` 字面量、种子、装置。
//!
//! 与 `mc-http/tests/channels/support.rs`（slack/telegram）同手法；拆出来同时是门 ⑩ 的
//! 800 行硬限要求（`docs/32` §31 的 D8）。

use super::*;
use mc_channel::wecom::credentials::{CredentialProbe, PlaintextSecret, ProbeError};
use mc_channel::wecom::store::PersistInstall;

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
    /// 另一个 workspace 里的一个 agent（跨工作区抢槽位那条用例要它）。
    pub(super) other_agent_id: uuid::Uuid,
    pub(super) admin: uuid::Uuid,
    pub(super) member: uuid::Uuid,
    pub(super) outsider: uuid::Uuid,
    pub(super) bot_id: String,
}

/// 造一个 workspace + 两个成员（owner / member）+ 一个 agent + 另一个 workspace。
pub(super) async fn seed(db: &mc_db::Db) -> Seed {
    async fn new_workspace(db: &mc_db::Db, tag: &str) -> uuid::Uuid {
        sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
            .bind(format!("itest-m715-{tag}"))
            .bind(format!("itest-m715-{tag}-{}", uuid::Uuid::new_v4()))
            .fetch_one(db.pool())
            .await
            .expect("insert workspace")
    }
    async fn new_member(db: &mc_db::Db, workspace_id: uuid::Uuid, role: &str) -> uuid::Uuid {
        let user_id: uuid::Uuid =
            sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
                .bind(format!("itest-m715-{role}"))
                .bind(format!("itest-m715-{}@example.com", uuid::Uuid::new_v4()))
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

    let workspace_id = new_workspace(db, "a").await;
    let other_workspace_id = new_workspace(db, "b").await;
    let admin = new_member(db, workspace_id, "owner").await;
    let member = new_member(db, workspace_id, "member").await;
    let outsider: uuid::Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m715-out', $1) RETURNING id"#,
    )
    .bind(format!(
        "itest-m715-out-{}@example.com",
        uuid::Uuid::new_v4()
    ))
    .fetch_one(db.pool())
    .await
    .expect("insert outsider");
    let runtime_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, owner_id) \
             VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-m715-rt-{}", uuid::Uuid::new_v4()))
    .bind(admin)
    .fetch_one(db.pool())
    .await
    .expect("insert agent_runtime");
    let agent_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
             VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-m715-agent-{}", uuid::Uuid::new_v4()))
    .bind(runtime_id)
    .bind(admin)
    .fetch_one(db.pool())
    .await
    .expect("insert agent");

    // 另一个 workspace 也带一个能用的 agent（跨工作区抢槽位要它）。
    let other_runtime_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, owner_id) \
             VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
    )
    .bind(other_workspace_id)
    .bind(format!("itest-m715-rt-b-{}", uuid::Uuid::new_v4()))
    .bind(admin)
    .fetch_one(db.pool())
    .await
    .expect("insert other agent_runtime");
    let other_agent_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
             VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
    )
    .bind(other_workspace_id)
    .bind(format!("itest-m715-agent-b-{}", uuid::Uuid::new_v4()))
    .bind(other_runtime_id)
    .bind(admin)
    .fetch_one(db.pool())
    .await
    .expect("insert other agent");

    Seed {
        workspace_id,
        other_workspace_id,
        agent_id,
        other_agent_id,
        admin,
        member,
        outsider,
        // 路由槽是**全局**唯一的 ⇒ 每个用例一个随机 bot id，用例之间不会互相占槽。
        bot_id: format!("bot_itest_{}", uuid::Uuid::new_v4().simple()),
    }
}

pub(super) fn boxed() -> mc_secrets::secretbox::SecretBox {
    mc_secrets::secretbox::SecretBox::new(&[5u8; 32]).expect("32 字节密钥")
}

/// 探针替身：直接放行（生产用的是拨号握手，本片的 wire 一半在 M7-16）。
pub(super) struct AcceptingProbe;

#[async_trait::async_trait]
impl CredentialProbe for AcceptingProbe {
    async fn probe(&self, _bot_id: &str, _secret: &PlaintextSecret) -> Result<(), ProbeError> {
        Ok(())
    }
}

pub(super) fn sealed_config(bot_id: &str) -> serde_json::Value {
    let sealed = boxed().seal(PLAINTEXT_SENTINEL.as_bytes()).expect("seal");
    let now = chrono::Utc::now();
    mc_channel::wecom::types::Installation {
        id: Id::nil(),
        workspace_id: Id::nil(),
        agent_id: Id::nil(),
        installer_user_id: Id::nil(),
        status: mc_core::channel::InstallationStatus::Active,
        bot_id: bot_id.to_string(),
        secret_encrypted: sealed,
        bot_display_name: "itest bot".to_string(),
        config: serde_json::Value::Null,
        installed_at: now,
        created_at: now,
        updated_at: now,
    }
    .encode_config()
    .expect("encode")
}

pub(super) fn persist_params(seed: &Seed, bot_id: &str) -> PersistInstall {
    PersistInstall {
        workspace_id: Id(seed.workspace_id),
        agent_id: Id(seed.agent_id),
        installer_user_id: Id(seed.admin),
        bot_id: bot_id.to_string(),
        config: sealed_config(bot_id),
        bot_display_name: "itest bot".to_string(),
    }
}

pub(super) fn app_state(db: mc_db::Db) -> Arc<AppState> {
    state_with_db(
        ChannelKeys::from_env_with(|name| {
            (name == "MULTICA_WECOM_SECRET_KEY").then(|| SECRET_KEY_BASE64.to_string())
        }),
        db,
    )
}
