//! job 注册与分发（M5-8）。
//!
//! - **写者**：M5-8（`jobs/**` 整组）。
//! - **上游**：`scheduler/jobs_autopilot.go`448（449）+ `scheduler/jobs_issue_wakeup.go`21（22）。
//! - **依赖方向**：M5-8 ← M5-7 的内核 + M5-4 的 dispatch + M5-6 的 wakeup
//!   （`docs/44` §4.3 的串行边）⇒ 本组只能在内核与两个面**合并之后**动。
//! - **注册点**：`apps/mc-server/src/main.rs` 的 spawn 块由 M5-7 写、M5-8 往注册表加 2 行
//!   （串行边，不是并发写）。
pub mod autopilot;
pub mod issue_wakeup;

use crate::error::SchedulerResult;
use crate::manager::Manager;

/// 把本波所有 job 注册进管理器。
///
/// **M5-7 只落空实现**：内核必须能在「0 个 job」下空转并干净退出（本切片据此独立验收）。
/// M5-8 往这里加两行：
///
/// * autopilot（上游 `jobs_autopilot.go:448`）—— 需要 M5-4 的 dispatch 面（串行边）；
/// * issue wakeup（上游 `jobs_issue_wakeup.go:21`）—— 需要 M5-6 的 wakeup 面（串行边）。
///
/// 调用方：`apps/mc-server/src/main.rs` 的 spawn 块（`manager.register` 必须在 `spawn` 前）。
pub fn register_all(_manager: &mut Manager) -> SchedulerResult<()> {
    // M5-8：`_manager.register(crate::jobs::autopilot::job()?)?;` …
    Ok(())
}
