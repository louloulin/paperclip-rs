//! binding 面：`channel_{user_binding,binding_token}` + lark 两套遗留表。
//!
//! - **写者**：M7-1（**W**；`docs/60-M7-PLAN.md` §3.3）。
//! - **上游**：`db/queries/channel_binding*.sql` + `internal/integrations/{lark,wecom}/binding*.go`。
//! - **语义**：发件人身份绑定（`(installation_id, channel_user_id)` 唯一）与**一次性令牌**的
//!   铸造 / 兑换（`token_hash` 主键、`expires_at <= created_at + 15min`、`consumed_at` 置位即
//!   兑换；**重复兑换不重复插行** —— 幂等靠 `UPDATE ... WHERE consumed_at IS NULL` 的行数）。
//! - **硬约束**：lark 的绑定走**遗留** `lark_{user_binding,binding_token}`（上游同时在用两套表，
//!   `docs/60` §6.4）；泛化层**不得**图省事把 lark 并进来。
//!   ⚠️ 泛化层的 `channel_*` **没有** member 外键（`124` 移除了它）⇒ 成员资格必须由调用方
//!   （身份解析器）**重新校验**，不能拿"行存在"当成员证明。
//! - 行预算（门 ⑩）：≤800 行（本文件约 300 行）。

use chrono::{DateTime, Utc};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_db::Db;
use serde_json::Value as Json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

const USER_BINDING_COLUMNS: &str = "id, workspace_id, multica_user_id, installation_id, \
                                    channel_type, channel_user_id, config, bound_at";
const BINDING_TOKEN_COLUMNS: &str = "token_hash, workspace_id, installation_id, channel_type, \
                                     channel_user_id, expires_at, consumed_at, created_at";

/// `channel_user_binding` 的一行（迁移 `124`）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelUserBindingRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub multica_user_id: Uuid,
    pub installation_id: Uuid,
    pub channel_type: String,
    pub channel_user_id: String,
    pub config: Json,
    pub bound_at: DateTime<Utc>,
}

impl ChannelUserBindingRow {
    /// 绑定的 Multica 用户。
    pub fn multica_user_id(&self) -> Id {
        Id(self.multica_user_id)
    }

