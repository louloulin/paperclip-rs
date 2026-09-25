//! installation 面：`channel_installation`（泛化安装行）+ `lark_installation`（遗留 per-channel
//! 行，**仍在用**）+ `dingtalk_{bot_identity,group_presence,group_route}`。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`db/queries/channel_installation.sql` 一类的安装行查询 + `internal/integrations/
//!   {lark,dingtalk}/store*.go` 的安装/身份面。
//! - **语义**：安装行的 CRUD / 启停（`status: active|revoked`）/ 配置 JSONB 的补写 / 长连接租约列
//!   （`ws_lease_token`、`ws_lease_expires_at`）的 **CAS 写**；`dingtalk_group_route` 的**行级**
//!   读写（对应路由已退役 ⇒ 不得给它建 HTTP 读面）。
//! - **硬约束**：lark 的安装行必须读/写**遗留** `lark_installation`（上游同时在用两套表，
//!   `docs/60` §6.4）；**不得**把 `lark_*` 并进 `channel_*`。
//! - **本仓约定**：裸 `Uuid` + 手写/derive `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定、jsonb → `serde_json::Value`；写路径必须带 `workspace_id` 收窄的**前置校验**
//!   （跨工作区写 = 越权）。
//! - **`channel_type` 的存储口径**：Lark 存 **`feishu`**（`ChannelKind::storage_str`），
//!   路由前缀才是 `lark` —— 两套字符串**别混**（`docs/32` §10 的 R-M7-10）。
//! - 行预算（门 ⑩）：≤800 行（本文件约 380 行）。

use chrono::{DateTime, Utc};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_db::Db;
use serde_json::Value as Json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

const INSTALLATION_COLUMNS: &str = "id, workspace_id, agent_id, channel_type, config, status, \
                                    ws_lease_token, ws_lease_expires_at, installer_user_id, \
                                    installed_at, created_at, updated_at";
const LARK_INSTALLATION_COLUMNS: &str = "id, workspace_id, agent_id, app_id, \
                                         app_secret_encrypted, tenant_key, bot_open_id, \
                                         bot_union_id, region, installer_user_id, status, \
                                         ws_lease_token, ws_lease_expires_at, installed_at, \
                                         created_at, updated_at";

/// `channel_installation` 的一行（迁移 `124`；**11 列**）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelInstallationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    /// ⚠️ **存储口径**：Lark 是 `feishu`（[`ChannelKind::storage_str`]）。
    pub channel_type: String,
    pub config: Json,
    pub status: String,
    pub ws_lease_token: Option<String>,
    pub ws_lease_expires_at: Option<DateTime<Utc>>,
    pub installer_user_id: Uuid,
    pub installed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ChannelInstallationRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 平台判别式（**解存储口径**：`feishu` → [`ChannelKind::Lark`]）。
    pub fn kind(&self) -> Option<ChannelKind> {
        ChannelKind::from_storage_str(&self.channel_type)
    }

    /// 是否还能承载长连接。
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    /// 平台路由键（`config ->> 'app_id'`；上游唯一索引就在这个表达式上）。
    pub fn app_id(&self) -> Option<&str> {
        self.config.get("app_id").and_then(Json::as_str)
    }
}

/// 插入一行安装的入参（只给**必填**列；其余走列默认值）。
#[derive(Debug, Clone)]
pub struct NewChannelInstallation {
    pub workspace_id: Id,
    pub agent_id: Id,
    pub kind: ChannelKind,
    pub config: Json,
    pub installer_user_id: Id,
}

/// `lark_installation` 的一行（迁移 `109` + `112`/`116`；**16 列**）。
///
/// ⚠️ 这是**遗留表**，与泛化层**并存**：`app_secret_encrypted` 是 `BYTEA`（不是 JSON 里的
/// base64 字符串 —— 那是泛化层的形态）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct LarkInstallationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub app_id: String,
    pub app_secret_encrypted: Vec<u8>,
    pub tenant_key: Option<String>,
    pub bot_open_id: String,
    pub bot_union_id: Option<String>,
    pub region: String,
    pub installer_user_id: Uuid,
    pub status: String,
    pub ws_lease_token: Option<String>,
    pub ws_lease_expires_at: Option<DateTime<Utc>>,
    pub installed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl LarkInstallationRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }
}

