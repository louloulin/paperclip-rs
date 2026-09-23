//! ACP 解码器的**纯函数部分**：帧构造、通知归一化、工具名/参数归一化、用量解析、
//! 权限选项挑选。
//!
//! 这里没有状态：输入是 JSON 值或字符串，输出是字符串/[`Value`]。状态机在
//! [`super::client`]，两者分开是为了让"协议格式"与"握手流程"各自可单独读、单独测，
//! 也避免单文件过长（R7：单文件 800 行上限）。

use serde_json::{json, Map, Value};

use super::super::cli_core::decoder::{field_str, tokens};
use super::AcpToolAliases;
use crate::adapter::{LaunchRequest, TokenUsage};

/// ACP v1 的权限选项 kind（上游同值）。
const KIND_ALLOW_ONCE: &str = "allow_once";
const KIND_ALLOW_ALWAYS: &str = "allow_always";
const KIND_REJECT_ONCE: &str = "reject_once";
/// 只授权"本次会话"的 optionId（ACP 没有会话级 kind，只能按 id 认）。
const SESSION_SCOPED_OPTION_IDS: [&str; 2] = ["allow_session", "approve_for_session"];

// ── 帧构造 ──

pub(super) fn request_frame(id: i64, method: &str, params: Value) -> String {
    frame(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
}

pub(super) fn notification_frame(method: &str, params: Value) -> String {
    frame(json!({"jsonrpc": "2.0", "method": method, "params": params}))
}

pub(super) fn response_frame(id: Value, result: Value) -> String {
    frame(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub(super) fn error_frame(id: Value, code: i64, message: impl Into<String>) -> String {
    frame(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}}))
}

pub(super) fn frame(value: Value) -> String {
    let mut line = value.to_string();
    line.push('\n');
    line
}

// ── 解析助手 ──

/// 非空（去空白后）才要。
pub(super) fn non_empty(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_owned())
    }
}

/// 环境变量是否非空（先看本次 run 的 env，再看进程 env）。
pub(super) fn env_non_empty(request: &LaunchRequest, key: &str) -> bool {
    if let Some(value) = request.env.get(key) {
        return !value.trim().is_empty();
    }
    std::env::var(key).is_ok_and(|value| !value.trim().is_empty())
}

/// 应答里的 JSON-RPC 错误 → 人类可读串（`data` 是 provider 的具体原因）。
pub(super) fn rpc_error_message(
    label: &str,
    method: &str,
    object: &Map<String, Value>,
) -> Option<String> {
    let error = object.get("error")?;
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let detail = match error.get("data") {
        Some(Value::String(text)) if !text.trim().is_empty() => format!("，{text}"),
        Some(Value::Null) | None => String::new(),
        Some(other) => format!("，{other}"),
    };
    Some(format!(
        "{label} {method} 失败：code={code} {message}{detail}"
    ))
}

