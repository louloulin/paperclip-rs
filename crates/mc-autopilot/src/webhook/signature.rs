//! provider 签名校验。
//!
//! - **写者**：M5-5。
//! - **上游**：`verifyWebhookSignatureForProvider`18 + `verifyHubSignature`18。
//! - **落库列**：`webhook_delivery.signature_status ∈ {not_required,valid,invalid,missing}`。
//! - **依赖**：`hmac` / `sha2` / `base64` / `hex` **不在本 crate 的依赖表里**
//!   （§5.2 只给了 serde/uuid/chrono/chrono-tz/sqlx/thiserror/tracing）⇒ 要用就在 M5-5 的 PR 里
//!   走依赖仲裁（`docs/15` §8.4），或复用 `mc-core` 已有的实现（`mc-core` 已声明 hmac/sha2/hex）。
