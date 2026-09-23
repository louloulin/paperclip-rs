//! M5-0 anchor：webhook **投递**读面 + replay —— **空 router 占位**。
//!
//! - **写者**：M5-4（`docs/44` §3.2）。切片只实现本文件的 `router()`。
//! - **路由**（`router.go` L2118–L2120）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 18 | GET | `/api/autopilots/:id/deliveries` | `ListAutopilotDeliveries` | 48 |
//! | 19 | GET | `/api/autopilots/:id/deliveries/:deliveryId` | `GetAutopilotDelivery` | 28 |
//! | 20 | POST | `/api/autopilots/:id/deliveries/:deliveryId/replay` | `ReplayAutopilotDelivery` | 117 |
//!
//! - **三条都是单形态** ⇒ 不要加尾斜杠别名。
//! - **replay 是幂等的**：`replay_idempotency_key` + `replayed_from_delivery_id`（`093`）
//!   ⇒ 重复 replay 必须打到同一条派生 delivery，不能每次都新建（需要真库测试）。
//! - **状态闭集**：`webhook_delivery.status ∈ {queued, dispatched, rejected, ignored, failed}`；
//!   `signature_status ∈ {not_required, valid, invalid, missing}`。**`reason_code` 是开放文本**
//!   （无 CHECK）⇒ 不要建封闭枚举，也不要在响应里做白名单过滤。
//! - **上游体量**：`webhook_delivery.go` 411 ⇒ 本片（execution + delivery + dispatch）合计
//!   是 M5 最重的一格，必要时按 §6.3 的口径再拆文件（**新文件不得进 `scripts/file_size_baseline.tsv`**）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M5-4 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
