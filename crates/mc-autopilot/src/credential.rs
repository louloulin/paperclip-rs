//! trigger 凭据的读写与**脱敏**（`webhook_token` / `signing_secret`）。
//!
//! - **写者**：M5-3。
//! - **上游**：`signingSecretHint`15 + `redactWebhookSecrets`19（`handler/autopilot.go`）。
//! - **契约**：响应里**只出 hint**，绝不出明文；写入两条路由（`rotate-webhook-token` /
//!   `signing-secret`）是写敏感值的路由。
//! - **通道**：走 `mc-telemetry` 的 redaction（`docs/33` §12.2），不要在本文件自己 `format!` 拼接
//!   日志字段 —— 明文进日志等于泄露。
//! - **依赖**：本 crate 已声明 `mc-telemetry`；不需要 `mc-secrets`（凭据是表列，不是 keyring）。
