//! `webhook_delivery` 仓储（28 列）。
//!
//! - **写者**：M5-4（**W**；`docs/44` §3.2）。
//! - **上游 SQL**：`db/queries/autopilot.sql` 与 `093` / `176` 的列。
//! - **两个计数器**：`attempt_count` = 入站**去重命中**计数；`dispatch_attempts` = worker 派发尝试
//!   次数（`176` 加的 worker 列）。混用会让重试策略完全失真。
//! - **租约列**：`available_at` / `lease_token` / `lease_expires_at` / `last_attempt_at`。
//! - **`raw_body` 是 `bytea`（可空）** ⇒ 本地用 `Option<Vec<u8>>`；`selected_headers` 是
//!   **NOT NULL** 的 jsonb；`response_status` / `response_body` / `error` / `reason_code` 都是
//!   `reason_code` 属**开放文本**（无 CHECK）⇒ 不要建封闭枚举。
