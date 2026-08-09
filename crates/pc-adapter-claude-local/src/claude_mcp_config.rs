#![forbid(unsafe_code)]

//! Claude runtime MCP servers 收集 + 配置文件写盘（对齐 Node `claude-config.ts` writePaperclipClaudeMcpConfig + `execute.ts` collectRuntimeMcpIdentity）。
//!
//! 本模块提供：
//! - `RuntimeMcpServer` — 适配器无关的 runtime MCP server 描述
//! - `collect_runtime_mcp_identity` — 把 server 列表序列化为稳定的 JSON identity 字符串（用于 session resume 校验）
//! - `build_mcp_servers_config` — 构造 Claude CLI `--mcp-config` 期望的 JSON 对象
//! - `resolve_mcp_config_path` — 计算 mcp-config.json 路径
//!
//! 实际写盘由调用方负责（lib.rs::execute），本模块只做**纯逻辑**。

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// 适配器无关的 runtime MCP server 描述（对齐 `AdapterRuntimeMcpServer`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMcpServer {
    pub name: String,
    pub url: String,
    pub token: String,
    pub connection_id: String,
}

/// 计算 mcp-config.json 路径（对齐 Node `path.join(stateDir, "runs", runId, "mcp", "mcp-config.json")`）。
#[must_use]
pub fn resolve_mcp_config_path(state_dir: &Path, run_id: &str) -> PathBuf {
    state_dir
        .join("runs")
        .join(run_id)
        .join("mcp")
        .join("mcp-config.json")
}

/// 把 servers 列表序列化为稳定的 JSON identity（用于 session resume 校验）。
///
/// 排序：先按 name，再按 url，最后按 connectionId。
/// 序列化：serde_json（与 Node `JSON.stringify` 等价）。
#[must_use]
pub fn collect_runtime_mcp_identity(servers: &[RuntimeMcpServer]) -> String {
    let mut sorted: Vec<&RuntimeMcpServer> = servers.iter().collect();
    sorted.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.url.cmp(&b.url))
            .then_with(|| a.connection_id.cmp(&b.connection_id))
    });
    let views: Vec<Value> = sorted
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "url": s.url,
                "connectionId": s.connection_id,
            })
        })
        .collect();
    serde_json::to_string(&Value::Array(views)).unwrap_or_else(|_| "[]".to_owned())
}

/// 构造 Claude CLI `--mcp-config` 期望的 JSON 对象。
///
/// name 冲突解决（对齐 Node L160-170）：
/// 1. 第一次出现：`name`
/// 2. 重复：`<name>-<connectionId[0..8]>`
/// 3. 仍重复：`<name>-<connectionId[0..8]>-2`, `-3`, ...
#[must_use]
pub fn build_mcp_servers_config(servers: &[RuntimeMcpServer]) -> Map<String, Value> {
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut mcp_servers = Map::new();
    for server in servers {
        let mut name = server.name.clone();
        if used_names.contains(&name) {
            let prefix = connection_id_prefix(&server.connection_id);
            name = format!("{}-{}", server.name, prefix);
            let mut suffix: u32 = 2;
            while used_names.contains(&name) {
                name = format!("{}-{}-{}", server.name, prefix, suffix);
                suffix += 1;
            }
        }
        used_names.insert(name.clone());
        mcp_servers.insert(
            name,
            json!({
                "type": "http",
                "url": server.url,
                "headers": {
                    "Authorization": format!("Bearer {}", server.token),
                },
            }),
        );
    }
    mcp_servers
}

/// 完整 `{ "mcpServers": {...} }` 配置对象（可直接写入磁盘）。
#[must_use]
pub fn build_mcp_config_json(servers: &[RuntimeMcpServer]) -> Value {
    json!({
        "mcpServers": Value::Object(build_mcp_servers_config(servers)),
    })
}

