//! 插件令牌：安装令牌（安装方调用宿主）与回调令牌（宿主带着回调进插件）的签发/校验。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`internal/service/plugin_token.go` —— `installTokenPrefix = "mpi_"`(36) /
//!   `callbackTokenPrefix = "mpc_"`(37) / `IssueInstallToken`(47，`base64.RawURLEncoding`) /
//!   `InstallCredentials`(65) / `RotateInstallCredentials`(74) / `RevokeInstallToken`(92) /
//!   `AuthenticateInstallToken`(103) / `hashToken`(121) / `CallbackGrant`(131) /
//!   `CallbackTokens`(166)。
//!
//! ## 两条**不能混**的令牌族
//!
//! | 族 | 前缀 | 落库 | 生命周期 |
//! | --- | --- | --- | --- |
//! | 安装令牌 | `mpi_` | 只有**哈希**进 `plugin_installation.token_hash`（+ `token_rotated_at`） | 可轮换 / 可吊销 |
//! | 回调令牌 | `mpc_` | **不落库**（进程内 `CallbackTokens`，带 sweep） | 短命、单次授权用 |
//!
//! 明文令牌只在**签发那一刻**存在；`token_hash` 是唯一的持久形态。回调令牌**绝不**进
//! iframe（`docs/57` §4.2 M6-7 的安全约束）。
//!
//! - **本仓约定**：`mc-core::plugin::PluginTokenKind` 已经固定了两个前缀常量
//!   （`Install` → `mpi_`、`Callback` → `mpc_`），**以它为准**，本文件不要再写一遍字面量；
//!   哈希用 `sha2`（纯 hex，不带前缀）。
//! - **不做什么**：不发 JWT（上游就是随机令牌 + 哈希比对）；不做 token 的跨进程共享
//!   （回调令牌是进程内的，多实例部署下由 M6-7 的 sticky/回源策略处理，本波不扩面）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 240 行以内。