/// `initialize` 应答里对端提供的认证方式 id（顺序即对端给的顺序）。
pub(super) fn extract_auth_methods(result: &Value) -> Vec<String> {
    result
        .get("authMethods")
        .and_then(Value::as_array)
        .map(|methods| {
            methods
                .iter()
                .filter_map(|method| {
                    method
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| method.get("id").and_then(Value::as_str).map(str::to_owned))
                })
                .map(|id| id.trim().to_owned())
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 按 xAI 的规则挑认证方式（上游 `selectGrokAuthMethod` 逐字移植）。
///
/// 只有**对端真的宣告过**的方式才会被选中；都没有时是启动失败，而不是"随便挑
/// 一个试"或"不认证继续跑"。
pub(super) fn select_xai_auth_method(offered: &[String], have_api_key: bool) -> Result<&'static str, String> {
    const API_KEY: &str = "xai.api_key";
    const CACHED: &str = "cached_token";
    let has = |want: &str| offered.iter().any(|id| id == want);
    if have_api_key && has(API_KEY) {
        return Ok(API_KEY);
    }
    if has(CACHED) {
        return Ok(CACHED);
    }
    if has(API_KEY) {
        return Err(
            "Grok advertised only API-key authentication, but XAI_API_KEY is not set".to_owned(),
        );
    }
    if offered.is_empty() {
        return Err(
            "Grok advertised no usable authentication methods; set XAI_API_KEY or run `grok login`"
                .to_owned(),
        );
    }
    let mut advertised = offered.to_vec();
    advertised.sort();
    Err(format!(
        "Grok advertised unsupported authentication methods {advertised:?}; update Multica or authenticate with XAI_API_KEY / `grok login`"
    ))
}

/// 通知归一化：`(类型, 数据)`（上游 `normalizeACPUpdate`）。
pub(super) fn normalize_update(update: &Value) -> (String, &Value) {
    let key = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .or_else(|| update.get("type").and_then(Value::as_str));
    if let Some(key) = key {
        return (normalize_update_type(key), update);
    }
    // 外部标记形态：{"agentMessageChunk": {...}}（只有一个键时键名就是类型）。
    if let Some(object) = update.as_object() {
        if object.len() == 1 {
            if let Some((key, value)) = object.iter().next() {
                return (normalize_update_type(key), value);
            }
        }
    }
    (String::new(), update)
}

/// 类型名归一化：小写 + 去掉 `_` / `-`（上游 `normalizeACPUpdateType`）。
pub(super) fn normalize_update_type(raw: &str) -> String {
    let key: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .collect();
    match key.as_str() {
        "agentmessagechunk" => "agent_message_chunk".to_owned(),
        "agentthoughtchunk" => "agent_thought_chunk".to_owned(),
        "toolcall" => "tool_call".to_owned(),
        "toolcallupdate" => "tool_call_update".to_owned(),
        "usageupdate" => "usage_update".to_owned(),
        "turnend" | "endturn" => "turn_end".to_owned(),
        _ => String::new(),
    }
}

/// 从 `content` 块数组里取文本（上游 `extractACPToolCallText`）。
///
/// - `{type:"text", text}` → 文本；
/// - `{type:"content", content:{type:"text",text}}` → 内层文本；
/// - `{type:"diff", path, oldText, newText}` → `--- p\n+++ p` + 字节数摘要；
/// - 其余（`terminal` / `image` / 未知）忽略。
pub(super) fn content_text(data: &Value) -> String {
    let Some(blocks) = data.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    let mut pieces: Vec<String> = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or_default() {
            "content" => {
                let inner = block.get("content");
                if inner.and_then(|value| value.get("type")).and_then(Value::as_str) == Some("text")
                {
                    if let Some(text) = inner
                        .and_then(|value| value.get("text"))
                        .and_then(Value::as_str)
                    {
                        if !text.is_empty() {
                            pieces.push(text.to_owned());
                        }
                    }
                }
            }
            "diff" => {
                if let Some(path) = block.get("path").and_then(Value::as_str) {
                    if !path.is_empty() {
                        let old = block
                            .get("oldText")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let new = block
                            .get("newText")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        pieces.push(if old.is_empty() {
                            format!("--- {path}\n+++ {path}\n(new file, {} bytes)", new.len())
                        } else {
                            format!(
                                "--- {path}\n+++ {path}\n(edited: {} → {} bytes)",
                                old.len(),
                                new.len()
                            )
                        });
                    }
                }
            }
            _ => {}
        }
    }
    pieces.join("\n")
}

/// 起始帧里的工具参数（`rawInput` → `input` → `parameters`）。
pub(super) fn tool_input(data: &Value) -> Option<Value> {
    ["rawInput", "input", "parameters"]
        .iter()
        .find_map(|key| match data.get(*key) {
            Some(Value::Null) | None => None,
            Some(value) => Some(value.clone()),
        })
}

// ── 工具名 ──

