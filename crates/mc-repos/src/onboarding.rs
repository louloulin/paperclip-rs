//! `OnboardingRepo` —— onboarding 的 user 列读写（**写者 M9-3** / `docs/62` §3.3）。
//!
//! # 上游面（`internal/handler/onboarding.go` 372 行 + `onboarding_shim.go` 623 行）
//!
//! 本文件只做**仓储**：四条 SQL 与上游 `server/pkg/db/queries/user.sql` 的四条
//! onboarding 查询**逐字**对齐（`MarkUserOnboarded` / `PatchUserOnboarding` /
//! `JoinCloudWaitlist` / `SetStarterContentState`），外加一个**只读**的
//! `read_waitlist_columns`（`docs/62` §9.7 的对齐断言专用口）。
//!
//! | 上游查询 | 本文件的入口 | 幂等语义 |
//! | --- | --- | --- |
//! | `MarkUserOnboarded` | [`OnboardingRepo::mark_onboarded`] | `COALESCE(onboarded_at, now())` ⇒ 重复调用**保留第一次**的时间戳 |
//! | `PatchUserOnboarding` | [`OnboardingRepo::patch_questionnaire`] | `COALESCE($2, col)` ⇒ 缺 `questionnaire` 的请求**不碰**那一列 |
//! | `JoinCloudWaitlist` | [`OnboardingRepo::join_cloud_waitlist`] | 覆盖写（重复调用覆盖 email + reason） |
//! | `SetStarterContentState` | [`OnboardingRepo::claim_starter_content_state_if_unset`] | `NULL → 'imported'`，已设则**短路**（不用 `COALESCE` —— 上游注释逐字说明原因） |
//!
//! # 三条纪律（`docs/62` §9.7）
//!
//! 1. 🔴 **只读不改** `"user".onboarding_state`（**本地独有列**，
//!    `migrations/compat/537_local_only_columns.up.sql:43`）—— 既有 `crate::user` 在读它，
//!    本 Repo **不得**把它变成第二处写者；
//! 2. **`cloud-waitlist` 的对齐断言必须直读列**（[`OnboardingRepo::read_waitlist_columns`]）——
//!    走 API 回显有「handler 自己拼出来」的假绿风险；
//! 3. **`OnboardingProfile` 的默认值不是"全 `all`"**：未答过问卷 ⇒
//!    `onboarding_questionnaire = '{}'`（列默认），读回来是
//!    [`mc_core::onboarding::QuestionnaireAnswers::default`]。
//!
//! # 返回值口径
//!
//! 上游四条查询都是 `RETURNING *`（**整个** `"user"` 行）⇒ 本文件的四个写方法返回
//! [`mc_core::user::User`]（HTTP 层据此组装 `MeResponse`，与 `GET /api/me` **同一形状**）。
//! `RETURNING` 只带问卷那几列的 [`OnboardingProfile`] 由 [`OnboardingRepo::profile`] 给出，
//! 它是 [`mc_core::onboarding::ONBOARDING_USER_COLUMNS`] 那 5 列的投影。
//!
//! # 与既有实现的交集（**禁改**清单，`docs/62` §9.7 的表）
//!
//! `crates/mc-chat/src/onboarding.rs`、`crates/mc-repos/src/chat_task/onboarding.rs`、
//! `crates/mc-http/src/routes/chat/task/dispatch.rs`、`crates/mc-repos/src/user.rs`
//! —— 四处**只读**。

use chrono::{DateTime, Utc};
use mc_core::onboarding::OnboardingProfile;
use mc_core::user::User;
use mc_core::Id;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

use mc_db::Db;

/// `"user"` 行的列清单（与 `crate::user` 的 `UserRow` 逐字同序 —— 同一个 `FromRow` 形状）。
const USER_COLUMNS: &str = "id, name, email, avatar_url, email_verified_at, language, timezone, \
                           profile_description, onboarded_at, onboarding_state, created_at, \
                           updated_at";

