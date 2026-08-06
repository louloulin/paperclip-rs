//! `inbox_dismissals` 域 — 用户级收件箱 dismissed/snoozed 状态。
//!
//! 设计：
//! - 单一 `(company_id, user_id, item_key)` 唯一约束
//! - `kind`: dismiss (永久) / snooze (限时，过期自动恢复)
//! - `item_key` 命名约定：`{kind}:{scope}:{entity}`，例 `approval:cm1:ap42` / `run:cm1:hb88`
//! - `snoozed_until` 是排他字段：dismiss 时必须为 NULL，snooze 时必填且未来

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{RepoError, RepoResult};
use pc_core::Timestamp;
use pc_db::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DismissKind {
    Dismiss,
    Snooze,
}
impl DismissKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dismiss => "dismiss",
            Self::Snooze => "snooze",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "dismiss" => Some(Self::Dismiss),
            "snooze" => Some(Self::Snooze),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Approval,
    Run,
    Join,
    Attention,
    Custom,
}
impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Run => "run",
            Self::Join => "join",
            Self::Attention => "attention",
            Self::Custom => "custom",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approval" => Some(Self::Approval),
            "run" => Some(Self::Run),
            "join" => Some(Self::Join),
            "attention" => Some(Self::Attention),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

const COLS: &str = "id, company_id, user_id, item_key, kind, dismissed_at, snoozed_until,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InboxDismissalRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub user_id: String,
    pub item_key: String,
    pub kind: String,
    pub dismissed_at: Timestamp,
    pub snoozed_until: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewDismissal {
    pub company_id: Uuid,
    pub user_id: String,
    pub item_key: String,
    pub kind: DismissKind,
    pub snoozed_until: Option<Timestamp>,
}

impl InboxDismissalRow {
    pub fn parsed_kind(&self) -> Option<DismissKind> {
        DismissKind::parse(&self.kind)
    }

    pub fn active_at(&self, now: Timestamp) -> bool {
        match self.parsed_kind() {
            Some(DismissKind::Dismiss) => true,
            Some(DismissKind::Snooze) => match self.snoozed_until {
                Some(until) => until.as_datetime() > now.as_datetime(),
                None => false,
            },
            None => false,
        }
    }
}

pub struct InboxRepo<'a> {
    pub db: &'a Db,
}

