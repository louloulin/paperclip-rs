//! Feishu / Lark 安装与扫码会话面：5 条路由（写者 **M7-14**；`docs/60-M7-PLAN.md` §1.1 / §3.3）。
//!
//! | 注册键 | `router.go` | 说明 |
//! | --- | ---: | --- |
//! | `GET /api/workspaces/:id/lark/installations` | 1783 | 列出安装（**未配置**时 200 空 + `install_supported:false`） |
//! | `DELETE /api/workspaces/:id/lark/installations/:installationId` | 1784 | 撤销一条安装 |
//! | `POST /api/workspaces/:id/lark/install/begin` | 1790 | 开一个设备流扫码会话 |
//! | `GET /api/workspaces/:id/lark/install/:sessionId/status` | 1791 | 轮询会话状态（前端按 `poll_interval_seconds` 拉） |
//! | `POST /api/lark/binding/redeem` | 1841 | 兑换用户绑定令牌（**无** workspace 前缀） |
//!
//! - **上游**：`internal/integrations/lark` 的 registration/installation/binding 面 + `internal/handler/lark.go`。
//! - **本文件的写者**：M7-14（anchor 期本文件是**空** `Router::new()` ⇒ 合并后注册键
//!   集合逐字不变，`docs/60` §6.1 的 M7-0 行）。
//! - **形态纪律**：只按上游字面量注册**那一形态**（M7 的 `dual-form required: 0` ⇒
//!   补尾斜杠是 `EXTRA_ALIAS`、漏字面量是 `MISSING_EXACT`，两类都是硬失败）。
//!   路径参数必须写 `:name`（matchit 0.7 把 `{name}` 当字面量 ⇒ 编译通过且恒 404）。
//! - **未配置语义**：部署密钥缺失 ⇒ adapter **不装配**，但本文件的路由**仍然存在**，
//!   并逐端点返回自己的"未配置"语义（**不许**统一 503；lark 列表是 200 空 +
//!   `install_supported:false`，钉在各 fixture 上，`docs/60` §2.4 / R-M7-3）。
//! - **凭据纪律**：响应/日志不得回显明文凭据（`docs/60` §2.3 四条判据）。
//! - **测试**：每条路由至少一条测试（handler 级或 e2e），且**不用** `health::placeholder`。
//!
//! ## 不做什么（anchor 期）
//!
//! 本文件**没有任何 handler**。M7-14 填自己那 5 条时，把子 router 直接写进下面的
//! `router()`；**不要**新增文件（`docs/60` §3.3 的表里本文件就是"5 个注册键的写者"）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本平台的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    // anchor：零注册键。M7-14 在这里注册上面表里的 5 条（完整路径，见 `mod.rs`
    // 的结构决策第 1 条：**不** nest 进 `workspaces.rs`）。
    Router::new()
}
