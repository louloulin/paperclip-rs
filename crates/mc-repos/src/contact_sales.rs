//! `ContactSalesRepo` —— `contact_sales_inquiry` 表的写入（**写者 M9-5**）。
//!
//! # anchor 期是**空桩**
//!
//! 本文件由 M9-0 anchor 建目录格（`crates/mc-repos/src/lib.rs` 的 `pub mod contact_sales;`），
//! M9-5 原地填充。
//!
//! # M9-5 要填什么
//!
//! 上游 `internal/handler/contact_sales.go`（323 行）的仓储面，表 =
//! **`contact_sales_inquiry`**。路由只有一条：`POST /api/contact-sales`。
//!
//! # 三条纪律
//!
//! 1. 🔴 **这条路由是"公开面"的一种**：它**没有** workspace 上下文（**无会话也可访问**，
//!    上游用无会话请求验它 —— `docs/62` §4.2 的 contact-sales 行）⇒ 本 Repo 的写入
//!    **不得**依赖 `workspace_id`；
//! 2. **限流 5/h 是路由层的事**（`RATE_LIMIT_CONTACT_SALES`，缺省用默认值 ——
//!    `envPositiveInt` 语义）；复用既有 `SlidingWindowLimiter`，**禁止**新写；
//! 3. **校验（企业邮箱域名 / `company_size` 枚举）在 handler 层**（`docs/62` §6.5 的
//!    M9-5 行 `DoD` 逐条点名）；本 Repo 只落库。

use mc_db::Db;

use crate::RepoWithDb;

/// `contact_sales_inquiry` 表访问（**M9-5 填充**）。
#[derive(Clone)]
pub struct ContactSalesRepo {
    db: Db,
}

impl ContactSalesRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for ContactSalesRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
