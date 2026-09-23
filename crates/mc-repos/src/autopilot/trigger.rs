//! `autopilot_trigger` 仓储（写面 + token 铸造）。
//!
//! - **写者**：M5-3（**W**；`docs/44` §3.2）。M5-5 读（webhook ingress 按 token 解析 trigger）。
//! - **上游 SQL**：`db/queries/autopilot.sql` 的 trigger 查询（`CreateAutopilotTrigger` /
//!   `UpdateAutopilotTrigger` / `DeleteAutopilotTrigger` / `RotateAutopilotTriggerWebhookToken` /
//!   `SetAutopilotTriggerSigningSecret`）。
//! - **要落的写点**：create / update / delete + `webhook_token` 轮换 + `signing_secret` 写入。
//! - **token 唯一性**：token 唯一冲突要能被上层识别并重试（`createWebhookTriggerWithMintedToken`56）
//!   ⇒ 错误映射要区分「唯一冲突」与其它 DB 错误。
//! - **列注意**：`kind ∈ {schedule, webhook, api}`、`provider ∈ {generic, github}`；
//!   `timezone TEXT NULL DEFAULT 'UTC'`（**可空**，与 `issue_wakeup.timezone` 的 NOT NULL 不同）；
//!   `created_by_*` / `published_by_*` **没有 CHECK**（约定 `member|agent`）。
//!
//! # 本文件**不**实现的两条查询（「一格一写者」裁决，`docs/44` §3.2）
//!
//! | 上游查询 | 归谁 | 为什么不是本片 |
//! | --- | --- | --- |
//! | `GetAutopilotTriggerForAutopilot`（绑 `autopilot_id` + workspace join） | **M5-4** | 它是「解析 run 的授权主体」用的（MUL-6951）；M5-3 的写面已经在 handler 里拿 `autopilot_id` 显式比对，不需要第二个真值 |
//! | `GetWebhookTriggerByToken` | **M5-5** | 无认证 ingress 的入口查询，属 ingress 面 |
//!
//! 本文件的 `get_by_id` 就是上游 `GetAutopilotTrigger`（`WHERE id = $1`，**不带** autopilot 绑定），
//! 调用方必须自己比对 `row.autopilot_id`（上游 handler 逐字如此：`prev.AutopilotID != ap.ID → 404`）。
//!
//! # token 形态（跨片不变量，别改）
//!
//! `createWebhookTriggerWithMintedToken`(56) 的 token 由 [`generate_webhook_token`] 铸造：
//! **`awt_` 前缀 + 32 字节随机 → 43 字符 URL-safe 无 padding base64 = 47 字符**，与
//! `crates/mc-repos/src/invitation.rs` 的既有形态同源。它同时决定公开入口路径
//! `webhookPathForToken`(4) ⇒ `/api/webhooks/autopilots/{token}`（M5-5 的 ingress 逐字依赖）。
//!
//! **为什么 token 在本 crate 而不在 `mc-autopilot`**：`mc-autopilot` 的 anchor 依赖表
//! （`docs/44` §5.2）里**没有** `rand` / `base64`，且禁止新增三方依赖；`mc-repos` 两个都有。
//!
//! # 事务边界（逐字对照上游）
//!
//! | 写点 | 上游 | 本地 |
//! | --- | --- | --- |
//! | create / update / delete | 在 handler 的事务里（与 `autopilot_rule_version` 同事务） | 接 `&mut PgConnection` |
//! | rotate / `set_signing_secret` | **无事务**（单语句 autocommit） | 接 `&PgPool` |
//!
//! # 已知缺口（**跨片登记**，见 `docs/46` 同族的 §5 偏离表）
//!
//! 上游 create / update(实质变更) / delete 三条路径都会在**同一事务**里追加
//! `autopilot_rule_version`（`recordAutopilotRuleVersion`，MUL-4302），update 还要
//! `SetAutopilotTriggerPublisher` 重盖 `published_by_*`。`docs/44` §4.2 把
//! `autopilot_rule_version` 落库整段判给 **M5-2**（`autopilot/write.rs`），而 C 波
//! `M5-2 ∥ M5-3` 并行 ⇒ 本片只播种 `published_by_*` / `created_by_*`（INSERT 时不可避免的列值，
//! 逐字对齐上游），**不**写版本、**不**重盖 publisher。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use mc_db::Db;

use super::AUTOPILOT_TRIGGER_COLUMNS;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb};

/// webhook token 前缀（上游 `generateWebhookToken`；`awt` = autopilot webhook token）。
pub const WEBHOOK_TOKEN_PREFIX: &str = "awt_";

/// token 的随机字节数（256 bit ⇒ base64 后 43 字符）。
pub const WEBHOOK_TOKEN_BYTES: usize = 32;