/// 正文 / 思维块的文本：`content` 是**单个** ContentBlock 对象（不是数组）。
///
/// 上游 `handleAgentMessage` 只读 `content.text`，连 `type` 都不看；空串会被
/// 调用方丢掉（`DecoderState::text` 对空增量不发事件）。
pub(super) fn message_text(data: &Value) -> String {
    data.get("content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// 把 hermes 映射过的工具名再过一遍 provider 别名表（上游 `onMessage` 里的重归一）。
pub(super) fn normalize_tool_aliases(tool: &str, style: AcpToolAliases) -> String {
    let trimmed = tool.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // 冒号前才是工具名（`"Run command: ls"` → `"Run command"`）；
    // 位置 0 的冒号不算分隔符（上游 `idx > 0`）。
    let head = match trimmed.find(':') {
        Some(idx) if idx > 0 => trimmed[..idx].trim(),
        _ => trimmed,
    };
    let lower = head.to_lowercase();
    let mapped = match lower.as_str() {
        "read" | "read file" => Some("read_file"),
        "write" | "write file" => Some("write_file"),
        "edit" | "patch" => Some("edit_file"),
        "shell" | "bash" | "terminal" | "run command" | "run shell command" => Some("terminal"),
        "search" | "grep" | "find" => Some("search_files"),
        "glob" => Some("glob"),
        "web search" => Some("web_search"),
        "fetch" | "web fetch" => Some("web_fetch"),
        "todo" | "todo write" => Some("todo_write"),
        "code" if style == AcpToolAliases::Kiro => Some("code"),
        "todo list" | "todo_list" if style == AcpToolAliases::Kiro => Some("todo_write"),
        // 认不出来就只做"小写 + 空格转下划线"，给 UI 一个稳定标识。
        _ => None,
    };
    match mapped {
        Some(name) => name.to_owned(),
        None => lower.replace(' ', "_"),
    }
}

/// `tool_call` 起始帧的工具名（上游 `handleToolCallStart` 起手）。
pub(super) fn tool_name(data: &Value) -> String {
    let title = field_str(data, "title").unwrap_or_default();
    let kind = field_str(data, "kind").unwrap_or_default();
    let mapped = tool_name_from_title(&title, &kind);
    if mapped.is_empty() {
        field_str(data, "name").unwrap_or_default()
    } else {
        mapped
    }
}

/// `tool_call_update` 完成帧的工具名：`title` 为空时用 `name` 顶替（上游
/// `handleToolCallUpdate` 与起始帧的差别就在这一步）。
pub(super) fn tool_name_from_update(data: &Value) -> String {
    let mut title = field_str(data, "title").unwrap_or_default();
    if title.is_empty() {
        title = field_str(data, "name").unwrap_or_default();
    }
    let kind = field_str(data, "kind").unwrap_or_default();
    tool_name_from_title(&title, &kind)
}

/// 把 `"<动词>: <细节>"` / 裸 `kind` 归一成工具名（上游 `hermesToolNameFromTitle`）。
pub(super) fn tool_name_from_title(title: &str, kind: &str) -> String {
    if title == "execute code" {
        return "execute_code".to_owned();
    }
    if let Some((prefix, _)) = title.split_once(':') {
        let name = prefix.trim();
        if !name.is_empty() {
            return match name {
                "terminal" => "terminal".to_owned(),
                "read" => "read_file".to_owned(),
                "write" => "write_file".to_owned(),
                "search" => "search_files".to_owned(),
                "web search" => "web_search".to_owned(),
                "extract" => "web_extract".to_owned(),
                "delegate" => "delegate_task".to_owned(),
                "analyze image" => "vision_analyze".to_owned(),
                other if other.starts_with("patch") => "patch".to_owned(),
                other => other.to_owned(),
            };
        }
    }
    match kind {
        "read" => "read_file".to_owned(),
        "edit" => "write_file".to_owned(),
        "execute" => "terminal".to_owned(),
        "search" => "search_files".to_owned(),
        "fetch" => "web_search".to_owned(),
        "think" => "thinking".to_owned(),
        _ => {
            if title.is_empty() {
                kind.to_owned()
            } else {
                title.to_owned()
            }
        }
    }
}

/// 工具结果文本（`rawOutput` → `output` → `content` 块的文本）。
pub(super) fn tool_output(data: &Value) -> String {
    raw_text(data.get("rawOutput"))
        .or_else(|| raw_text(data.get("output")))
        .unwrap_or_else(|| content_text(data))
}

/// 任意 JSON 值 → 文本：字符串原样，其余保留 JSON 文本（上游 `acpRawText`）。
pub(super) fn raw_text(raw: Option<&Value>) -> Option<String> {
    match raw {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            if text.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        Some(other) => Some(other.to_string()),
    }
}

/// 延迟发射时累积的参数文本 → `input`：能当对象解析就用它，否则包成 `{"text": …}`。
pub(super) fn parse_tool_args(args_text: &str) -> Value {
    let trimmed = args_text.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    serde_json::from_str::<Map<String, Value>>(trimmed)
        .map(Value::Object)
        .unwrap_or_else(|_| json!({"text": trimmed}))
}

/// ACP 用量快照的字段别名（上游 `parseACPTokenUsageSnapshot` 逐条对应）。
pub(super) fn parse_usage(raw: Option<&Value>) -> Option<TokenUsage> {
    let Some(value) = raw.and_then(Value::as_object) else {
        return None;
    };
    let pick = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| value.get(*key))
            .map_or(0, |found| {
                found
                    .as_u64()
                    .or_else(|| found.as_f64().map(|number| number as u64))
                    .or_else(|| found.as_str().and_then(|text| text.parse().ok()))
                    .unwrap_or(0)
            })
    };
    let input = pick(&["inputTokens", "input_tokens"]);
    let output = pick(&["outputTokens", "output_tokens"]);
    let cache_read = pick(&[
        "cachedReadTokens",
        "cacheReadTokens",
        "cached_input_tokens",
        "cache_read_tokens",
        "cache_read_input_tokens",
    ]);
    let cache_write = pick(&[
        "cachedWriteTokens",
        "cacheWriteTokens",
        "cache_write_tokens",
        "cache_creation_input_tokens",
    ]);
    let total = pick(&["totalTokens", "total_tokens"]);
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 && total == 0 {
        return None;
    }
    let mut usage = tokens(input, output, cache_read, cache_write);
    if total > usage.total_tokens {
        usage.total_tokens = total;
    }
    Some(usage)
}

