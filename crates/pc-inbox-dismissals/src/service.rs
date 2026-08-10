//! Service —— `InboxDismissalService` 实现。
//!
//! 与 Node `inboxDismissalService(db)` 1:1 对齐。
//!
//! 设计：
//! - `db: pc_repos::Db` 拥有（与 `pc-decision-bundle` / `pc-inbox-agent-policy` 一致）
//! - 通过 `InboxRepo::new(&self.db)` 访问仓储
//! - Hook 在 `before_*` / `after_*` 六个时机调用
//! - 校验错误从 `RepoError::Invalid` 透传 → 转换为 `InboxDismissalServiceError::Validation`

use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use pc_core::Timestamp;
use pc_repos::inbox::{InboxDismissalRow, InboxRepo, NewDismissal};
use pc_repos::{Db, RepoError};

use crate::hook::{InboxDismissalHook, NoopInboxDismissalHook};
use crate::types::{
    InboxDismissalFilter, InboxDismissalServiceError,
};

// ============================================================================
// Re-export from types
// ============================================================================



// ============================================================================
// Service
// ============================================================================

/// Inbox dismissal service（与 Node `inboxDismissalService(db)` 1:1 对齐）。
#[derive(Clone)]
pub struct InboxDismissalService {
    db: Db,
    hook: Arc<dyn InboxDismissalHook>,
}

impl std::fmt::Debug for InboxDismissalService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxDismissalService")
            .field("hook", &"<dyn InboxDismissalHook>")
            .finish()
    }
}

impl InboxDismissalService {
    pub fn new(db: Db) -> Self {
        Self { db, hook: Arc::new(NoopInboxDismissalHook) }
    }

    pub fn with_hook(db: Db, hook: Arc<dyn InboxDismissalHook>) -> Self {
        Self { db, hook }
    }

    pub fn with_hook_arc(mut self, hook: Arc<dyn InboxDismissalHook>) -> Self {
        self.hook = hook;
        self
    }

    pub fn hook(&self) -> Arc<dyn InboxDismissalHook> {
        self.hook.clone()
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    fn repo(&self) -> InboxRepo<'_> {
        InboxRepo::new(&self.db)
    }

    fn map_repo_err(e: RepoError) -> InboxDismissalServiceError {
        match e {
            RepoError::Invalid(msg) => InboxDismissalServiceError::Validation(msg),
            other => InboxDismissalServiceError::Repo(other),
        }
    }

    // ===== Reads =====