/// onboarding 的那 5 列（[`mc_core::onboarding::ONBOARDING_USER_COLUMNS`] 的投影）。
const PROFILE_COLUMNS: &str = "onboarded_at, onboarding_questionnaire, cloud_waitlist_email, \
                               cloud_waitlist_reason, starter_content_state";

/// 上游 `MarkUserOnboarded`：`COALESCE` 是**幂等**的全部机制（重复调用保留第一次的时间戳）。
const SQL_MARK_ONBOARDED: &str = "UPDATE \"user\" SET onboarded_at = COALESCE(onboarded_at, \
                                  now()), updated_at = now() WHERE id = $1";

/// 上游 `PatchUserOnboarding`：`$2` 为 `NULL` ⇒ 那一列**不碰**（省略 `questionnaire` 的合法请求）。
const SQL_PATCH_QUESTIONNAIRE: &str =
    "UPDATE \"user\" SET onboarding_questionnaire = COALESCE($2::jsonb, onboarding_questionnaire), \
     updated_at = now() WHERE id = $1";

/// 上游 `JoinCloudWaitlist`：覆盖写，且**不动** `onboarded_at`（加入 ≠ 完成）。
const SQL_JOIN_WAITLIST: &str = "UPDATE \"user\" SET cloud_waitlist_email = $2, \
                                 cloud_waitlist_reason = $3, updated_at = now() WHERE id = $1";

/// 上游 `SetStarterContentState`：直赋（**不是** `COALESCE` —— 上游注释逐字说明了原因）。
const SQL_SET_STARTER_CONTENT: &str =
    "UPDATE \"user\" SET starter_content_state = $2, updated_at = now() WHERE id = $1";

/// `"user"` 行（[`USER_COLUMNS`] 的顺序）。
#[derive(Debug, FromRow)]
struct UserRow {
    id: Uuid,
    name: String,
    email: String,
    avatar_url: Option<String>,
    email_verified_at: Option<DateTime<Utc>>,
    language: Option<String>,
    timezone: Option<String>,
    profile_description: Option<String>,
    onboarded_at: Option<DateTime<Utc>>,
    onboarding_state: Option<serde_json::Value>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<UserRow> for User {
    type Error = RepoError;

    fn try_from(row: UserRow) -> Result<Self> {
        Ok(Self {
            id: Id::from(row.id),
            name: row.name,
            email: row.email,
            avatar_url: row.avatar_url,
            email_verified_at: row.email_verified_at.map(mc_core::Timestamp::from),
            language: row.language,
            timezone: row.timezone,
            profile_description: row.profile_description,
            onboarded_at: row.onboarded_at.map(mc_core::Timestamp::from),
            onboarding_state: row.onboarding_state,
            created_at: row.created_at.into(),
            updated_at: row.updated_at.into(),
        })
    }
}

/// onboarding 那 5 列的行形状。
#[derive(Debug, FromRow)]
struct ProfileRow {
    onboarded_at: Option<DateTime<Utc>>,
    onboarding_questionnaire: serde_json::Value,
    cloud_waitlist_email: Option<String>,
    cloud_waitlist_reason: Option<String>,
    starter_content_state: Option<String>,
}

/// waitlist 两列的**直读**结果（对齐断言专用，**不经** API 回显）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitlistColumns {
    /// `user.cloud_waitlist_email`（`VARCHAR(254)`）。
    pub email: Option<String>,
    /// `user.cloud_waitlist_reason`（`TEXT`；空串折成 `NULL`）。
    pub reason: Option<String>,
}

/// onboarding 的 user 列读写（**M9-3**）。
#[derive(Clone)]
pub struct OnboardingRepo {
    db: Db,
}

