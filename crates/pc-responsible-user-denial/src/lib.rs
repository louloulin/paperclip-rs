//! `pc-responsible-user-denial` —— responsible-user denial 业务聚合。
//!
//! 由 2 个 crate 合并而来（本 crate 保留原 `pc-responsible-user-denial` 名 + 新增
//! `run_outcomes` 子模块，等价于旧 `pc-responsible-user-denial-run-outcomes`）：
//!
//! ## 设计
//! - 高内聚：denial code 规范化 + active run 写操作（记录 + live event）都属于
//!   "responsible-user 被拒" 业务闭环
//! - 低耦合：`run_outcomes` 子模块依赖本 crate 顶层的 `normalize_*` / `is_valid_code`
//!
//! ## R572 扩展: copy 合约桥接
//!
//! 本 crate 现在也作为 **`pc-responsible-user-denial-copy`** 在 server 端的统一入口：
//! server middleware / error handler 通过 [`copy`] 模块拿到 copy-side 代码常量 +
//! 用户可见文案渲染，run-outcome 端通过 [`codes`] + [`run_outcomes`] 处理分类与持久化。
//! 两个域（authz copy vs run-outcome classification）保持完全分离。
//!
//! ## 与 Node 的对应
//! - Node `services/responsible-user-denial-run-outcomes.ts`：
//!   - `normalizeResponsibleUserDenialCode` → [`normalize_responsible_user_denial_code_value`]
//!   - `recordResponsibleUserDenialOnActiveRun` → [`run_outcomes::record_responsible_user_denial_on_active_run`]
//! - Node `server/src/middleware/auth.ts:364` 发出 `"RESPONSIBLE_USER_UNAVAILABLE"` 等
//!   copy-side 代码 → [`copy`] 模块通过 `pc-responsible-user-denial-copy` 渲染文案。

#![forbid(unsafe_code)]

pub mod copy;
pub mod run_outcomes;

mod codes;

pub use codes::{
    is_valid_code, normalize_responsible_user_denial_code,
    normalize_responsible_user_denial_code_value, ResponsibleUserDenialCode,
};

// 便捷 re-export，让外部写 `pc_responsible_user_denial::is_responsible_user_denial_code`
// （旧 run_outcomes crate 的 API）等价于顶层 `is_valid_code`。
pub use codes::is_valid_code as is_responsible_user_denial_code;
