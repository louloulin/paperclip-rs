//! per-task MCP overlay 的**构建** —— 上游 `integrations/composio/dispatch.go` 的
//! session URL / overlay 部分（M8-0 anchor 建桩、**M8-6 落地**：`LUM-1803`）。
//!
//! # 与 `mc-core::mcp::overlay` 的分工（`docs/61` §2.3 / R-M8-9）
//!
//! - 本文件（M8-6）：**构建** overlay 的那一端 —— 用已连接的 composio session 生成
//!   `{"mcpServers": {"composio": {...}}}`；
//! - `mc-core::mcp::overlay`（M8-3）：把 overlay **合并**进 agent 的 `mcp_config` 的纯函数。
//!
//! ⚠️ R-M8-9：本仓没有上游 `service/task.go` 那样的**中心 enqueue 函数**（task 行由 3 处
//! INSERT 分散创建）⇒ 「3 处 enqueue 接线」**不在本波写集内**，由 M8-7 登记为明确尾账。
//! 因此 `runtime_mcp_overlay` 在本波结束后**仍然恒 NULL** —— 这是**登记过的缺口**，
//! 不是遗漏。
//!
//! # 载荷形状（三处一致，改一处要改三处）
//!
//! 1. [`MCP_OVERLAY_SERVER_NAME`] 是本集成在 `mcpServers` 里的**固定键**（上游常量逐字：
//!    daemon 侧按 server 名合并 ⇒ 这个名字就是本集成的命名空间，将来别家 provider 必须另取
//!    一个名字，例如 `pipedream`）；
//! 2. 条目是 `{"type":"http","url":…}`（上游 `composioMCPServer` 逐字：`type: http` 标记它是
//!    流式 HTTP MCP 端点）；
//! 3. **顶层只有 `mcpServers` 一个键** —— `mc-core/src/mcp/overlay.rs` 的合并契约逐字：
//!    「任何不在 `mcpServers` 下的顶层键只从 agent 侧保留……overlay 今天只携带 server 条目，
//!    不得悄悄引入别的顶层键」。
//!
//! # 闸门（缺一 ⇒ `None`，即「不注入任何 server」）
//!
//! | # | 闸门 | 上游对应 |
//! | :-: | --- | --- |
//! | 1 | `toolkit_slug` 为空/纯空白 | 上游 gate 2/3（allowlist ∩ 活跃连接为空 ⇒ 无 session） |
//! | 2 | `composio_user_id` 为空/纯空白 | 上游 gate 1（没有 owner ⇒ 解析不出连接视图） |
//! | 3 | `session_url` 为空 —— 上游 gate 4（200 但没有 MCP URL） | 上游 gate 4 |
//!
//! 前两条是**本地**的形态：session 是「某个用户的某个 toolkit」的会话，两者缺一就无从谈起
//! （会话本身由 [`crate::service::ComposioService::create_mcp_session`] 按用户开）。
//!
//! # 凭据（`x-api-key`）为什么不在这里
//!
//! 本函数的签名**不带** bearer（anchor 冻结的签名如此）。composio 的 MCP 端点用
//! `x-api-key` 认证（上游 `MCPAuthHeaders` 逐字），那枚密钥的取值走
//! [`crate::client::ComposioClient::auth_headers`] ⇒ **接线时**（M8-7 尾账）由调用侧把它
//! 补进 server 条目。本函数因此可以是一条**纯函数**（可注入、可测、零 I/O）。

use serde_json::{json, Value};

/// 本集成在 `mcpServers` 里的固定 server 名（上游 `mcpOverlayServerName` 逐字）。
pub const MCP_OVERLAY_SERVER_NAME: &str = "composio";

/// 条目的 `type`（上游 `composioMCPServer.Type` 逐字：流式 HTTP MCP）。
pub const MCP_OVERLAY_SERVER_TYPE: &str = "http";

