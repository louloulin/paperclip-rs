//! webhook 入站仓储：按 token 解析 trigger、落 `webhook_delivery`、去重与认领。
//!
//! - **写者**：M5-5（**W**；`docs/44` §3.2）。
//! - **上游**：`persistInboundDelivery`52 + `AdmitAutopilotWebhookDelivery`68 的 SQL 侧。
//! - **去重**：`dedupe_key` / `dedupe_source` / `replay_idempotency_key` + `replayed_from_delivery_id`
//!   （replay 是 M5-4 的路由，`replay_idempotency_key` 的写入在本文件）。
//! - **并发认领**：`recoverConcurrentWebhookAdmission`26 对应「同一 delivery 被两个请求同时 admit」
//!   ⇒ 需要 `INSERT ... ON CONFLICT` / `FOR UPDATE` 级别的原子路径，不能先 SELECT 再 INSERT。
//! - **无认证入口**：本文件的查询是**唯一**在无认证路径上执行的 SQL ⇒ 参数化必须严格，
//!   且不要把 workspace 作用域交给客户端提供的值（token 才是唯一入口凭证）。
