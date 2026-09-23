//! 授权 scope 的裁决（**唯一实现点**）。
//!
//! - **写者**：M6-1（`docs/57` §3.2 / §4.2：「scope 判定唯一实现点」）。
//! - **上游**：`pkg/plugincontract` 的 scope 相关判定 + `internal/handler/plugin*.go` 的
//!   授权检查；落库列是 `plugin_installation.granted_scopes`（JSONB）。
//! - **为什么单独一个文件**：这个判定要被 **安装/启停（M6-5）、运行时面（M6-6）、
//!   公开 API + bridge（M6-7）** 三处调用。抄三份就会出现「预览能装、真装不能」这类
//!   不对称缺陷 —— 上游只有一份，本仓也必须只有一份。
//! - **与 `mc-core::plugin::PluginScope` 的分工**：那个是 **transparent newtype**（只保证
//!   字符串形态与序列化），**取值集合的权威在本文件**；`mc-core` 故意不枚举 scope 值。
//! - **本仓约定**：`fn is_granted(&self /*或 granted_scopes*/, wanted: &str) -> bool` 形态的
//!   纯函数；`granted_scopes` 为空 ⇒ **不给**任何 scope（不是「全给」）。
//! - **不做什么**：不做 OAuth 的授权码流程（那是 remote MCP 的 `mc-mcp/oauth.rs`）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 120 行以内。