    /// 列出某用户在该公司的所有 dismissal（含已过期的 snooze）。
    pub async fn list(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> Result<Vec<InboxDismissalRow>, InboxDismissalServiceError> {
        Ok(self.repo().list_for_user(company_id, user_id).await?)
    }

    /// 列出某用户在该公司的**生效中** dismissal（dismiss 一律 + 未到期的 snooze）。
    pub async fn list_active(
        &self,
        company_id: Uuid,
        user_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<InboxDismissalRow>, InboxDismissalServiceError> {
        Ok(self
            .repo()
            .list_active_for_user(company_id, user_id, Timestamp::from_dt(now))
            .await?)
    }

    /// 列出全公司的生效数（dashboard 聚合）。
    pub async fn count_active(
        &self,
        company_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<i64, InboxDismissalServiceError> {
        Ok(self
            .repo()
            .count_active(company_id, Timestamp::from_dt(now))
            .await?)
    }

    /// 取一行（`None` 表示不存在）。
    pub async fn get(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> Result<Option<InboxDismissalRow>, InboxDismissalServiceError> {
        Ok(self.repo().get(company_id, user_id, item_key).await?)
    }

    // ===== Mutations =====

    /// 通用 upsert（kind = dismiss 时 `snoozedUntil` 必须为 `None`；kind = snooze 时必须为 `Some` 且未来）。
    pub async fn upsert(
        &self,
        n: &NewDismissal,
    ) -> Result<InboxDismissalRow, InboxDismissalServiceError> {
        self.repo()
            .upsert(n)
            .await
            .map_err(Self::map_repo_err)
    }

    /// Dismiss 某条 item（与 Node `dismiss` 1:1 对齐）。
    pub async fn dismiss(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> Result<InboxDismissalRow, InboxDismissalServiceError> {
        self.hook.before_dismiss(company_id, user_id, item_key);
        let row = self
            .repo()
            .dismiss(company_id, user_id, item_key)
            .await
            .map_err(Self::map_repo_err)?;
        self.hook.after_dismiss(&row);
        Ok(row)
    }

    /// Snooze 某条 item 直到 `until`（与 Node `snooze` 1:1 对齐）。
    pub async fn snooze(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
        until: DateTime<Utc>,
    ) -> Result<InboxDismissalRow, InboxDismissalServiceError> {
        self.hook
            .before_snooze(company_id, user_id, item_key, until);
        let row = self
            .repo()
            .snooze(company_id, user_id, item_key, Timestamp::from_dt(until))
            .await
            .map_err(Self::map_repo_err)?;
        self.hook.after_snooze(&row);
        Ok(row)
    }

    /// Restore（删除一行）。返回是否真的删除（与 Node `restore` 1:1 对齐）。
    pub async fn restore(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> Result<bool, InboxDismissalServiceError> {
        self.hook.before_restore(company_id, user_id, item_key);
        let removed = self.repo().restore(company_id, user_id, item_key).await?;
        self.hook
            .after_restore(company_id, user_id, item_key, removed);
        Ok(removed)
    }

    /// 清除全局过期的 snooze 行（maintenance job 调用）。
    pub async fn expire_snoozes(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, InboxDismissalServiceError> {
        Ok(self
            .repo()
            .expire_snoozes(Timestamp::from_dt(now))
            .await?)
    }
}

// ============================================================================
// Pure helpers / trait impls
// ============================================================================

/// 把一批 row 在内存侧按 kind 过滤 + active 时间筛选（与 Node 内存过滤 1:1 对齐）。
///
/// `now` 为筛选时间点（决定哪些 snooze 已过期）。
pub fn filter_rows(
    rows: Vec<InboxDismissalRow>,
    filter: &InboxDismissalFilter,
) -> Vec<InboxDismissalRow> {
    rows.into_iter()
        .filter(|row| {
            if let Some(kind) = filter.kind {
                if row.parsed_kind() != Some(kind) {
                    return false;
                }
            }
            if let Some(now) = filter.active_at {
                // active_now 意味着 snooze 未过期 / dismiss 一律算 active
                let now_t = Timestamp::from_dt(now);
                if !row.active_at(now_t) {
                    return false;
                }
            }
            true
        })
        .collect()
}

// ============================================================================
// Tests — pure helpers
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{activity_at_kind, codes, filter_by_kind, InboxDismissalActivity};
    use chrono::Duration;
    use pc_repos::inbox::DismissKind;

    fn make_row(kind: DismissKind, snoozed_until: Option<DateTime<Utc>>) -> InboxDismissalRow {
        InboxDismissalRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            user_id: "u".into(),
            item_key: "approval:cm1:ap1".into(),
            kind: kind.as_str().to_string(),
            dismissed_at: Timestamp::from_dt(Utc::now()),
            snoozed_until: snoozed_until.map(Timestamp::from_dt),
            created_at: Timestamp::from_dt(Utc::now()),
            updated_at: Timestamp::from_dt(Utc::now()),
        }
    }

    #[test]
    fn r679_activity_at_kind_dismiss_is_dismiss() {
        let now = Utc::now();
        assert_eq!(
            activity_at_kind(DismissKind::Dismiss, None, Timestamp::from_dt(now)),
            InboxDismissalActivity::Dismiss
        );
    }

    #[test]
    fn r679_activity_at_kind_snooze_active_when_future() {
        let now = Utc::now();
        let future = now + Duration::hours(1);
        assert_eq!(
            activity_at_kind(
                DismissKind::Snooze,
                Some(Timestamp::from_dt(future)),
                Timestamp::from_dt(now)
            ),
            InboxDismissalActivity::SnoozeActive
        );
    }

    #[test]
    fn r679_activity_at_kind_snooze_expired_when_past() {
        let now = Utc::now();
        let past = now - Duration::hours(1);
        assert_eq!(
            activity_at_kind(
                DismissKind::Snooze,
                Some(Timestamp::from_dt(past)),
                Timestamp::from_dt(now)
            ),
            InboxDismissalActivity::SnoozeExpired
        );
    }

    #[test]
    fn r679_activity_at_kind_snooze_no_until_is_expired() {
        let now = Utc::now();
        assert_eq!(
            activity_at_kind(DismissKind::Snooze, None, Timestamp::from_dt(now)),
            InboxDismissalActivity::SnoozeExpired
        );
    }

    #[test]
    fn r679_filter_by_kind_drops_other_kinds() {
        let a = make_row(DismissKind::Dismiss, None);
        let b = make_row(DismissKind::Snooze, Some(Utc::now() + Duration::hours(1)));
        let rows = vec![a.clone(), b];
        let only_dismiss = filter_by_kind(rows.clone(), DismissKind::Dismiss);
        assert_eq!(only_dismiss.len(), 1);
        assert_eq!(only_dismiss[0].id, a.id);

        let only_snooze = filter_by_kind(rows, DismissKind::Snooze);
        assert_eq!(only_snooze.len(), 1);
        assert_eq!(only_snooze[0].kind, "snooze");
    }

    #[test]
    fn r679_filter_rows_by_kind() {
        let a = make_row(DismissKind::Dismiss, None);
        let b = make_row(DismissKind::Snooze, Some(Utc::now() + Duration::hours(1)));
        let rows = vec![a, b];
        let now = Utc::now();
        let f = InboxDismissalFilter::new().with_kind(DismissKind::Dismiss);
        let out = filter_rows(rows.clone(), &f);
        assert_eq!(out.len(), 1);

        let f = InboxDismissalFilter::new().with_active_at(now);
        let out = filter_rows(rows, &f);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn r679_filter_rows_active_excludes_expired_snooze() {
        let a = make_row(DismissKind::Dismiss, None);
        let b = make_row(DismissKind::Snooze, Some(Utc::now() - Duration::hours(1)));
        let rows = vec![a, b];
        let now = Utc::now();
        let f = InboxDismissalFilter::new().with_active_at(now);
        let out = filter_rows(rows, &f);
        assert_eq!(out.len(), 1, "expired snooze should be filtered out");
    }

    #[test]
    fn r679_empty_filter_returns_all() {
        let a = make_row(DismissKind::Dismiss, None);
        let b = make_row(DismissKind::Snooze, Some(Utc::now() + Duration::hours(1)));
        let rows = vec![a, b];
        let out = filter_rows(rows, &InboxDismissalFilter::new());
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn r679_error_infer_code() {
        let err = InboxDismissalServiceError::Validation("snooze requires snoozed_until".into());
        assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_SNOOZE_REQUIRES_UNTIL));

        let err = InboxDismissalServiceError::Validation("snoozed_until must be in the future".into());
        assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_SNOOZE_IN_PAST));

        let err = InboxDismissalServiceError::Validation("dismiss must not carry snoozed_until".into());
        assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_DISMISS_WITH_UNTIL));

        let err = InboxDismissalServiceError::Validation("user_id/item_key must not be empty".into());
        assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_EMPTY_IDENTIFIER));

        let err = InboxDismissalServiceError::Database("oops".into());
        assert_eq!(err.infer_code(), None);
    }

    #[test]
    fn r679_codes_constants_match_node() {
        assert_eq!(codes::INBOX_DISMISSAL_SNOOZE_IN_PAST, "inbox_dismissal_snooze_in_past");
        assert_eq!(
            codes::INBOX_DISMISSAL_SNOOZE_REQUIRES_UNTIL,
            "inbox_dismissal_snooze_requires_until"
        );
        assert_eq!(codes::INBOX_DISMISSAL_DISMISS_WITH_UNTIL, "inbox_dismissal_dismiss_with_until");
        assert_eq!(codes::INBOX_DISMISSAL_EMPTY_IDENTIFIER, "inbox_dismissal_empty_identifier");
    }
}