/// `createWebhookTriggerWithMintedToken` 的重试上限（上游 `for attempt := 0; attempt < 3`）。
///
/// 冲突概率在真随机源下是 2⁻²⁵⁶，这个上限的存在只为「RNG 退化时现象明显」而不是吞吐。
pub const WEBHOOK_TOKEN_ATTEMPTS: usize = 3;

/// 上游 `generateWebhookToken`，逐字等价：`awt_` + 43 字符 URL-safe 无 padding base64。
///
/// 前缀是**契约**（公开 URL 里看得见），改它会让存量 webhook 地址全部失效。
#[must_use]
pub fn generate_webhook_token() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngCore;

    let mut bytes = [0u8; WEBHOOK_TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{WEBHOOK_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// `CreateAutopilotTrigger` 的入参（`sqlc.narg` 的每一格都在这里体现为 `Option`）。
///
/// 三处**不能**顺手「优化」的地方：
///
/// 1. `provider` 是 `Option` 且 SQL 里带 `COALESCE($n, 'generic')` —— schedule 路径上游**不绑**
///    这个参数（走 COALESCE 落 `generic`），webhook 路径永远绑。
/// 2. `event_filters` 为 `None` 时落 **NULL**（不是 `[]`）：上游 `encodeWebhookEventFilters` 明确
///    「nil/empty → nil bytes」，只有 update 的清除路径用 `encodeWebhookEventFiltersAlways` 落 `[]`。
/// 3. `label` **不做空串折叠**：上游 `ptrToText` 把非 nil 指针（含 `""`）都落成合法值，
///    `""` 存 `''`、`nil` 存 NULL。别顺手 `.filter(|s| !s.is_empty())`。
#[derive(Debug, Clone)]
pub struct NewTrigger<'a> {
    /// 归属自动机。
    pub autopilot_id: Uuid,
    /// `schedule` / `webhook`（`api` 已被上游 400 拒绝，写面不会落）。
    pub kind: &'a str,
    /// 创建路径恒 `true`（上游 `Enabled: true`）。
    pub enabled: bool,
    /// schedule 的 cron 表达式。
    pub cron_expression: Option<&'a str>,
    /// schedule 的时区（**原样**落库：`nil → NULL`、`"" → ''`）。
    pub timezone: Option<&'a str>,
    /// `computeNextRun` 的结果；无下次触发时本地落 NULL（见 handler 的偏离说明）。
    pub next_run_at: Option<DateTime<Utc>>,
    /// webhook 的明文 token（schedule 路径恒 `None`）。
    pub webhook_token: Option<&'a str>,
    /// 展示名（`nil → NULL`、`"" → ''`）。
    pub label: Option<&'a str>,
    /// `generic` / `github`；`None` ⇒ SQL 的 COALESCE 落 `generic`。
    pub provider: Option<&'a str>,
    /// 事件过滤（序列化后的 JSONB）；`None` ⇒ NULL。
    pub event_filters: Option<serde_json::Value>,
    /// 配置责任人（`published_by_*`）＝**不可变授权主体**（`created_by_*`）＝调用成员。
    pub actor_id: Uuid,
}

/// `UpdateAutopilotTrigger` 的入参。
///
/// ⚠️ `next_run_at` 在 SQL 里是 `= sqlc.narg(...)`，**没有 COALESCE** ⇒ 它总是被绑定，
/// `None` 就是「落 NULL」。所以 handler 必须像上游那样先把它初始化成**上一行的值**再决定是否重算，
/// 否则一次普通 PATCH 会把 `next_run_at` 抹成 NULL。
#[derive(Debug, Clone)]
pub struct TriggerPatch<'a> {
    /// 目标行。
    pub id: Uuid,
    /// `enabled`（`None` ⇒ COALESCE 保留）。
    pub enabled: Option<bool>,
    /// `cron_expression`（`None` ⇒ 保留）。
    pub cron_expression: Option<&'a str>,
    /// `timezone`（`None` ⇒ 保留；`Some("")` ⇒ 落 `''`）。
    pub timezone: Option<&'a str>,
    /// **总是绑定**（见上）。
    pub next_run_at: Option<DateTime<Utc>>,
    /// `label`（`None` ⇒ 保留）。
    pub label: Option<&'a str>,
    /// `event_filters`：`None` ⇒ 保留原值；`Some([])` ⇒ 清成 `'[]'::jsonb`；`Some([...])` ⇒ 替换。
    pub event_filters: Option<serde_json::Value>,
}

/// `autopilot_trigger` 写面仓储。
///
/// 与 M5-1 的 [`super::AutopilotRepo`] 分开是「一格一写者」的直接后果：`mod.rs` 判给 M5-1，
/// 本片的查询只能落在本文件里，所以另起一个类型而不是往 `AutopilotRepo` 加方法。
#[derive(Debug, Clone)]
pub struct AutopilotTriggerRepo {
    db: Db,
}

