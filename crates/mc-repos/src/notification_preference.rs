//! `NotificationPreferenceRepo` —— `notification_preference` 表的读写（**写者 M9-5**）。
//!
//! # anchor 期是**空桩**
//!
//! 本文件由 M9-0 anchor 建目录格（`crates/mc-repos/src/lib.rs` 的
//! `pub mod notification_preference;`），M9-5 原地填充。
//!
//! # M9-5 要填什么
//!
//! 上游 `internal/handler/notification_preference.go`（172 行）的仓储面，
//! 表 = **`notification_preference`**（上游 `0001` 建，本仓 `contracts/upstream-schema.sql` 里同名），
//! 关键是 `(workspace_id, user_id)` 唯一 + `preferences` 一列 **JSONB**。
//!
//! 词表与校验**不在本文件**：三处（`GROUP` / `VALUE` / 错误文本）都在
//! [`mc_core::notification`]，本 Repo 只负责读写那一列 JSONB。
//!
//! # 三条纪律
//!
//! 1. **没有行 ≠ 全 `all`**：未设置过偏好 ⇒ 上游返回 `preferences: {}`
//!    （**空对象**），不是默认表 ⇒ 本 Repo 的 `get` 要区分「无行」与「有行但空 map」
//!    （用 `Option<Row>`，**不要** `unwrap_or_default()` 把两者抹平）；
//! 2. **`GET` 不写行**：读面**不得**为了"顺手初始化"插一行（那会让「从没设过」变成
//!    「设过但全默认」，两者的客户端行为不同）；
//! 3. **`workspace_id` 来自中间件**：客户端走私的 `workspace_id` 必须被覆盖（`docs/62` §2.7 第 7 条）。

use mc_db::Db;

use crate::RepoWithDb;

/// `notification_preference` 表访问（**M9-5 填充**）。
#[derive(Clone)]
pub struct NotificationPreferenceRepo {
    db: Db,
}

impl NotificationPreferenceRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for NotificationPreferenceRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