impl OnboardingRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `GetUser`（`RETURNING *` 那一族共用）—— 读整个 `"user"` 行。
    pub async fn get_user(&self, user_id: Id) -> Result<User> {
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM \"user\" WHERE id = $1"
        ))
        .bind(user_id.as_uuid())
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    /// [`mc_core::onboarding::ONBOARDING_USER_COLUMNS`] 那 5 列的投影。
    ///
    /// ⚠️ 列默认值语义：`onboarding_questionnaire` 是 `NOT NULL DEFAULT '{}'`
    /// ⇒ 从没答过的用户读回来是 [`QuestionnaireAnswers::default`]（**不是**「全已答」）。
    pub async fn profile(&self, user_id: Id) -> Result<OnboardingProfile> {
        let row = sqlx::query_as::<_, ProfileRow>(&format!(
            "SELECT {PROFILE_COLUMNS} FROM \"user\" WHERE id = $1"
        ))
        .bind(user_id.as_uuid())
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        Ok(profile_from_row(&row))
    }

    /// 上游 `MarkUserOnboarded`：`onboarded_at = COALESCE(onboarded_at, now())`。
    ///
    /// **幂等**：重复调用**保留第一次**的时间戳（返回行里的 `onboarded_at` 逐字不变）
    /// —— 这就是 `POST /api/me/onboarding/complete` 的「重复调用不改状态」的全部机制。
    pub async fn mark_onboarded(&self, user_id: Id) -> Result<User> {
        let row =
            sqlx::query_as::<_, UserRow>(&format!("{SQL_MARK_ONBOARDED} RETURNING {USER_COLUMNS}"))
                .bind(user_id.as_uuid())
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?
                .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    /// 上游 `PatchUserOnboarding`：
    /// `onboarding_questionnaire = COALESCE(sqlc.narg('questionnaire'), onboarding_questionnaire)`。
    ///
    /// `questionnaire = None` ⇒ **不碰**那一列（请求里省略 `questionnaire` 是合法调用，
    /// 且**保留**已存的答案）。
    pub async fn patch_questionnaire(
        &self,
        user_id: Id,
        questionnaire: Option<&serde_json::Value>,
    ) -> Result<User> {
        // 🔴 存的是**客户端的原始 JSON**（上游 `params.Questionnaire = []byte(*req.Questionnaire)`）。
        // 本仓**不**在这里把问卷反序列化成 `QuestionnaireAnswers` 再序列化回去 ——
        // 那样会把客户端**没写**的字段补成默认值（`role: ""` 等），属于改写用户数据。
        let payload = questionnaire.cloned();
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "{SQL_PATCH_QUESTIONNAIRE} RETURNING {USER_COLUMNS}"
        ))
        .bind(user_id.as_uuid())
        .bind(payload)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    /// 上游 `JoinCloudWaitlist`：`cloud_waitlist_email = $2, cloud_waitlist_reason = $3`。
    ///
    /// **覆盖写**（重复调用覆盖 email + reason）；`reason` 为 `None` ⇒ 写 `NULL`。
    /// ⚠️ 这一步**不**动 `onboarded_at`（上游逐字：加入 waitlist **不**等于完成 onboarding）。
    pub async fn join_cloud_waitlist(
        &self,
        user_id: Id,
        email: &str,
        reason: Option<&str>,
    ) -> Result<User> {
        let row =
            sqlx::query_as::<_, UserRow>(&format!("{SQL_JOIN_WAITLIST} RETURNING {USER_COLUMNS}"))
                .bind(user_id.as_uuid())
                .bind(email)
                .bind(reason)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?
                .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    /// 上游 `SetStarterContentState`（shim 的 `claimStarterContentStateIfUnset`）。
    ///
    /// `current = None` ⇒ `NULL → 'imported'`；`current = Some(_)` ⇒ **短路，不写**
    /// （上游注释逐字：这里用 `COALESCE` 会把迁移吞掉，所以是**直赋**，
    /// 而「要不要迁」的判断由 handler 读当前值负责 —— 那正是 `current` 这个参数的职责）。
    pub async fn claim_starter_content_state_if_unset(
        &self,
        user_id: Id,
        current: Option<&str>,
    ) -> Result<()> {
        if current.is_some() {
            return Ok(());
        }
        sqlx::query(SQL_SET_STARTER_CONTENT)
            .bind(user_id.as_uuid())
            .bind(OnboardingProfile::STARTER_CONTENT_IMPORTED)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// **直读** waitlist 两列（`docs/62` §9.7 的对齐断言专用口）。
    ///
    /// 🔴 这是 `POST /api/me/onboarding/cloud-waitlist` 的**唯一**对齐判据：
    /// 响应体是 handler 自己从返回行拼出来的，走响应比对会有「handler 把请求体原样回显」
    /// 的假绿；直读列才是「真的写进去了」。
    pub async fn read_waitlist_columns(&self, user_id: Id) -> Result<WaitlistColumns> {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT cloud_waitlist_email, cloud_waitlist_reason FROM \"user\" WHERE id = $1",
        )
        .bind(user_id.as_uuid())
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        match row {
            Some((email, reason)) => Ok(WaitlistColumns { email, reason }),
            None => Err(RepoError::NotFound),
        }
    }
}