fn connection_id_prefix(connection_id: &str) -> String {
    let end = connection_id.len().min(8);
    connection_id[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, url: &str, token: &str, conn: &str) -> RuntimeMcpServer {
        RuntimeMcpServer {
            name: name.to_owned(),
            url: url.to_owned(),
            token: token.to_owned(),
            connection_id: conn.to_owned(),
        }
    }

    #[test]
    fn resolve_mcp_config_path_joins_components() {
        let path = resolve_mcp_config_path(Path::new("/state"), "run-1");
        assert_eq!(path, PathBuf::from("/state/runs/run-1/mcp/mcp-config.json"));
    }

    #[test]
    fn collect_runtime_mcp_identity_empty() {
        assert_eq!(collect_runtime_mcp_identity(&[]), "[]");
    }

    #[test]
    fn collect_runtime_mcp_identity_single_server() {
        let servers = vec![server("a", "https://example.com", "tok", "conn-1")];
        let id = collect_runtime_mcp_identity(&servers);
        let parsed: Value = serde_json::from_str(&id).unwrap();
        assert!(parsed.is_array());
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "a");
        assert_eq!(arr[0]["url"], "https://example.com");
        assert_eq!(arr[0]["connectionId"], "conn-1");
    }

    #[test]
    fn collect_runtime_mcp_identity_is_sorted() {
        let servers = vec![
            server("z", "u", "t", "c"),
            server("a", "u", "t", "c"),
            server("m", "u", "t", "c"),
        ];
        let id = collect_runtime_mcp_identity(&servers);
        assert!(id.find("\"name\":\"a\"").unwrap() < id.find("\"name\":\"m\"").unwrap());
        assert!(id.find("\"name\":\"m\"").unwrap() < id.find("\"name\":\"z\"").unwrap());
    }

    #[test]
    fn collect_runtime_mcp_identity_stable_for_same_input() {
        let servers = vec![server("a", "u1", "t", "c"), server("b", "u2", "t", "c")];
        assert_eq!(
            collect_runtime_mcp_identity(&servers),
            collect_runtime_mcp_identity(&servers)
        );
    }

    #[test]
    fn collect_runtime_mcp_identity_deterministic_regardless_of_input_order() {
        let a = vec![server("a", "u1", "t", "c"), server("b", "u2", "t", "c")];
        let b = vec![server("b", "u2", "t", "c"), server("a", "u1", "t", "c")];
        assert_eq!(
            collect_runtime_mcp_identity(&a),
            collect_runtime_mcp_identity(&b)
        );
    }

    #[test]
    fn build_mcp_servers_config_single() {
        let servers = vec![server("a", "https://x", "tok", "conn-1")];
        let cfg = build_mcp_servers_config(&servers);
        assert_eq!(cfg.len(), 1);
        assert_eq!(cfg["a"]["type"], "http");
        assert_eq!(cfg["a"]["url"], "https://x");
        assert_eq!(cfg["a"]["headers"]["Authorization"], "Bearer tok");
    }

    #[test]
    fn build_mcp_servers_config_unique_names() {
        let servers = vec![
            server("a", "https://x1", "t1", "c1"),
            server("b", "https://x2", "t2", "c2"),
        ];
        let cfg = build_mcp_servers_config(&servers);
        assert_eq!(cfg.len(), 2);
        assert!(cfg.contains_key("a"));
        assert!(cfg.contains_key("b"));
    }

    #[test]
    fn build_mcp_servers_config_collision_appends_connection_prefix() {
        // 两个 server 共享 name="a"，但 connection_id 前缀不同
        // 第一个保留原名，第二个用 connection_id 前缀去重
        let servers = vec![
            server("a", "https://x1", "t1", "abcdef01-extra"),
            server("a", "https://x2", "t2", "fedcba99-extra"),
        ];
        let cfg = build_mcp_servers_config(&servers);
        assert!(cfg.contains_key("a"));
        assert!(cfg.contains_key("a-fedcba99"));
        assert!(!cfg.contains_key("a-abcdef01"));
    }

    #[test]
    fn build_mcp_servers_config_three_collisions_append_suffix() {
        let servers = vec![
            server("a", "u1", "t", "abc12345"),
            server("a", "u2", "t", "abc12345"),
            server("a", "u3", "t", "abc12345"),
        ];
        let cfg = build_mcp_servers_config(&servers);
        assert!(cfg.contains_key("a"));
        assert!(cfg.contains_key("a-abc12345"));
        assert!(cfg.contains_key("a-abc12345-2"));
    }

    #[test]
    fn build_mcp_servers_config_short_connection_id_uses_whole_id() {
        let servers = vec![
            server("a", "u1", "t", "short"),
            server("a", "u2", "t", "short"),
        ];
        let cfg = build_mcp_servers_config(&servers);
        assert!(cfg.contains_key("a"));
        assert!(cfg.contains_key("a-short"));
    }

    #[test]
    fn build_mcp_config_json_wraps_with_mcp_servers_key() {
        let servers = vec![server("a", "u", "t", "c")];
        let cfg = build_mcp_config_json(&servers);
        assert!(cfg.get("mcpServers").is_some());
        assert_eq!(cfg["mcpServers"]["a"]["url"], "u");
    }
}
