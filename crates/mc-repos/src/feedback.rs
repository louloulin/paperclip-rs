//! `FeedbackRepo` —— `feedback` 表的写入（**写者 M9-5**）。
//!
//! # anchor 期是**空桩**
//!
//! 本文件由 M9-0 anchor 建目录格（`crates/mc-repos/src/lib.rs` 的 `pub mod feedback;`），
//! M9-5 原地填充。
//!
//! # M9-5 要填什么
//!
//! 上游 `internal/handler/feedback.go`（177 行）的仓储面，表 = **`feedback`**。
//! 路由只有一条：`POST /api/feedback`。
//!
//! # 三条纪律
//!
//! 1. **`has_images` 是一个标记，不是"图片本体"**：上传通道在别处（附件面），
//!    本波只落布尔/计数（`docs/62` §4.1 的 M9-5 行 `DoD`：「feedback 的 `has_images` 标记」）；
//! 2. **限流是路由层的事**（10/h，复用 `mc_autopilot::webhook::ratelimit` 的
//!    `SlidingWindowLimiter`，**禁止**新写限流器 —— `docs/62` §2.3 / §2.7 第 6 条）；
//!    本 Repo **不**做限流；
//! 3. **`workspace_id` / `user_id` 来自鉴权上下文**，不来自请求体。

use mc_db::Db;

use crate::RepoWithDb;

/// `feedback` 表访问（**M9-5 填充**）。
#[derive(Clone)]
pub struct FeedbackRepo {
    db: Db,
}

impl FeedbackRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for FeedbackRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