impl RepoWithDb for AutopilotTriggerRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl AutopilotTriggerRepo {
    /// 从应用共享 `Db` 句柄构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `GetAutopilotTrigger`：`WHERE id = $1`，**不绑** autopilot。
    ///
    /// 调用方必须自己比对 `row.autopilot_id`（上游 handler 逐字如此）。要拿「绑定 autopilot +
    /// workspace」的那条（MUL-6951 的授权主体解析）请等 M5-4 的 `GetAutopilotTriggerForAutopilot`。
    ///
    /// # Errors
    ///
    /// 行不存在 → [`RepoError::NotFound`]（handler 折 404 `trigger not found`）。
    pub async fn get_by_id(&self, id: Uuid) -> Result<super::AutopilotTriggerRow, RepoError> {
        let sql =
            format!("SELECT {AUTOPILOT_TRIGGER_COLUMNS} FROM autopilot_trigger WHERE id = $1");
        sqlx::query_as::<_, super::AutopilotTriggerRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?
            .ok_or(RepoError::NotFound)
    }

    /// 上游 `RotateAutopilotTriggerWebhookToken`：`WHERE id = $1 AND kind = 'webhook'`。
    ///
    /// `kind` 限制让误调 schedule / api 触发器变成 **0 行**（本地 `RowNotFound` → `NotFound`），
    /// 而不是改坏无关状态。上游这条**不走事务**（单语句 autocommit），本地照做。
    ///
    /// # Errors
    ///
    /// token 撞唯一索引 → [`RepoError::Conflict`]（handler 据此换 token 重试）。
    pub async fn rotate_webhook_token(
        &self,
        id: Uuid,
        token: &str,
    ) -> Result<super::AutopilotTriggerRow, RepoError> {
        let sql = format!(
            "UPDATE autopilot_trigger SET webhook_token = $2, updated_at = now() \
             WHERE id = $1 AND kind = 'webhook' \
             RETURNING {AUTOPILOT_TRIGGER_COLUMNS}"
        );
        sqlx::query_as::<_, super::AutopilotTriggerRow>(&sql)
            .bind(id)
            .bind(token)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `SetAutopilotTriggerSigningSecret`：`WHERE id = $1 AND kind = 'webhook'`。
    ///
    /// `secret = None` ⇒ `signing_secret = NULL` ⇒ **清除**（回落到「只验 bearer token」）。
    /// 这条刻意不挂在 `UpdateAutopilotTrigger` 上（上游注释：让请求体里只有 secret 一个字段，
    /// 免得被「记录 PATCH 体」的通用日志顺手抄走）。同样**不走事务**。
    ///
    /// # Errors
    ///
    /// 行不存在 / 非 webhook → [`RepoError::NotFound`]；DB 故障 → [`RepoError::Db`]。
    pub async fn set_signing_secret(
        &self,
        id: Uuid,
        secret: Option<&str>,
    ) -> Result<super::AutopilotTriggerRow, RepoError> {
        let sql = format!(
            "UPDATE autopilot_trigger SET signing_secret = $2, updated_at = now() \
             WHERE id = $1 AND kind = 'webhook' \
             RETURNING {AUTOPILOT_TRIGGER_COLUMNS}"
        );
        sqlx::query_as::<_, super::AutopilotTriggerRow>(&sql)
            .bind(id)
            .bind(secret)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 底层连接池（真库测试的种子 SQL 直接用它，不经过本仓储）。
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        self.db.pool()
    }

    /// 用「本仓储的 pool」建事务（M5-3 的两条 create 路径各要一次）。
    ///
    /// # Errors
    ///
    /// 取连接失败 → [`RepoError::Db`]。
    pub async fn begin(&self) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, RepoError> {
        self.db
            .pool()
            .begin()
            .await
            .map_err(|err| RepoError::Db(err.to_string()))
    }
}

/// 上游 `CreateAutopilotTrigger`（写面；**接 `&mut PgConnection` 以便与版本写入同事务**）。
///
/// 列清单与上游逐字一致，`provider` 走 `COALESCE($10, 'generic')`、其余 `sqlc.narg` 列走 `$n`。
///
/// # Errors
///
/// `webhook_token` 撞部分唯一索引 `idx_autopilot_trigger_webhook_token` → [`RepoError::Conflict`]
/// （**这是重试信号**，上游 `isUniqueViolation` 把 `23505` 一律当 token 冲突：这条 INSERT 上
/// 除主键外只有那一个唯一约束，所以两者不可区分也不需要区分）。
pub async fn create_trigger(
    conn: &mut sqlx::PgConnection,
    new: &NewTrigger<'_>,
) -> Result<super::AutopilotTriggerRow, RepoError> {
    let sql = format!(
        "INSERT INTO autopilot_trigger \
            (autopilot_id, kind, enabled, cron_expression, timezone, next_run_at, webhook_token, \
             label, provider, event_filters, published_by_type, published_by_id, \
             created_by_type, created_by_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, COALESCE($9::text, 'generic'), $10, \
                 $11, $12, $13, $14) \
         RETURNING {AUTOPILOT_TRIGGER_COLUMNS}"
    );
    sqlx::query_as::<_, super::AutopilotTriggerRow>(&sql)
        .bind(new.autopilot_id)
        .bind(new.kind)
        .bind(new.enabled)
        .bind(new.cron_expression)
        .bind(new.timezone)
        .bind(new.next_run_at)
        .bind(new.webhook_token)
        .bind(new.label)
        .bind(new.provider)
        .bind(new.event_filters.clone())
        // 上游 `Valid: publisherID.Valid`：走到这里 actor 一定是本工作区成员 ⇒ 恒 `member`。
        .bind("member")
        .bind(new.actor_id)
        .bind("member")
        .bind(new.actor_id)
        .fetch_one(conn)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `UpdateAutopilotTrigger`（**接 `&mut PgConnection`**，与规则版本写入同事务）。
///
/// # Errors
///
/// 行不存在 → [`RepoError::NotFound`]；DB 故障 → [`RepoError::Db`]。
pub async fn update_trigger(
    conn: &mut sqlx::PgConnection,
    patch: &TriggerPatch<'_>,
) -> Result<super::AutopilotTriggerRow, RepoError> {
    let sql = format!(
        "UPDATE autopilot_trigger SET \
            enabled = COALESCE($2::boolean, enabled), \
            cron_expression = COALESCE($3, cron_expression), \
            timezone = COALESCE($4, timezone), \
            next_run_at = $5, \
            label = COALESCE($6, label), \
            event_filters = COALESCE($7, event_filters), \
            updated_at = now() \
         WHERE id = $1 \
         RETURNING {AUTOPILOT_TRIGGER_COLUMNS}"
    );
    sqlx::query_as::<_, super::AutopilotTriggerRow>(&sql)
        .bind(patch.id)
        .bind(patch.enabled)
        .bind(patch.cron_expression)
        .bind(patch.timezone)
        .bind(patch.next_run_at)
        .bind(patch.label)
        .bind(patch.event_filters.clone())
        .fetch_one(conn)
        .await
        .map_err(map_sqlx_err)
}

/// 上游 `DeleteAutopilotTrigger`（**接 `&mut PgConnection`**，与规则版本写入同事务）。
///
/// # Errors
///
/// DB 故障 → [`RepoError::Db`]。删 0 行**不算错**（上游 `:exec` 同样不检查 `RowsAffected`；
/// 存在性已由调用方的 `get_by_id` 保证）。
pub async fn delete_trigger(conn: &mut sqlx::PgConnection, id: Uuid) -> Result<(), RepoError> {
    sqlx::query("DELETE FROM autopilot_trigger WHERE id = $1")
        .bind(id)
        .execute(conn)
        .await
        .map_err(map_sqlx_err)
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// token 形态是**跨片契约**（`webhookPathForToken` 的路径参数 + M5-5 ingress）：
    /// `awt_` 前缀 + 43 字符 URL-safe base64 ⇒ 定长 47，且不含 `/`、`+`（URL 路径里安全）。
    #[test]
    fn token_shape_is_url_safe_and_fixed_length() {
        let token = generate_webhook_token();
        assert!(token.starts_with(WEBHOOK_TOKEN_PREFIX), "{token}");
        assert_eq!(token.len(), 47, "{token}");
        let body = &token[WEBHOOK_TOKEN_PREFIX.len()..];
        assert_eq!(body.len(), 43);
        assert!(
            body.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "非 URL-safe 字符: {body}"
        );
        assert!(!token.contains('/') && !token.contains('+') && !token.contains('='));
    }

    /// 随机性冒烟：1000 抽不得重复（退化 RNG 必须被这条抓住）。
    #[test]
    fn tokens_do_not_repeat() {
        let set: std::collections::HashSet<String> =
            (0..1000).map(|_| generate_webhook_token()).collect();
        assert_eq!(set.len(), 1000);
    }

    /// 重试上限是上游常量（别在别处再写一个 3）。
    #[test]
    fn attempts_match_upstream() {
        assert_eq!(WEBHOOK_TOKEN_ATTEMPTS, 3);
        assert_eq!(WEBHOOK_TOKEN_BYTES, 32);
    }
}
