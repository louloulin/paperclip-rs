//! remote MCP 的 OAuth 授权（授权码 / token 交换 / 刷新）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`pkg/remotemcp` 的 OAuth 流程（+ `internal/service/plugin_mcp_transport.go` 的调用侧）。
//! - **本仓约定**：token 的**持久化**不是本文件的事 —— 走 `mc-secrets` 的 store（服务端密钥）
//!   ，**不要**把令牌塞进 `plugin_installation.config` 的 JSONB（那是给用户看的配置，会被
//!   原样下发/回显）。`configured_secrets` 只暴露**键名**，不暴露值（上游
//!   `pluginInstallationResponse.ConfiguredSecrets`）。
//! - **回调地址**：生产必须是本服务的公网 origin；开发态由 `devorigin.rs` 的白名单放行。
//! - **不做什么**：不做 PKCE 之外的额外加固（上游这一代就是授权码 + PKCE）；不做多租户
//!   `IdP` 发现（.well-known 的自动发现不在本波范围，源 URL 由 manifest 声明）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 220 行以内。
