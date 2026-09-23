//! `/v1` 的**中间件层**：凭据校验入口 + 限流档位（**锚点期是恒等函数**）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`pkg/publicapi/v1` 的凭据/限流口径（4 种凭据 + 2 档限流）。
//!
//! ## 为什么是一个 `apply`，而不是每个子文件自己挂层
//!
//! `router()` 拿不到 `Arc<AppState>`（state 由 `main.rs` 的 `with_state` 一次性注入），
//! 而 `tower_governor` 的限流桶**按 router 实例分片**：三个子 router 各挂一层 = 3 倍配额。
//! 所以层必须由 `v1/mod.rs` 在**合并点之后**加一次，签名保持**与状态无关的泛型**：
//!
//! ```text
//! pub fn apply<S>(router: Router<S>) -> Router<S> where S: Clone + Send + Sync + 'static
//! ```
//!
//! - **本 anchor 的实现是恒等**（返回入参，零注册键、零行为）⇒ 合并本片后路由表逐字不变；
//! - M6-7 落地时把 `tower_governor::GovernorLayer` 与凭据提取器接进来（`tower_governor`
//!   的依赖已在 M6-0 的根 `Cargo.toml` + `mc-http/Cargo.toml` 里声明好）。
//!
//! ## 凭据口径（M6-7 落地的判据）
//!
//! 4 种凭据：安装令牌（`mpi_`）/ 回调令牌（`mpc_`，进程内）/ 用户会话 / 无凭据的 surface 页面。
//! 校验逻辑**只有一份**（`mc_plugin_host::token`）；本层只负责「取出来、判一判、放进请求扩展」。
//! Bearer 头可以走提取器（`routes/auth_user.rs` 的 `AuthUser` 形态）⇒ **不必**用
//! `from_fn_with_state`，从而不破坏上面那条「与状态无关」的签名。
//!
//! - **不做什么**：不做 CORS（那是 `mc-http` mount 处的 `tower-http` 层）。
//!
//! **状态：M6-7 待落地**（本文件由 M6-0 anchor 建为**恒等**桩）。
//!
//! 行预算（门 ⑩）：预计 200 行以内。

use axum::Router;

/// 给 `/v1` 的合并 router 加一层（凭据 + 限流）。
///
/// **锚点期是恒等**：不注册路由、不改行为。M6-7 在此接入限流与凭据。
pub fn apply<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router
}
