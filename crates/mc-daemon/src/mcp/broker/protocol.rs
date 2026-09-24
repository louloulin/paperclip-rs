//! broker 的**纯**协议件：SSE 解码、`tools/list` 过滤、配置合并、凭据头构造。
//!
//! 这些函数不碰 socket、不碰任务生命周期，只做「字节进、字节出」的判定 —— 拆到这里既是
//! 上游 `remote_mcp_broker.go` 后半段的自然边界，也让 [`super`] 保持在门 ⑩ 的 800 行以内。

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use mc_mcp::client::canonical_input_schema;
use mc_mcp::types::{digest_bytes, Tool};
use serde_json::{json, Map, Value};

use super::super::mcp_servers_document;

/// 把 SSE 响应体解成 JSON（上游 `decodeRemoteMCPSSEData`）。
///
/// 非 `text/event-stream` 原样返回；是的话取**第一条非空 `data:` 行**。一条都没有 ⇒ 错误
/// （上游 `errors.New("Remote MCP SSE response contained no data")`）。
pub fn decode_remote_mcp_sse_data(
    content_type: Option<&str>,
    raw: &[u8],
) -> Result<Vec<u8>, String> {
    let is_event_stream = content_type
        .map(str::to_ascii_lowercase)
        .is_some_and(|value| value.starts_with("text/event-stream"));
    if !is_event_stream {
        return Ok(raw.to_vec());
    }
    for line in raw.split(|byte| *byte == b'\n') {
        let Some(rest) = line.strip_prefix(b"data:") else {
            continue;
        };
        let data = trim_ascii(rest);
        if !data.is_empty() {
            return Ok(data.to_vec());
        }
    }
    Err("Remote MCP SSE response contained no data".to_string())
}

fn trim_ascii(raw: &[u8]) -> &[u8] {
    let start = raw
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(raw.len());
    let end = raw
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |position| position + 1);
    &raw[start..end]
}

/// 按批准集合过滤 `tools/list` 的答案（上游 `filterToolsListResponse`）。
///
/// 规则：批准集合里**每一个**工具都必须在远端这次答案里出现、且 schema 摘要一致；顺序按
/// 批准的**名字升序**重排；每个工具只输出 `name` / `description` / `inputSchema` 三个键
/// （远端多给的字段一律丢掉）。任何一个对不上 ⇒ 错误（调用方翻成 `-32004`）。
pub fn filter_tools_list_response(raw: &[u8], approved: &[Tool]) -> Result<Vec<u8>, String> {
    let mut response: Map<String, Value> =
        serde_json::from_slice(raw).map_err(|err| format!("decode tools/list response: {err}"))?;
    let Some(Value::Object(result)) = response.get("result").cloned() else {
        return Err("decode tools/list result".to_string());
    };
    let Some(Value::Array(tools)) = result.get("tools").cloned() else {
        return Err("decode tools/list tools".to_string());
    };

    let mut by_name: Map<String, Value> = Map::new();
    for tool in tools {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        let canonical = canonical_input_schema(tool.get("inputSchema"));
        by_name.insert(name.to_string(), canonical);
    }

    let mut pinned: Vec<&Tool> = approved.iter().collect();
    pinned.sort_by(|left, right| left.name.cmp(&right.name));

    let mut filtered: Vec<Value> = Vec::with_capacity(pinned.len());
    for tool in pinned {
        let Some(current) = by_name.get(&tool.name) else {
            return Err("tool schema drift".to_string());
        };
        let digest = serde_json::to_vec(current)
            .map(|bytes| digest_bytes(&bytes))
            .map_err(|err| format!("encode tool schema: {err}"))?;
        if digest != tool.schema_digest {
            return Err("tool schema drift".to_string());
        }
        filtered.push(json!({
            "name": tool.name,
            "description": tool.description,
            "inputSchema": current,
        }));
    }

    let mut result = result;
    result.insert("tools".to_string(), Value::Array(filtered));
    response.insert("result".to_string(), Value::Object(result));
    serde_json::to_vec(&Value::Object(response)).map_err(|err| format!("encode response: {err}"))
}

/// 把两段 `{"mcpServers": …}` 配置合并（上游 `mergeTaskRemoteMCPConfig`）。
///
/// `overlay` 空 ⇒ `base` 原样返回；`base` 为 `null` / 空 ⇒ 当作空文档。同名条目由
/// `overlay` 覆盖。
pub fn merge_task_remote_mcp_config(
    base: Option<&Value>,
    overlay: Option<&Value>,
) -> Result<Option<Value>, String> {
    let Some(overlay) = overlay else {
        return Ok(base.cloned());
    };
    let Value::Object(overlay) = overlay else {
        return Err("decode overlay mcp config".to_string());
    };
    let overlay_servers = overlay.get("mcpServers").cloned().unwrap_or(Value::Null);
    let Value::Object(overlay_servers) = overlay_servers else {
        // 上游对该形状不做校验（`map[string]json.RawMessage` 为 nil 时是空表）。
        return Ok(base.cloned());
    };

    let mut servers: Map<String, Value> = Map::new();
    match base {
        Some(Value::Object(base)) => {
            if let Some(Value::Object(existing)) = base.get("mcpServers") {
                servers = existing.clone();
            }
        }
        Some(Value::Null) | None => {}
        Some(_) => return Err("decode base mcp config".to_string()),
    }
    for (name, server) in overlay_servers {
        servers.insert(name, server);
    }
    Ok(Some(mcp_servers_document(servers)))
}

/// 类型别名：把凭据头的构造留给调用方（不必再 `use` 一次 axum 的 http 类型）。
#[must_use]
pub fn header_map_from(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        let name = HeaderName::from_static(name);
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }
    headers
}