/// `dingtalk_bot_identity` 的一行（迁移 `387`…`389` + `474`）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct DingtalkBotIdentityRow {
    pub workspace_id: Uuid,
    pub installation_id: Uuid,
    pub bot_name: String,
    pub bot_identity_issue: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `dingtalk_group_presence` 的一行（迁移 `383`…`386`）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct DingtalkGroupPresenceRow {
    pub workspace_id: Uuid,
    pub installation_id: Uuid,
    pub conversation_id: String,
    pub conversation_title: String,
    pub bot_name: String,
    pub bot_identity_issue: String,
    pub first_seen_at: DateTime<Utc>,
    pub last_active_at: Option<DateTime<Utc>>,
    pub mention_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `dingtalk_group_route` 的一行（迁移 `304`…`307` + `382`）。
///
/// ⚠️ 对应路由**已退役**（`group-routes` 必须保持 404，`docs/60` §1.6）：本结构只给 M7-9 做
/// **行级**读写，**不**为它建 HTTP 读面。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct DingtalkGroupRouteRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub installation_id: Uuid,
    pub conversation_id: String,
    pub conversation_title: String,
    pub agent_id: Uuid,
    pub revision: i64,
    pub discovered_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 安装面仓储。
#[derive(Clone)]
pub struct ChannelInstallationRepo {
    db: Db,
}

impl ChannelInstallationRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 全部**活跃**安装（跨渠道类型；`Supervisor` 的枚举口，`docs/60` §2.4）。
    ///
    /// 顺序按 `created_at, id`（稳定：同一秒创建的行不会随机换位）。
    pub async fn list_active(&self) -> Result<Vec<ChannelInstallationRow>> {
        let sql = format!(
            "SELECT {INSTALLATION_COLUMNS} FROM channel_installation \
             WHERE status = 'active' ORDER BY created_at ASC, id ASC"
        );
        sqlx::query_as::<_, ChannelInstallationRow>(&sql)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 某渠道类型的活跃安装。
    pub async fn list_active_by_kind(
        &self,
        kind: ChannelKind,
    ) -> Result<Vec<ChannelInstallationRow>> {
        let sql = format!(
            "SELECT {INSTALLATION_COLUMNS} FROM channel_installation \
             WHERE status = 'active' AND channel_type = $1 ORDER BY created_at ASC, id ASC"
        );
        sqlx::query_as::<_, ChannelInstallationRow>(&sql)
            .bind(kind.storage_str())
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 单条（不存在 ⇒ `RepoError::NotFound`）。
    pub async fn get(&self, installation_id: Id) -> Result<ChannelInstallationRow> {
        let sql = format!("SELECT {INSTALLATION_COLUMNS} FROM channel_installation WHERE id = $1");
        sqlx::query_as::<_, ChannelInstallationRow>(&sql)
            .bind(installation_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// workspace 收窄的单条（跨工作区读 = 越权 ⇒ 与不存在同一结果）。
    pub async fn get_in_workspace(
        &self,
        workspace_id: Id,
        installation_id: Id,
    ) -> Result<ChannelInstallationRow> {
        let sql = format!(
            "SELECT {INSTALLATION_COLUMNS} FROM channel_installation \
             WHERE id = $1 AND workspace_id = $2"
        );
        sqlx::query_as::<_, ChannelInstallationRow>(&sql)
            .bind(installation_id.0)
            .bind(workspace_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 平台路由键 → 活跃安装（上游 `idx_channel_installation_type_appid` 的表达式；
    /// 多行命中是**实现错误**（索引保证），这里只取一行并留给调用方判）。
    pub async fn find_active_by_app_id(
        &self,
        kind: ChannelKind,
        app_id: &str,
    ) -> Result<Option<ChannelInstallationRow>> {
        let sql = format!(
            "SELECT {INSTALLATION_COLUMNS} FROM channel_installation \
             WHERE channel_type = $1 AND config ->> 'app_id' = $2 AND status = 'active' \
             ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, ChannelInstallationRow>(&sql)
            .bind(kind.storage_str())
            .bind(app_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 插入一行安装（`status` 默认 `active`）。
    pub async fn insert(&self, new: NewChannelInstallation) -> Result<ChannelInstallationRow> {
        let sql = format!(
            "INSERT INTO channel_installation \
             (workspace_id, agent_id, channel_type, config, installer_user_id) \
             VALUES ($1, $2, $3, $4, $5) RETURNING {INSTALLATION_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelInstallationRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(new.agent_id.0)
            .bind(new.kind.storage_str())
            .bind(new.config)
            .bind(new.installer_user_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 撤销（`active → revoked`；`UNIQUE(workspace_id, agent_id, channel_type)` 因此被让出）。
    pub async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE channel_installation SET status = 'revoked', updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 AND status = 'active'",
        )
        .bind(installation_id.0)
        .bind(workspace_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 长连接租约的 **CAS 获取**（上游 `TryAcquireWSLease`）：
    /// 无主 / 已过期 / 令牌相同（同一持有者的安全重试）才授予，返回是否到手。
    pub async fn try_acquire_ws_lease(
        &self,
        installation_id: Id,
        token: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE channel_installation \
             SET ws_lease_token = $2, ws_lease_expires_at = $3, updated_at = now() \
             WHERE id = $1 AND status = 'active' \
               AND (ws_lease_token IS NULL OR ws_lease_token = $2 \
                    OR ws_lease_expires_at IS NULL OR ws_lease_expires_at <= now())",
        )
        .bind(installation_id.0)
        .bind(token)
        .bind(expires_at)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 续租（**仅**当当前令牌等于 `token`）。
    pub async fn renew_ws_lease(
        &self,
        installation_id: Id,
        token: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE channel_installation SET ws_lease_expires_at = $3, updated_at = now() \
             WHERE id = $1 AND ws_lease_token = $2",
        )
        .bind(installation_id.0)
        .bind(token)
        .bind(expires_at)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 释放（**仅**当当前令牌等于 `token`；迟到的释放不能清掉后继者的租约）。
    pub async fn release_ws_lease(&self, installation_id: Id, token: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE channel_installation \
             SET ws_lease_token = NULL, ws_lease_expires_at = NULL, updated_at = now() \
             WHERE id = $1 AND ws_lease_token = $2",
        )
        .bind(installation_id.0)
        .bind(token)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 按 `(kind, app_id)` 读**遗留** `lark_installation`（上游同时在用两套表）。
    pub async fn find_lark_by_app_id(&self, app_id: &str) -> Result<Option<LarkInstallationRow>> {
        let sql = format!(
            "SELECT {LARK_INSTALLATION_COLUMNS} FROM lark_installation \
             WHERE app_id = $1 AND status = 'active' ORDER BY created_at ASC"
        );
        sqlx::query_as::<_, LarkInstallationRow>(&sql)
            .bind(app_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 读一条遗留 lark 安装行。
    pub async fn get_lark(&self, installation_id: Id) -> Result<LarkInstallationRow> {
        let sql =
            format!("SELECT {LARK_INSTALLATION_COLUMNS} FROM lark_installation WHERE id = $1");
        sqlx::query_as::<_, LarkInstallationRow>(&sql)
            .bind(installation_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 撤销遗留 lark 安装行。
    pub async fn revoke_lark(&self, workspace_id: Id, installation_id: Id) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE lark_installation SET status = 'revoked', updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 AND status = 'active'",
        )
        .bind(installation_id.0)
        .bind(workspace_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();
        Ok(affected > 0)
    }

    /// dingtalk 的 workspace 群路由（行级读；**没有** HTTP 读面，`docs/60` §1.6）。
    pub async fn list_dingtalk_group_routes(
        &self,
        workspace_id: Id,
        agent_id: Option<Id>,
    ) -> Result<Vec<DingtalkGroupRouteRow>> {
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, workspace_id, installation_id, conversation_id, conversation_title, \
             agent_id, revision, discovered_at, updated_at FROM dingtalk_group_route \
             WHERE workspace_id = ",
        );
        builder.push_bind(workspace_id.0);
        if let Some(agent_id) = agent_id {
            builder.push(" AND agent_id = ");
            builder.push_bind(agent_id.0);
        }
        builder.push(" ORDER BY conversation_id ASC");
        builder
            .build_query_as::<DingtalkGroupRouteRow>()
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for ChannelInstallationRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod db_tests {
    //! 安装面的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。
    //!
    //! 未设置变量 → 打印跳过并 `return`；**已设置但连不上 / 没建表 → panic**（不许静默假装绿）。
    //! `channel_installation` 没有外键（`124` 的硬规则）⇒ fixture 不需要造 workspace/agent。

    use super::*;

    async fn setup() -> Option<(Db, ChannelInstallationRepo)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
        Some((db.clone(), ChannelInstallationRepo::new(db)))
    }

    macro_rules! fixture {
        () => {
            match setup().await {
                Some(v) => v,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }

    fn new_installation(kind: ChannelKind, app_id: &str) -> NewChannelInstallation {
        NewChannelInstallation {
            workspace_id: Id::new(),
            agent_id: Id::new(),
            kind,
            config: serde_json::json!({ "app_id": app_id, "region": "feishu" }),
            installer_user_id: Id::new(),
        }
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn lark_rows_are_stored_under_the_feishu_storage_slug() {
        let (_db, repo) = fixture!();
        let app_id = format!("cli_itest_{}", Uuid::new_v4().simple());
        let row = repo
            .insert(new_installation(ChannelKind::Lark, &app_id))
            .await
            .expect("insert");
        assert_eq!(row.channel_type, "feishu", "存储口径是 feishu，不是 lark");
        assert_eq!(row.kind(), Some(ChannelKind::Lark));
        assert!(row.is_active());
        assert_eq!(row.app_id(), Some(app_id.as_str()));

        // 路由键查找只命中活跃行。
        let found = repo
            .find_active_by_app_id(ChannelKind::Lark, &app_id)
            .await
            .expect("find")
            .expect("命中");
        assert_eq!(found.id, row.id);
        assert!(repo
            .find_active_by_app_id(ChannelKind::Slack, &app_id)
            .await
            .expect("find")
            .is_none());
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn revoke_is_idempotent_and_leaves_the_row() {
        let (_db, repo) = fixture!();
        let app_id = format!("cli_itest_{}", Uuid::new_v4().simple());
        let row = repo
            .insert(new_installation(ChannelKind::Lark, &app_id))
            .await
            .expect("insert");
        assert!(repo
            .revoke(row.workspace_id(), row.id())
            .await
            .expect("revoke"));
        assert!(!repo
            .revoke(row.workspace_id(), row.id())
            .await
            .expect("revoke"));
        let after = repo.get(row.id()).await.expect("row 仍在（撤销不是删除）");
        assert_eq!(after.status, "revoked");
        assert!(repo
            .find_active_by_app_id(ChannelKind::Lark, &app_id)
            .await
            .expect("find")
            .is_none());
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn lease_cas_only_grants_the_free_or_expired_or_same_token() {
        let (_db, repo) = fixture!();
        let app_id = format!("cli_itest_{}", Uuid::new_v4().simple());
        let row = repo
            .insert(new_installation(ChannelKind::Lark, &app_id))
            .await
            .expect("insert");
        let future = Utc::now() + chrono::Duration::seconds(180);
        assert!(repo
            .try_acquire_ws_lease(row.id(), "node-a", future)
            .await
            .expect("acquire"));
        // 同一令牌 = 安全重试。
        assert!(repo
            .try_acquire_ws_lease(row.id(), "node-a", future)
            .await
            .expect("renew by same token"));
        // 另一副本在有效期内拿不到。
        assert!(!repo
            .try_acquire_ws_lease(row.id(), "node-b", future)
            .await
            .expect("contended"));
        // 持有者死在租约过期之后 ⇒ 另一副本可接管：先把租约写成已过期。
        let past = Utc::now() - chrono::Duration::seconds(1);
        assert!(repo
            .try_acquire_ws_lease(row.id(), "node-a", past)
            .await
            .expect("expire own lease"));
        assert!(repo
            .try_acquire_ws_lease(row.id(), "node-b", future)
            .await
            .expect("expired takeover"));
        // 令牌围栏：迟到的释放不能清掉别人的租约。
        assert!(!repo
            .release_ws_lease(row.id(), "node-a")
            .await
            .expect("stale release"));
        assert!(repo
            .release_ws_lease(row.id(), "node-b")
            .await
            .expect("owner release"));
        assert!(repo
            .try_acquire_ws_lease(row.id(), "node-c", future)
            .await
            .expect("acquire after release"));
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn active_listing_is_scoped_and_ordered() {
        let (_db, repo) = fixture!();
        let kind = ChannelKind::Telegram;
        let marker = format!("itest-{}", Uuid::new_v4().simple());
        let first = repo
            .insert(NewChannelInstallation {
                config: serde_json::json!({ "bot_id": marker }),
                ..new_installation(kind, &marker)
            })
            .await
            .expect("insert");
        let listed = repo.list_active_by_kind(kind).await.expect("list");
        assert!(listed.iter().any(|row| row.id == first.id));
        let all = repo.list_active().await.expect("list all");
        assert!(all.iter().any(|row| row.id == first.id));
        // 撤销后不在活跃列表里。
        repo.revoke(first.workspace_id(), first.id())
            .await
            .expect("revoke");
        let listed = repo.list_active_by_kind(kind).await.expect("list");
        assert!(!listed.iter().any(|row| row.id == first.id));
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn workspace_scoped_read_hides_other_workspaces() {
        let (_db, repo) = fixture!();
        let app_id = format!("cli_itest_{}", Uuid::new_v4().simple());
        let row = repo
            .insert(new_installation(ChannelKind::Lark, &app_id))
            .await
            .expect("insert");
        assert!(repo
            .get_in_workspace(row.workspace_id(), row.id())
            .await
            .is_ok());
        let other = Id::new();
        assert!(matches!(
            repo.get_in_workspace(other, row.id()).await,
            Err(crate::RepoError::NotFound)
        ));
    }
}
