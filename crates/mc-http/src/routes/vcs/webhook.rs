//! `POST /api/webhooks/vcs/:connectionId`（`router.go:1500`，**公开块**）—— 写者 **M8-2**。
//!
//! - **凭据**：路径里的 `connectionId` 决定 workspace / provider / **解密密钥**，再用该连接
//!   的 webhook secret 验签。三种签名方案：Forgejo/Gitea = HMAC 头；GitLab =
//!   `X-Gitlab-Token` **明文常量时间比较**（`docs/61` §2.7 第 3 条）。
//! - **失败语义**：连接不存在 ⇒ 404；`VCSSecretBox` 空 ⇒ 503；验签失败 ⇒ 401（**逐条对齐**）。
//! - **本文件写者**：M8-2（anchor 期是**空** `Router::new()`）。
//! - ⚠️ 公开路由**不得**挂会话 middleware（`docs/61` §2.7 第 4 条）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 本文件的路由切片（anchor 期空实现）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