/// 用 composio 的会话信息构建 per-task overlay —— 上游 `dispatch.go` 的 `mcpOverlayPayload`。
///
/// 返回 `None` = 该用户没有可用的 composio 连接（⇒ 不注入任何 server）。
///
/// # 参数
///
/// - `toolkit_slug`：这次会话所属的 toolkit（**闸门**，不进载荷 —— 见文件头）；
/// - `session_url`：`create_mcp_session` 拿到的流式 HTTP MCP URL（进载荷）；
/// - `composio_user_id`：会话主体（**闸门**，不进载荷）。
pub fn build_task_overlay(
    toolkit_slug: &str,
    session_url: &str,
    composio_user_id: &str,
) -> Option<Value> {
    if toolkit_slug.trim().is_empty() || composio_user_id.trim().is_empty() {
        return None;
    }
    let url = session_url.trim();
    if url.is_empty() {
        return None;
    }
    Some(json!({
        "mcpServers": {
            MCP_OVERLAY_SERVER_NAME: {
                "type": MCP_OVERLAY_SERVER_TYPE,
                "url": url,
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "0b2f5a52-0b1a-4f4e-9d2f-7a1c6f0d1e2a";

    #[test]
    fn builds_the_claude_style_server_entry() {
        let overlay =
            build_task_overlay("notion", "https://mcp.example/s/abc", USER).expect("overlay");
        assert_eq!(
            overlay,
            json!({"mcpServers":{"composio":{"type":"http","url":"https://mcp.example/s/abc"}}})
        );
        assert_eq!(
            overlay["mcpServers"][MCP_OVERLAY_SERVER_NAME]["type"],
            json!(MCP_OVERLAY_SERVER_TYPE)
        );
    }

    #[test]
    fn the_payload_has_exactly_one_top_level_key() {
        let overlay =
            build_task_overlay("notion", "https://mcp.example/s/abc", USER).expect("overlay");
        let object = overlay.as_object().expect("object");
        assert_eq!(
            object.keys().collect::<Vec<_>>(),
            vec!["mcpServers"],
            "合并契约不允许别的顶层键（mc-core/src/mcp/overlay.rs 逐字）"
        );
        let servers = overlay["mcpServers"].as_object().expect("servers");
        assert_eq!(servers.keys().collect::<Vec<_>>(), vec!["composio"]);
        let entry = servers["composio"].as_object().expect("entry");
        assert_eq!(
            entry.keys().collect::<Vec<_>>(),
            vec!["type", "url"],
            "条目只带 type / url（bearer 由调用侧补，见文件头）"
        );
        assert!(
            !overlay.to_string().contains(USER),
            "用户身份不进载荷（它只是闸门）"
        );
        assert!(
            !overlay.to_string().contains("notion"),
            "toolkit 也不进载荷"
        );
    }

    #[test]
    fn every_blank_input_means_no_overlay() {
        let url = "https://mcp.example/s/abc";
        for (slug, session, user) in [
            ("", url, USER),
            ("   ", url, USER),
            ("\t", url, USER),
            ("notion", "", USER),
            ("notion", "   ", USER),
            ("notion", url, ""),
            ("notion", url, "  "),
        ] {
            assert_eq!(
                build_task_overlay(slug, session, user),
                None,
                "闸门未过 ⇒ 不注入：{slug:?}/{session:?}/{user:?}"
            );
        }
        assert!(build_task_overlay("notion", url, USER).is_some());
    }

    #[test]
    fn the_session_url_is_trimmed_but_otherwise_verbatim() {
        let overlay =
            build_task_overlay("notion", "  https://mcp.example/s/abc \n", USER).expect("overlay");
        assert_eq!(
            overlay["mcpServers"][MCP_OVERLAY_SERVER_NAME]["url"],
            json!("https://mcp.example/s/abc")
        );
    }

    #[test]
    fn an_agents_own_entry_named_composio_is_meant_to_be_overridden() {
        // M8-3 的合并函数就在做这件事；这里只钉住「本片产出的键名与它一致」。
        let overlay =
            build_task_overlay("notion", "https://mcp.example/new", USER).expect("overlay");
        let merged = mc_core::mcp::overlay::merge_task_overlay(
            &json!({"mcpServers":{"composio":{"url":"https://placeholder.example/old"}}}),
            &overlay,
        )
        .expect("merge")
        .expect("合并结果非空");
        assert_eq!(
            merged["mcpServers"]["composio"]["url"],
            json!("https://mcp.example/new"),
            "overlay 胜出（它带的是用户自己的实时 session URL）"
        );
        assert_eq!(merged["mcpServers"]["composio"]["type"], json!("http"));
    }
}
