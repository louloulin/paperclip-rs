//! `MikaRepo` —— 内置 agent（Mika）的供给与 onboarding 会话（**写者 M9-7**）。
//!
//! # anchor 期是**空桩**
//!
//! 本文件由 M9-0 anchor 建目录格；**注意**：它的模块声明写在
//! `crates/mc-repos/src/agent.rs` 里（`pub mod mika;` ⇒ 本文件），
//! **不是** `crates/mc-repos/src/lib.rs` —— 因为 `agent` 是一个**文件 + 子目录**的模块
//! （`agent.rs` + `agent/{env,labels,tasks}.rs`），`mika.rs` 是它的第四个兄弟。
//! ⚠️ 计划文本（`docs/62` §3.3 的写集表）把这一格写成 `crates/mc-repos/src/agent/mod.rs`
//! —— **那个文件不存在**（本仓用的是 `agent.rs`）；逐字路径勘误登记 `docs/32` §9.13。
//!
//! # M9-7 要填什么
//!
//! 上游 `internal/handler/mika_agent.go`（328 行）的仓储面。一条路由：
//! `POST /api/agents/mika`。
//!
//! 上游 import 只有 `pgx`/`service`/`protocol`/`db`/`logger`/`metrics` ⇒ **零** cloud
//! transport / entitlement / Stripe 依赖（`docs/62` §9.1 的裁定依据之一）。
//!
//! # 五条纪律（`docs/62` §6.5 的 M9-7 行 `DoD`）
//!
//! 1. **get-or-create 幂等**：同 workspace 第二次调用返回**既有** agent
//!    （不是再建一个）；
//! 2. **并发安全**：2 个并发请求 ⇒ **1 个 agent + 1 个会话**（上游用
//!    `LockWorkspaceForChatSessionCreate` 的会话锁语义 —— 本仓**复用**
//!    `crates/mc-repos/src/chat_session.rs` 的既有语义）；
//! 3. 🔴 **`kind` / `system_key` 不可由客户端铸造**（多传的字段被**忽略**，
//!    不是"原样落库"）—— `agent.rs` 的 `NewAgent` 里**没有**这两个字段，这正是机制；
//! 4. **`language` 白名单外的值 ⇒ 400**：白名单在
//!    `crates/mc-chat/src/onboarding.rs`（M4-4 已交付）—— **只读复用**，不复制一份；
//! 5. **`runtime_id` 不合法 ⇒ 400**（复用 `AgentRepo::runtime_binding` 的既有校验）。
//!
//! # 两处**禁止**
//!
//! - **不建 `mc-mika` crate**（`docs/01:92` 的 `mc-mika` 已被 `docs/62` §9.3 收敛掉）；
//! - **不改** `crates/mc-chat/src/onboarding.rs` / `crates/mc-repos/src/chat_task/onboarding.rs`
//!   / `crates/mc-http/src/routes/chat/task/dispatch.rs`（M4-4 的交付，**只读**）。

use mc_db::Db;

use crate::RepoWithDb;

/// Mika 内置 agent 的供给（**M9-7 填充**）。
#[derive(Clone)]
pub struct MikaRepo {
    db: Db,
}

impl MikaRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for MikaRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