impl<'a> InboxRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_for_user(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> RepoResult<Vec<InboxDismissalRow>> {
        let sql = format!(
            "SELECT {COLS} FROM inbox_dismissals              WHERE company_id=$1 AND user_id=$2 ORDER BY updated_at DESC"
        );
        Ok(sqlx::query_as::<_, InboxDismissalRow>(&sql)
            .bind(company_id)
            .bind(user_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_active_for_user(
        &self,
        company_id: Uuid,
        user_id: &str,
        now: Timestamp,
    ) -> RepoResult<Vec<InboxDismissalRow>> {
        let sql = format!(
            "SELECT {COLS} FROM inbox_dismissals              WHERE company_id=$1 AND user_id=$2                AND (kind='dismiss' OR (kind='snooze' AND snoozed_until > $3))              ORDER BY updated_at DESC"
        );
        Ok(sqlx::query_as::<_, InboxDismissalRow>(&sql)
            .bind(company_id)
            .bind(user_id)
            .bind(now)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> RepoResult<Option<InboxDismissalRow>> {
        let sql = format!(
            "SELECT {COLS} FROM inbox_dismissals              WHERE company_id=$1 AND user_id=$2 AND item_key=$3"
        );
        Ok(sqlx::query_as::<_, InboxDismissalRow>(&sql)
            .bind(company_id)
            .bind(user_id)
            .bind(item_key)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn upsert(&self, n: &NewDismissal) -> RepoResult<InboxDismissalRow> {
        if n.user_id.trim().is_empty() || n.item_key.trim().is_empty() {
            return Err(RepoError::Invalid(
                "user_id/item_key must not be empty".into(),
            ));
        }
        match n.kind {
            DismissKind::Dismiss => {
                if n.snoozed_until.is_some() {
                    return Err(RepoError::Invalid(
                        "dismiss must not carry snoozed_until".into(),
                    ));
                }
            }
            DismissKind::Snooze => match n.snoozed_until {
                None => {
                    return Err(RepoError::Invalid("snooze requires snoozed_until".into()));
                }
                Some(until) if until.as_datetime() <= chrono::Utc::now() => {
                    return Err(RepoError::Invalid(
                        "snoozed_until must be in the future".into(),
                    ));
                }
                _ => {}
            },
        }
        let sql = format!(
            "INSERT INTO inbox_dismissals (company_id, user_id, item_key, kind, snoozed_until)              VALUES ($1,$2,$3,$4,$5)              ON CONFLICT (company_id, user_id, item_key) DO UPDATE SET                 kind=EXCLUDED.kind, dismissed_at=now(),                 snoozed_until=EXCLUDED.snoozed_until, updated_at=now()              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, InboxDismissalRow>(&sql)
            .bind(n.company_id)
            .bind(&n.user_id)
            .bind(&n.item_key)
            .bind(n.kind.as_str())
            .bind(n.snoozed_until)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// 解析为 dismiss（清空 snoozed_until）
    pub async fn dismiss(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> RepoResult<InboxDismissalRow> {
        let n = NewDismissal {
            company_id,
            user_id: user_id.into(),
            item_key: item_key.into(),
            kind: DismissKind::Dismiss,
            snoozed_until: None,
        };
        self.upsert(&n).await
    }

    pub async fn snooze(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
        until: Timestamp,
    ) -> RepoResult<InboxDismissalRow> {
        let n = NewDismissal {
            company_id,
            user_id: user_id.into(),
            item_key: item_key.into(),
            kind: DismissKind::Snooze,
            snoozed_until: Some(until),
        };
        self.upsert(&n).await
    }

    pub async fn restore(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "DELETE FROM inbox_dismissals              WHERE company_id=$1 AND user_id=$2 AND item_key=$3",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(item_key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 自动恢复过期 snooze 的项（返回受影响行数）
    pub async fn expire_snoozes(&self, now: Timestamp) -> RepoResult<u64> {
        let n =
            sqlx::query("DELETE FROM inbox_dismissals WHERE kind='snooze' AND snoozed_until <= $1")
                .bind(now)
                .execute(self.db.pool())
                .await?
                .rows_affected();
        Ok(n)
    }

    /// 按公司聚合：每个 user 的 dismiss 数量（用于 dashboard）
    pub async fn count_active(&self, company_id: Uuid, now: Timestamp) -> RepoResult<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inbox_dismissals              WHERE company_id=$1                AND (kind='dismiss' OR (kind='snooze' AND snoozed_until > $2))",
        )
        .bind(company_id)
        .bind(now)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }

    /// Back-compat: positional upsert(company_id, user_id, item_key, kind, snoozed_until).
    #[allow(dead_code)]
    pub async fn upsert_simple(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
        kind: &str,
        snoozed_until: Option<Timestamp>,
    ) -> RepoResult<InboxDismissalRow> {
        let parsed = DismissKind::parse(kind).unwrap_or(DismissKind::Dismiss);
        let n = NewDismissal {
            company_id,
            user_id: user_id.into(),
            item_key: item_key.into(),
            kind: parsed,
            snoozed_until,
        };
        self.upsert(&n).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_strings_round_trip() {
        assert_eq!(DismissKind::parse("dismiss"), Some(DismissKind::Dismiss));
        assert_eq!(DismissKind::parse("snooze"), Some(DismissKind::Snooze));
        assert_eq!(DismissKind::parse("nope"), None);
        assert_eq!(ItemKind::parse("approval"), Some(ItemKind::Approval));
        assert_eq!(ItemKind::parse("custom"), Some(ItemKind::Custom));
    }

    #[test]
    fn active_at_logic() {
        let row = InboxDismissalRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            user_id: "u1".into(),
            item_key: "approval:cm1:ap1".into(),
            kind: "dismiss".into(),
            dismissed_at: pc_core::Timestamp::from_dt(chrono::Utc::now()),
            snoozed_until: None,
            created_at: pc_core::Timestamp::from_dt(chrono::Utc::now()),
            updated_at: pc_core::Timestamp::from_dt(chrono::Utc::now()),
        };
        assert!(row.active_at(pc_core::Timestamp::from_dt(chrono::Utc::now())));
    }
}
