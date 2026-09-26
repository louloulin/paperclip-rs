//! `TimelineRepo` —— issue timeline 的两个读面合并（**写者 M9-8**）。
//!
//! # anchor 期是**空桩**
//!
//! 本文件由 M9-0 anchor 建目录格（`crates/mc-repos/src/lib.rs` 的 `pub mod timeline;`），
//! M9-8 原地填充。
//!
//! # M9-8 要填什么
//!
//! 上游 `internal/handler/activity.go` 的 **`L63–L393`**（`L394` 起是 M2-A 的
//! `GetAssigneeFrequency`，**不属本波** —— 那块已由 `crate::stats` 交付）。
//! 一条路由：`GET /api/issues/{id}/timeline`。
//!
//! 数据源**两类行**：
//!
//! | 半边 | 表 | 备注 |
//! | --- | --- | --- |
//! | 评论 | `comment` | 既有 `crate::comment` |
//! | 活动 | `activity_log` | 本地**只有 1 个写者**：`crates/mc-repos/src/agent/env.rs:80` |
//!
//! # 四条口径（`docs/62` §6.5 的 M9-8 行 `DoD`）
//!
//! 1. **顺序与去重**：两半按时间合并（同一时刻的稳定次序要**定死**，不许靠 SQL 的隐含顺序）；
//! 2. **keyset 四参**：`before` / `after` / `around` / `limit` 的边界；
//! 3. 🔴 **两侧独立截断、不 clamp 到同一个 floor**（上游注释逐字）⇒ 合并前的两半**各自**
//!    按 `limit` 截断，合并后可能超过 `limit`；响应头 `X-Timeline-Truncated` 由
//!    handler 写（`mc-http/src/routes/timeline.rs`）；
//! 4. **非本 workspace 的 issue ⇒ 404**（不是 403，也不是空列表）。
//!
//! # 两条**不碰**（`docs/62` §2.3 / §9.7）
//!
//! - **不做** `GetAssigneeFrequency`（`crate::stats` 已交付，是 M2-A 的账）；
//! - **不补** `activity_log` 的写入面：本地只有 `agent/env.rs:80` 一个写者
//!   （上游也只有 3 处 `CreateActivity`）⇒ 多数 issue 的 activity 半边**是空的**，
//!   这是**既有面的覆盖率事实**（R-M9-4），由 M9-10 登记，不由本片"顺手补写者"。

use mc_db::Db;

use crate::RepoWithDb;

/// issue timeline 的读聚合（**M9-8 填充**）。
#[derive(Clone)]
pub struct TimelineRepo {
    db: Db,
}

impl TimelineRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for TimelineRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