/// prompt 应答 `_meta` 里的模型名（grok 每 turn 都盖 `modelId`）。
pub(super) fn parse_model_id(meta: Option<&Value>) -> Option<String> {
    let meta = meta?.as_object()?;
    field_str(&Value::Object(meta.clone()), "modelId")
        .or_else(|| field_str(&Value::Object(meta.clone()), "model_id"))
        .and_then(|value| non_empty(Some(value.as_str())))
}

/// 用量事件归属的模型名（没报就退回 provider 名）。
pub(super) fn model_for_usage(model: Option<&str>, label: &str) -> String {
    non_empty(model).unwrap_or_else(|| label.to_owned())
}

/// 挑一个可安全自动选中的权限选项（上游 `selectACPPermissionOption`）。
///
/// 永不选"永久授权"（`allow_always` 且非会话级 id）：它在部分后端会写进磁盘
/// 白名单、活过本次任务；也永不回 `cancelled`（有后端读成取消整个 turn）。
/// 只有**对端真的提供过**的 optionId 会被选。
pub(super) fn select_permission_option(options: &[Value]) -> Option<String> {
    let option_id = |option: &Value| field_str(option, "optionId").unwrap_or_default();
    let kind = |option: &Value| field_str(option, "kind").unwrap_or_default();

    // 1) 已知的会话级授权 id，且对端确实把它标成了授权类。
    for want in SESSION_SCOPED_OPTION_IDS {
        for option in options {
            if option_id(option) == want && is_grant_kind(&kind(option)) {
                return Some(option_id(option));
            }
        }
    }
    // 2) 任意单次授权（id 不透明也无所谓，allow_once 本身就是一次性的）。
    for option in options {
        if !option_id(option).is_empty() && kind_is(&kind(option), KIND_ALLOW_ONCE) {
            return Some(option_id(option));
        }
    }
    // 3) 没有可安全授权的：用对端提供的单次拒绝拒掉**这一次动作**。
    for option in options {
        if !option_id(option).is_empty() && kind_is(&kind(option), KIND_REJECT_ONCE) {
            return Some(option_id(option));
        }
    }
    // 4) 空 / 畸形 / 只有永久项 → 交给调用方回协议错误（fail-closed）。
    None
}

/// kind 是否相等（大小写不敏感 + 去空白，上游 `strings.EqualFold`）。
pub(super) fn kind_is(kind: &str, want: &str) -> bool {
    kind.trim().eq_ignore_ascii_case(want)
}