impl RepoWithDb for OnboardingRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

fn profile_from_row(row: &ProfileRow) -> OnboardingProfile {
    OnboardingProfile {
        onboarded_at: row.onboarded_at.map(mc_core::Timestamp::from),
        // 列是 `NOT NULL DEFAULT '{}'`；真读到 `null`（历史回填）时也折成默认值，
        // 与上游 `beforeRaw == "null"` ⇒ 「当作没答过」的判定同向。
        onboarding_questionnaire: serde_json::from_value(row.onboarding_questionnaire.clone())
            .unwrap_or_default(),
        cloud_waitlist_email: row.cloud_waitlist_email.clone(),
        cloud_waitlist_reason: row.cloud_waitlist_reason.clone(),
        starter_content_state: row.starter_content_state.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_core::onboarding::{QuestionnaireAnswers, ONBOARDING_USER_COLUMNS};

    /// 仓储的**读**投影恰好是那 5 列（外加 `updated_at` 这条直赋列）——
    /// 尤其是**不得**出现 `onboarding_state`（本地独有列，纪律 ①）。
    #[test]
    fn the_repo_touches_only_the_five_columns_plus_updated_at() {
        for column in ONBOARDING_USER_COLUMNS {
            assert!(PROFILE_COLUMNS.contains(column), "投影缺列 {column}");
        }
        assert!(!PROFILE_COLUMNS.contains("onboarding_state"));
    }

    /// 🔴 三条写路径的 SQL 文本里**一次都不得**出现 `onboarding_state`
    /// （本地独有列，本片只读；判据是 SQL 常量本身，可被变异测试直接打中）。
    #[test]
    fn no_write_statement_touches_the_local_only_column() {
        for sql in [
            SQL_MARK_ONBOARDED,
            SQL_PATCH_QUESTIONNAIRE,
            SQL_JOIN_WAITLIST,
        ] {
            assert!(!sql.contains("onboarding_state"), "{sql}");
        }
        // `starter_content_state` 的写入**只**在 shim 那一条（`SetStarterContentState`），
        // 且用的是**直赋**而不是 `COALESCE`（上游逐字：`COALESCE` 会把迁移吞掉）。
        assert!(!SQL_SET_STARTER_CONTENT.contains("COALESCE"));
        assert!(SQL_SET_STARTER_CONTENT.contains("starter_content_state = $2"));
    }

    /// 幂等的**机制**在 SQL 里，不在 Rust 里（这是可被变异测试打中的判据）。
    #[test]
    fn the_idempotence_is_carried_by_coalesce_in_sql() {
        assert!(SQL_MARK_ONBOARDED.contains("COALESCE(onboarded_at, now())"));
        // 问卷那一条的 `COALESCE` 方向相反：`$2` 为 `NULL` 时**保留旧值**。
        assert!(SQL_PATCH_QUESTIONNAIRE.contains("COALESCE($2::jsonb, onboarding_questionnaire)"));
        // waitlist 是**覆盖**写：既不 `COALESCE` 也不看 `onboarded_at`。
        assert!(!SQL_JOIN_WAITLIST.contains("COALESCE"));
        assert!(!SQL_JOIN_WAITLIST.contains("onboarded_at"));
    }

    #[test]
    fn profile_row_maps_the_column_defaults() {
        let row = ProfileRow {
            onboarded_at: None,
            onboarding_questionnaire: serde_json::json!({}),
            cloud_waitlist_email: None,
            cloud_waitlist_reason: None,
            starter_content_state: None,
        };
        let profile = profile_from_row(&row);
        assert!(!profile.is_complete());
        assert!(!profile.has_joined_cloud_waitlist());
        // 「从没答过」不是「全已答」。
        assert_eq!(
            profile.onboarding_questionnaire,
            QuestionnaireAnswers::default()
        );
        assert!(!profile.onboarding_questionnaire.in_flow_resolved());
    }

    /// 历史行里 `onboarding_questionnaire` 是 `null`（`094` 之前）⇒ 折成默认值而不是报错。
    #[test]
    fn profile_row_tolerates_a_null_questionnaire() {
        let row = ProfileRow {
            onboarded_at: None,
            onboarding_questionnaire: serde_json::Value::Null,
            cloud_waitlist_email: Some("a@b.test".into()),
            cloud_waitlist_reason: None,
            starter_content_state: None,
        };
        let profile = profile_from_row(&row);
        assert!(profile.has_joined_cloud_waitlist());
        assert_eq!(
            profile.onboarding_questionnaire,
            QuestionnaireAnswers::default()
        );
    }

    /// v2 形状（`094`）逐字段往返：写进去什么，读回来还是什么。
    #[test]
    fn profile_row_round_trips_the_v2_questionnaire_field_by_field() {
        let raw = serde_json::json!({
            "source": ["search"],
            "source_other": "",
            "source_skipped": false,
            "role": "engineer",
            "role_other": "",
            "role_skipped": false,
            "use_case": ["ship_code", "manage_team"],
            "use_case_other": "",
            "use_case_skipped": false,
            "version": 2,
        });
        let row = ProfileRow {
            onboarded_at: None,
            onboarding_questionnaire: raw.clone(),
            cloud_waitlist_email: None,
            cloud_waitlist_reason: None,
            starter_content_state: None,
        };
        let answers = profile_from_row(&row).onboarding_questionnaire;
        assert_eq!(answers.source, vec!["search"]);
        assert_eq!(answers.role, "engineer");
        assert_eq!(answers.use_case, vec!["ship_code", "manage_team"]);
        assert!(answers.is_current_schema());
        assert!(answers.in_flow_resolved());
        // 逐字回写：序列化回去的 JSON 与落库的 JSON 等价。
        assert_eq!(serde_json::to_value(&answers).expect("serialize"), raw);
    }

    /// `onboarding_questionnaire` 是 `NOT NULL DEFAULT '{}'` ⇒ `get_user` 永远拿得到一行。
    #[test]
    fn user_row_maps_every_declared_column() {
        let row = UserRow {
            id: Uuid::new_v4(),
            name: "alice".into(),
            email: "alice@example.test".into(),
            avatar_url: None,
            email_verified_at: None,
            language: Some("zh-CN".into()),
            timezone: None,
            profile_description: None,
            onboarded_at: Some(Utc::now()),
            // 🔴 本地独有列：仓储层**读**它（`User` 的形状要求）但**从不写**它。
            onboarding_state: Some(serde_json::json!({"step": 2})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let user = User::try_from(row).expect("user row");
        assert!(user.onboarded_at.is_some());
        assert_eq!(user.language.as_deref(), Some("zh-CN"));
    }
}