    /// 安装 id。
    pub fn installation_id(&self) -> Id {
        Id(self.installation_id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 平台判别式。
    pub fn kind(&self) -> Option<ChannelKind> {
        ChannelKind::from_storage_str(&self.channel_type)
    }

    /// 平台给出的跨安装稳定身份（`config ->> 'union_id'`）。
    pub fn union_id(&self) -> Option<&str> {
        self.config.get("union_id").and_then(Json::as_str)
    }
}

/// `channel_binding_token` 的一行（迁移 `124`）。
///
/// `token_hash` 是**哈希**（明文永不入库）：兑换靠它定位，`consumed_at` 是单次性的判据。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelBindingTokenRow {
    pub token_hash: String,
    pub workspace_id: Uuid,
    pub installation_id: Uuid,
    pub channel_type: String,
    pub channel_user_id: String,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 铸造一枚绑定令牌的入参。
///
/// `token_hash` 由调用方算（上游是 SHA-256 hex；仓储只存**哈希**，明文永远不入库）。
#[derive(Debug, Clone)]
pub struct NewBindingToken {
    pub token_hash: String,
    pub workspace_id: Id,
    pub installation_id: Id,
    pub kind: ChannelKind,
    pub channel_user_id: String,
    /// **必须在 `created_at + 15min` 之内**（DB 的 `CHECK` 会拒更长的寿命）。
    pub expires_at: DateTime<Utc>,
}

/// `lark_user_binding` 的一行（迁移 `109`；遗留表）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct LarkUserBindingRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub multica_user_id: Uuid,
    pub installation_id: Uuid,
    pub lark_open_id: String,
    pub union_id: Option<String>,
    pub bound_at: DateTime<Utc>,
}

/// `lark_binding_token` 的一行（迁移 `109`；遗留表）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct LarkBindingTokenRow {
    pub token_hash: String,
    pub workspace_id: Uuid,
    pub installation_id: Uuid,
    pub lark_open_id: String,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 绑定面仓储。
#[derive(Clone)]
pub struct ChannelBindingRepo {
    db: Db,
}

impl ChannelBindingRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 按 `(安装, 平台用户 id)` 读绑定（身份解析的第一步）。
    pub async fn find_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<Option<ChannelUserBindingRow>> {
        let sql = format!(
            "SELECT {USER_BINDING_COLUMNS} FROM channel_user_binding \
             WHERE installation_id = $1 AND channel_user_id = $2"
        );
        sqlx::query_as::<_, ChannelUserBindingRow>(&sql)
            .bind(installation_id.0)
            .bind(channel_user_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 写一条绑定（已存在则**只更新** `multica_user_id` / `config`：重绑不该换 `id`/`bound_at`）。
    pub async fn upsert_user_binding(
        &self,
        workspace_id: Id,
        multica_user_id: Id,
        installation_id: Id,
        kind: ChannelKind,
        channel_user_id: &str,
        config: Json,
    ) -> Result<ChannelUserBindingRow> {
        let sql = format!(
            "INSERT INTO channel_user_binding \
             (workspace_id, multica_user_id, installation_id, channel_type, channel_user_id, config) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (installation_id, channel_user_id) DO UPDATE \
             SET multica_user_id = EXCLUDED.multica_user_id, config = EXCLUDED.config \
             RETURNING {USER_BINDING_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelUserBindingRow>(&sql)
            .bind(workspace_id.0)
            .bind(multica_user_id.0)
            .bind(installation_id.0)
            .bind(kind.storage_str())
            .bind(channel_user_id)
            .bind(config)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 解绑（成员被移出 workspace 时上游会清绑定）。
    pub async fn delete_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<bool> {
        let affected = sqlx::query(
            "DELETE FROM channel_user_binding \
             WHERE installation_id = $1 AND channel_user_id = $2",
        )
        .bind(installation_id.0)
        .bind(channel_user_id)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .rows_affected();
        Ok(affected > 0)
    }

    /// 铸造一枚令牌（`token_hash` 冲突 = 调用方重复用哈希 ⇒ `RepoError::Conflict`）。
    pub async fn insert_binding_token(
        &self,
        new: NewBindingToken,
    ) -> Result<ChannelBindingTokenRow> {
        let sql = format!(
            "INSERT INTO channel_binding_token \
             (token_hash, workspace_id, installation_id, channel_type, channel_user_id, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING {BINDING_TOKEN_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelBindingTokenRow>(&sql)
            .bind(&new.token_hash)
            .bind(new.workspace_id.0)
            .bind(new.installation_id.0)
            .bind(new.kind.storage_str())
            .bind(&new.channel_user_id)
            .bind(new.expires_at)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// **兑换**令牌：只有"未消费且未过期"的那一次能把 `consumed_at` 置位。
    ///
    /// 返回 `Some(row)` = 这次兑换成功；`None` = 令牌不存在 / 已消费 / 已过期（三者**同**结果：
    /// 不给攻击者区分信号，也不重复插绑定行）。
    pub async fn redeem_binding_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<ChannelBindingTokenRow>> {
        let sql = format!(
            "UPDATE channel_binding_token SET consumed_at = now() \
             WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > now() \
             RETURNING {BINDING_TOKEN_COLUMNS}"
        );
        sqlx::query_as::<_, ChannelBindingTokenRow>(&sql)
            .bind(token_hash)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 读一枚令牌（诊断 / 测试用；兑换走 [`Self::redeem_binding_token`]）。
    pub async fn get_binding_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<ChannelBindingTokenRow>> {
        let sql = format!(
            "SELECT {BINDING_TOKEN_COLUMNS} FROM channel_binding_token WHERE token_hash = $1"
        );
        sqlx::query_as::<_, ChannelBindingTokenRow>(&sql)
            .bind(token_hash)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按 `(安装, lark open_id)` 读**遗留**绑定。
    pub async fn find_lark_user_binding(
        &self,
        installation_id: Id,
        lark_open_id: &str,
    ) -> Result<Option<LarkUserBindingRow>> {
        let sql = "SELECT id, workspace_id, multica_user_id, installation_id, lark_open_id, \
                   union_id, bound_at FROM lark_user_binding \
                   WHERE installation_id = $1 AND lark_open_id = $2";
        sqlx::query_as::<_, LarkUserBindingRow>(sql)
            .bind(installation_id.0)
            .bind(lark_open_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 兑换一枚**遗留** lark 令牌（语义与泛化层逐条一致）。
    pub async fn redeem_lark_binding_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<LarkBindingTokenRow>> {
        let sql = "UPDATE lark_binding_token SET consumed_at = now() \
                   WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > now() \
                   RETURNING token_hash, workspace_id, installation_id, lark_open_id, expires_at, \
                   consumed_at, created_at";
        sqlx::query_as::<_, LarkBindingTokenRow>(sql)
            .bind(token_hash)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for ChannelBindingRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod db_tests {
    //! 绑定面的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。
    //! `channel_*` 无外键 ⇒ fixture 不需要 workspace/member 行。

    use super::*;

    async fn setup() -> Option<(Db, ChannelBindingRepo)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
        Some((db.clone(), ChannelBindingRepo::new(db)))
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

    fn token(installation_id: Id) -> NewBindingToken {
        NewBindingToken {
            token_hash: format!("hash-{}", Uuid::new_v4().simple()),
            workspace_id: Id::new(),
            installation_id,
            kind: ChannelKind::Lark,
            channel_user_id: format!("ou_{}", Uuid::new_v4().simple()),
            expires_at: Utc::now() + chrono::Duration::minutes(15),
        }
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn binding_rows_carry_the_storage_slug_and_round_trip() {
        let (_db, repo) = fixture!();
        let installation_id = Id::new();
        let push_id = format!("ou_{}", Uuid::new_v4().simple());
        let row = repo
            .upsert_user_binding(
                Id::new(),
                Id::new(),
                installation_id,
                ChannelKind::Lark,
                &push_id,
                serde_json::json!({ "union_id": "on_x" }),
            )
            .await
            .expect("upsert");
        assert_eq!(row.channel_type, "feishu");
        assert_eq!(row.kind(), Some(ChannelKind::Lark));
        assert_eq!(row.union_id(), Some("on_x"));

        // 重绑：只换 multica_user_id / config，行 id 与 bound_at 不动。
        let rebound_user = Id::new();
        let again = repo
            .upsert_user_binding(
                row.workspace_id(),
                rebound_user,
                installation_id,
                ChannelKind::Lark,
                &push_id,
                serde_json::json!({ "union_id": "on_y" }),
            )
            .await
            .expect("re-upsert");
        assert_eq!(again.id, row.id);
        assert_eq!(again.multica_user_id(), rebound_user);
        assert_eq!(again.bound_at, row.bound_at);
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn redemption_is_single_use_and_expiry_is_hidden() {
        let (_db, repo) = fixture!();
        let installation_id = Id::new();
        let new = token(installation_id);
        let row = repo.insert_binding_token(new).await.expect("insert");

        let first = repo
            .redeem_binding_token(&row.token_hash)
            .await
            .expect("redeem");
        assert!(first.is_some(), "第一次兑换成功");
        assert!(first.expect("row").consumed_at.is_some());
        // 第二次：与"不存在"同一个结果（不给区分信号）。
        assert!(repo
            .redeem_binding_token(&row.token_hash)
            .await
            .expect("second")
            .is_none());

        // 过期令牌也拿不到。
        let expired = NewBindingToken {
            expires_at: Utc::now() - chrono::Duration::seconds(1),
            ..token(installation_id)
        };
        let expired_row = repo
            .insert_binding_token(expired)
            .await
            .expect("insert expired");
        assert!(repo
            .redeem_binding_token(&expired_row.token_hash)
            .await
            .expect("expired")
            .is_none());
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn duplicate_token_hash_is_a_conflict() {
        let (_db, repo) = fixture!();
        let installation_id = Id::new();
        let new = token(installation_id);
        let hash = new.token_hash.clone();
        repo.insert_binding_token(new).await.expect("insert");
        let clash = NewBindingToken {
            token_hash: hash,
            ..token(installation_id)
        };
        assert!(matches!(
            repo.insert_binding_token(clash).await,
            Err(crate::RepoError::Conflict)
        ));
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn unbinding_removes_the_row() {
        let (_db, repo) = fixture!();
        let installation_id = Id::new();
        let push_id = format!("ou_{}", Uuid::new_v4().simple());
        repo.upsert_user_binding(
            Id::new(),
            Id::new(),
            installation_id,
            ChannelKind::Lark,
            &push_id,
            serde_json::json!({}),
        )
        .await
        .expect("upsert");
        assert!(repo
            .find_user_binding(installation_id, &push_id)
            .await
            .expect("find")
            .is_some());
        assert!(repo
            .delete_user_binding(installation_id, &push_id)
            .await
            .expect("delete"));
        assert!(repo
            .find_user_binding(installation_id, &push_id)
            .await
            .expect("find")
            .is_none());
    }
}
