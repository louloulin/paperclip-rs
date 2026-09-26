//! `GET` + `PATCH` + `PUT /api/notification-preferences/` —— **写者 M9-5**（`docs/62` §4.1 第 6 行）。
//!
//! ## anchor 期（本文件由 M9-0 `LUM-1815` 建桩，`docs/62` §5）
//!
//! **空** `Router::new()` ⇒ **零注册键**。🔴 **不得**在这里注册 501 占位：这 3 个方法都是
//! **上游键**，占位会让 ⑦ 把它们算成 `implemented_placeholder`（`owners.M9` 假清零），
//! 而 ⑨ 会从 `unmounted` 变 `mismatch`。
//!
//! ## 🔴 形态：本波**唯一**需要补尾斜杠别名的地方（`docs/62` §1.4 / R-M9-5）
//!
//! 上游是 `Route("/api/notification-preferences") + Get("/")/Patch("/")/Put("/")`（chi 的
//! `Mount` 形态）⇒ **两种形态都服务**。而 `docs/fixtures/upstream-routes.tsv` 里这 3 条的
//! 字面量**带**尾斜杠 ⇒ 门 ⑦ 会把"只注册了带斜杠那一形态"折叠算成已实现（看不见缺陷）
//! ⇒ `scripts/slash_alias_audit.py --declared docs/fixtures/m9-declared-routes.tsv` 的预测是
//! **`declared 34 / dual-form required: 3`**（`exit 1` 是预测，不是缺陷）。
//!
//! ⇒ M9-5 **必须**两个形态都注册：
//! ```text
//! .route("/api/notification-preferences",  get(…).patch(…).put(…))
//! .route("/api/notification-preferences/", get(…).patch(…).put(…))
//! ```
//! ⚠️ 这 3 条**不进** `docs/fixtures/slash-alias-allowlist.tsv`（那是我**必须补**的形态，
//! 不是欠账 —— `docs/62` §3.1 末行逐字）。
//!
//! ## 词表与校验不在本文件
//!
//! 7 个分组 × 2 个取值 + 两条错误文本在 `mc_core::notification`（anchor 定形）；
//! 表访问在 `mc_repos::notification_preference`。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 通知偏好切片（M9-5 在这里挂 3 个方法 × 2 个形态）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
