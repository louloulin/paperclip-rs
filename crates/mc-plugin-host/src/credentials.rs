//! 部署密钥的**消费**侧：hook 签名密钥派生 + 密钥封装的线格式（上游 `secretbox` 同形）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`internal/util/secretbox/secretbox.go` 的 `LoadKey` / `Seal` / `Open`，
//!   以及调用方 `internal/handler/plugin_surface.go:157`、`internal/service/plugin.go:46`。
//!
//! ## 密钥的**来源**不在本文件（别搞两处）
//!
//! 环境变量 `MULTICA_PLUGIN_SECRET_KEY` 的读取在 **`mc-http::state`**（`PluginSecretKey`，
//! 与 `GoogleOAuthConfig::from_env()` 同款：读 env、解析失败 = `None`、绝不 panic）。本文件
//! 只接受 `&[u8; 32]` / `&[u8]`，**不做 `std::env::var`** —— 否则「配置的入口」会有两处，
//! 测试也无法注入。原因：`mc-plugin-host` 不依赖 `mc-http`（分层），而 `mc-http` 依赖本 crate。
//!
//! ## 线格式（必须逐字对齐，否则老数据读不出来）
//!
//! - **key** = 32 字节（AES-256-GCM）；上游口径：env 是 **base64（`StdEncoding`，带填充）**，
//!   解码后长度必须**恰好 32**；空 / 非法 base64 / 长度不对 ⇒ 上游返回 error，本仓一律
//!   当「未配置」（`None`）—— **绝不**用零密钥兜底，也**不做 trim**（trim 会让上游拒绝的
//!   `" abc "` 被接受，是放宽）。
//! - **封装块** = `nonce(12) ‖ ciphertext ‖ tag(16)`，整块进 `plugin_secret.ciphertext`（BYTEA）。
//!   这是 secretbox 的排布，**不是** `mc-secrets::cipher::EncryptedPayload` 的 base64 双字段
//!   形态（两者不同形，所以本 crate 直接用 `aes-gcm`）。
//! - **AAD / 域分隔**：上游把密钥按用途域分隔（hook 签名 / storage / callback token 等）——
//!   各派生上下文要用**不同的**标签，禁止把同一把 key 既当签名密钥又当加密密钥用。
//!
//! - **本仓约定**：`zeroize` 语义要保留（key 不做 `Debug` 打印；需要 `Debug` 就手写脱敏实现）；
//!   派生用 `hmac`/`sha2` 的既有 workspace 依赖。
//! - **不做什么**：不做密钥轮换的持久化（`plugin_installation.token_rotated_at` 是 token 的，
//!   不是这把部署密钥的）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 260 行以内（封装/解封 + 派生 + 用例）。