/// kind 是否属于授权类（fail-closed：未知 kind 一律不算授权，上游 `isACPGrantKind`）。
pub(super) fn is_grant_kind(kind: &str) -> bool {
    kind_is(kind, KIND_ALLOW_ONCE) || kind_is(kind, KIND_ALLOW_ALWAYS)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn offered(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn grok_auth_selection_follows_the_upstream_rules() {
        assert_eq!(
            select_xai_auth_method(&offered(&["cached_token", "xai.api_key"]), true).unwrap(),
            "xai.api_key"
        );
        assert_eq!(
            select_xai_auth_method(&offered(&["cached_token", "xai.api_key"]), false).unwrap(),
            "cached_token"
        );
        let error = select_xai_auth_method(&offered(&["xai.api_key"]), false).unwrap_err();
        assert!(error.contains("XAI_API_KEY is not set"), "{error}");
        let error = select_xai_auth_method(&[], false).unwrap_err();
        assert!(error.contains("no usable authentication methods"), "{error}");
        let error = select_xai_auth_method(&offered(&["oauth"]), false).unwrap_err();
        assert!(error.contains("unsupported authentication methods"), "{error}");
        // 空白 id 不算"提供过"。
        assert_eq!(
            select_xai_auth_method(&offered(&["", "cached_token"]), false).unwrap(),
            "cached_token"
        );
    }

    #[test]
    fn auth_methods_are_read_from_strings_and_objects() {
        let result = json!({"authMethods": [{"id": "cached_token"}, "xai.api_key", {"id": "  "}, 7]});
        assert_eq!(extract_auth_methods(&result), vec!["cached_token", "xai.api_key"]);
        assert!(extract_auth_methods(&json!({})).is_empty());
    }

    #[test]
    fn tool_names_are_normalized_like_upstream() {
        assert_eq!(tool_name_from_title("execute code", ""), "execute_code");
        // 上游只认小写前缀（`hermesToolNameFromTitle`），大小写由各家的别名表处理。
        assert_eq!(tool_name_from_title("terminal: ls -l", "execute"), "terminal");
        assert_eq!(tool_name_from_title("Shell: ls -l", "execute"), "Shell");
        assert_eq!(tool_name_from_title("read: a.rs", ""), "read_file");
        // `patch` 走前缀匹配，且大小写敏感（"Patch" 原样返回，交给别名表）。
        assert_eq!(tool_name_from_title("patch (replace): a.rs", ""), "patch");
        assert_eq!(tool_name_from_title("Patch: a.rs", ""), "Patch");
        assert_eq!(tool_name_from_title("web search: x", ""), "web_search");
        assert_eq!(tool_name_from_title("Read file: a.rs", ""), "Read file");
        assert_eq!(tool_name_from_title("analyze image: a.png", ""), "vision_analyze");
        assert_eq!(tool_name_from_title("", "fetch"), "web_search");
        assert_eq!(tool_name_from_title("", "think"), "thinking");
        // kimi 会发没有 kind 的裸标题（保留原样，交给别名表归一）。
        assert_eq!(tool_name_from_title("Shell", ""), "Shell");
        assert_eq!(tool_name_from_title("", "weird"), "weird");
        // 冒号开头不当作前缀。
        assert_eq!(tool_name_from_title(": x", "read"), "read_file");
    }

    #[test]
    fn tool_name_prefers_the_normalized_title_then_falls_back_to_name() {
        assert_eq!(tool_name(&json!({"title": "terminal: ls", "name": "shell"})), "terminal");
        assert_eq!(tool_name(&json!({"name": "shell"})), "shell");
        assert_eq!(tool_name(&json!({})), "");
        // 完成帧：`title` 为空时用 `name` 顶替（起始帧不这么做）。
        assert_eq!(tool_name_from_update(&json!({"name": "read", "kind": "read"})), "read_file");
        assert_eq!(tool_name(&json!({"name": "read", "kind": "read"})), "read_file");
    }

    #[test]
    fn provider_tool_aliases_normalize_capitalised_titles() {
        // kimi/qoder/traecli/grok 共用一张表。
        assert_eq!(normalize_tool_aliases("Read file: /x", AcpToolAliases::Kimi), "read_file");
        assert_eq!(normalize_tool_aliases("Run command: ls", AcpToolAliases::Kimi), "terminal");
        assert_eq!(normalize_tool_aliases("Write file: /x", AcpToolAliases::Kimi), "write_file");
        assert_eq!(normalize_tool_aliases("Read file", AcpToolAliases::Kimi), "read_file");
        assert_eq!(normalize_tool_aliases("Glob: *.rs", AcpToolAliases::Kimi), "glob");
        assert_eq!(normalize_tool_aliases("Fetch: http://x", AcpToolAliases::Kimi), "web_fetch");
        assert_eq!(normalize_tool_aliases("Todo write", AcpToolAliases::Kimi), "todo_write");
        // 未识别的名字：小写 + 空格转下划线（给 UI 稳定标识）。
        assert_eq!(normalize_tool_aliases("Shell Tool", AcpToolAliases::Kimi), "shell_tool");
        // 已归一的名字是幂等的。
        assert_eq!(normalize_tool_aliases("read_file", AcpToolAliases::Kimi), "read_file");
        assert_eq!(normalize_tool_aliases("", AcpToolAliases::Kimi), "");
        // kiro 的表多两个别名。
        assert_eq!(normalize_tool_aliases("code", AcpToolAliases::Kimi), "code");
        assert_eq!(normalize_tool_aliases("code", AcpToolAliases::Kiro), "code");
        assert_eq!(
            normalize_tool_aliases("Todo list", AcpToolAliases::Kimi),
            "todo_list"
        );
        assert_eq!(
            normalize_tool_aliases("Todo list", AcpToolAliases::Kiro),
            "todo_write"
        );
        assert_eq!(
            normalize_tool_aliases("Edit: a.rs", AcpToolAliases::Kiro),
            "edit_file"
        );
    }

    #[test]
    fn message_text_reads_the_single_content_block() {
        assert_eq!(
            message_text(&json!({"content": {"type": "text", "text": "hi"}})),
            "hi"
        );
        assert_eq!(message_text(&json!({"content": {}})), "");
        assert_eq!(message_text(&json!({"content": [{"type": "text", "text": "hi"}]})), "");
    }

    #[test]
    fn diff_blocks_render_a_summary() {
        let data = json!({"content": [
            {"type": "diff", "path": "a.rs", "oldText": "", "newText": "abcd"},
            // 裸 `text` 块不参与拼接（上游同样只认 content/diff 两种）。
            {"type": "text", "text": "ignored"},
            {"type": "content", "content": {"type": "text", "text": "tail"}},
        ]});
        let text = content_text(&data);
        assert!(text.contains("--- a.rs\n+++ a.rs\n(new file, 4 bytes)"), "{text}");
        assert!(text.ends_with("tail"), "{text}");
        assert!(!text.contains("ignored"), "{text}");
    }

    #[test]
    fn edits_and_nested_content_blocks_are_rendered() {
        let data = json!({"content": [
            {"type": "diff", "path": "a.rs", "oldText": "old", "newText": "newer"},
            {"type": "content", "content": {"type": "text", "text": "nested"}},
            {"type": "terminal", "terminalId": "t1"},
            {"type": "image", "data": "…"},
        ]});
        let text = content_text(&data);
        assert!(text.contains("(edited: 3 → 5 bytes)"), "{text}");
        assert!(text.contains("nested"), "{text}");
        assert!(!text.contains("t1"), "终端块不参与文本拼接：{text}");
    }

    #[test]
    fn raw_text_preserves_non_string_payloads() {
        assert_eq!(raw_text(Some(&json!("plain"))), Some("plain".to_owned()));
        assert_eq!(raw_text(Some(&json!({"a": 1}))), Some("{\"a\":1}".to_owned()));
        assert_eq!(raw_text(Some(&json!(""))), None);
        assert_eq!(raw_text(None), None);
    }

    #[test]
    fn tool_arguments_are_parsed_or_wrapped() {
        assert_eq!(parse_tool_args("{\"a\":1}")["a"], 1);
        assert_eq!(parse_tool_args("not json")["text"], "not json");
        assert_eq!(parse_tool_args("  "), Value::Null);
        // 数组不是对象 → 按文本包起来（上游同样只认 map）。
        assert_eq!(parse_tool_args("[1]")["text"], "[1]");
    }

    #[test]
    fn usage_aliases_and_explicit_totals_are_honored() {
        let usage = parse_usage(Some(&json!({"input_tokens": 3, "outputTokens": 4}))).unwrap();
        assert_eq!((usage.input, usage.output, usage.total_tokens), (3, 4, 7));
        let usage = parse_usage(Some(&json!({"inputTokens": 3, "totalTokens": 99}))).unwrap();
        assert_eq!(usage.total_tokens, 99);
        assert!(parse_usage(Some(&json!({"inputTokens": 0}))).is_none());
        assert!(parse_usage(Some(&json!("nope"))).is_none());
        assert!(parse_usage(None).is_none());
    }

    #[test]
    fn model_id_is_read_from_meta_in_both_spellings() {
        assert_eq!(parse_model_id(Some(&json!({"modelId": "m1"}))), Some("m1".to_owned()));
        assert_eq!(parse_model_id(Some(&json!({"model_id": "m2"}))), Some("m2".to_owned()));
        assert_eq!(parse_model_id(Some(&json!({"modelId": "  "}))), None);
        assert_eq!(parse_model_id(None), None);
    }

    #[test]
    fn update_type_normalization_accepts_aliases() {
        assert_eq!(normalize_update_type("Agent_Message-Chunk"), "agent_message_chunk");
        assert_eq!(normalize_update_type("TurnEnd"), "turn_end");
        assert_eq!(normalize_update_type("endTurn"), "turn_end");
        assert_eq!(normalize_update_type("future"), "");
    }

    #[test]
    fn empty_text_is_never_emitted() {
        let data = json!({"content": [{"type": "text", "text": ""}]});
        assert_eq!(content_text(&data), "");
    }
}
