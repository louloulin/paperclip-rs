//! 入站限流。
//!
//! - **写者**：M5-5。
//! - **上游**：`writeWebhookRateLimit`22 + `clientIPForRateLimit`22 + `remoteAddrHost`17 +
//!   `parseNetIPAddr`11 + `addrInPrefixes`9。
//! - **注意**：这是**无认证入口**上的限流，键必须取自可信来源（远端地址），不要用客户端可伪造的
//!   header（除非在代理白名单之后，那时要与 `addrInPrefixes` 的语义一起判）。
