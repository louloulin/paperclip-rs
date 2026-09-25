//! composio 面聚合：**5 条**注册键（`docs/61-M8-PLAN.md` §1.1 的第 21–25 行）—— 写者 **M8-6**。
//!
//! | 注册键 | 方法 | `router.go` | 文件 |
//! | --- | :-: | ---: | --- |
//! | `/api/integrations/composio/callback` | GET | 1517 | `callback.rs` |
//! | `/api/integrations/composio/connect/init` | POST | 1866 | `connect.rs` |
//! | `/api/integrations/composio/toolkits` | GET | 1867 | `catalog.rs` |
//! | `/api/integrations/composio/connections` | GET | 1868 | `catalog.rs` |
//! | `/api/integrations/composio/connections/:id` | DELETE | 1869 | `catalog.rs` |
//!
//! - **归属**：连接属于**用户**，不属于 workspace（**会话级 user 面**，§1.1 第 4 簇）。
//! - **授权层**：callback 在**公开块**（凭 `state` HMAC 从会话之外取身份）；
//!   其余 4 条在 Auth 组内（匿名 ⇒ 401）。
//! - **四种「未配置」缺一即不装配**（`docs/61` §2.5）：缺 `COMPOSIO_API_KEY` / flag 关 /
//!   缺 state secret / 缺回调基址 ⇒ **503**；**不许**统一成 401。
//! - **⑨ 的唯一 M8 fixture**：`integrations/TestComposioCallbackIsPublic_NoCookieNot401`
//!   断言匿名 + 错 state ⇒ **401**（**不是** 404）—— 本波承诺它 `unmounted → pass`（§6.2）。
//! - **anchor 期**：三个子文件全是**空** `Router::new()` ⇒ 合并后**零注册键**。

pub mod callback;
pub mod catalog;
pub mod connect;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// composio 面的聚合 router（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(callback::router())
        .merge(connect::router())
        .merge(catalog::router())
}
