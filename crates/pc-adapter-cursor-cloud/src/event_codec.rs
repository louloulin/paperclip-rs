//! Cursor Cloud JSONL 事件编解码 — 对齐 Node
//! `packages/adapters/cursor-cloud/src/ui/parse-stdout.ts`。
//!
//! 4 类事件：
//! - `cursor_cloud.init`    — session/agent/runId 初始化
//! - `cursor_cloud.status`  — 状态消息
//! - `cursor_cloud.message` — SDK 消息包装
//! - `cursor_cloud.result`  — 最终结果（status/result/model/durationMs/git/error）
//!
//! 6 类 SDK 内部消息：
//! - assistant / user / thinking
//! - tool_call (status=running|completed|error)
//! - tool_result / task / status

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Cursor Cloud 顶层事件（写向 stdout 的 JSONL）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CursorCloudEvent {
    #[serde(rename = "cursor_cloud.init")]
    Init {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
        #[serde(rename = "runId", skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    #[serde(rename = "cursor_cloud.status")]
    Status {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "cursor_cloud.message")]
    Message { message: Value },
    #[serde(rename = "cursor_cloud.result")]
    Result {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        git: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Cursor Cloud 事件 → single-line JSON string (供 adapter onLog 写出)。
pub fn event_line(event: &CursorCloudEvent) -> String {
    let line = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_owned());
    format!("{line}\n")
}

/// 子结构：SDK message kind（解析时仅消费必要字段，其他字段透传）。
#[derive(Debug, Clone, PartialEq)]
pub enum SdkMessageKind {
    Assistant {
        text: String,
    },
    User {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCallRunning {
        name: String,
        tool_use_id: String,
        input: Value,
    },
    ToolCallCompleted {
        name: String,
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    ToolResult {
        name: String,
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    System {
        text: String,
    },
    Unknown(Value),
}

fn as_record(v: &Value) -> Option<&serde_json::Map<String, Value>> {
    v.as_object()
}

fn read_string(v: Option<&Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").to_owned()
}

/// 解析 SDK `assistant` message (嵌套 `message.content[]`) → SdkMessageKind。
fn parse_assistant_message(message: &Value) -> Vec<SdkMessageKind> {
    let mut entries = Vec::new();
    let content = match message.get("content").and_then(|v| v.as_array()) {
        Some(c) => c,
        None => return entries,
    };
    for part in content {
        let Some(part) = as_record(part) else {
            continue;
        };
        let kind = read_string(part.get("type")).trim().to_owned();
        if kind == "text" {
            let text = read_string(part.get("text")).trim().to_owned();
            if !text.is_empty() {
                entries.push(SdkMessageKind::Assistant { text });
            }
        } else if kind == "tool_use" {
            entries.push(SdkMessageKind::ToolCallRunning {
                name: read_string(part.get("name")),
                tool_use_id: read_string(part.get("id")),
                input: part.get("input").cloned().unwrap_or(Value::Null),
            });
        }
    }
    entries
}

/// 解析 SDK `user` message → 抽取多段 text 合并。
fn parse_user_message(message: &Value) -> Vec<SdkMessageKind> {
    let Some(content) = message.get("content").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let text = content
        .iter()
        .filter_map(|e| as_record(e))
        .map(|e| read_string(e.get("text")).trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        Vec::new()
    } else {
        vec![SdkMessageKind::User { text }]
    }
}

/// 解析 SDK message 包装（SDKMessage.type ∈ {assistant,user,thinking,...}）。
fn parse_sdk_message_inner(message: &Value) -> Vec<SdkMessageKind> {
    let kind = read_string(message.get("type"));
    match kind.as_str() {
        "assistant" => {
            let body = message.get("message").cloned().unwrap_or(Value::Null);
            parse_assistant_message(&body)
        }
        "user" => {
            let body = message.get("message").cloned().unwrap_or(Value::Null);
            parse_user_message(&body)
        }
        "thinking" => {
            let text = read_string(message.get("text")).trim().to_owned();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![SdkMessageKind::Thinking { text }]
            }
        }
        "tool_call" => {
            let tool_use_id = read_string(message.get("call_id")).trim().to_owned();
            let tool_use_id = if tool_use_id.is_empty() {
                read_string(message.get("id"))
            } else {
                tool_use_id
            };
            let status = read_string(message.get("status")).to_lowercase();
            let name = read_string(message.get("name"));
            let input = message.get("args").cloned().unwrap_or(Value::Null);
            if status == "running" {
                vec![SdkMessageKind::ToolCallRunning {
                    name,
                    tool_use_id,
                    input,
                }]
            } else if status == "completed" || status == "error" {
                vec![SdkMessageKind::ToolCallCompleted {
                    name,
                    tool_use_id,
                    content: stringify_unknown(message.get("result").cloned().unwrap_or(input)),
                    is_error: status == "error",
                }]
            } else {
                Vec::new()
            }
        }
        "tool_result" => {
            let tool_use_id = read_string(message.get("call_id"));
            let tool_use_id = if tool_use_id.is_empty() {
                read_string(message.get("id"))
            } else {
                tool_use_id
            };
            let is_error = message
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || read_string(message.get("status")).to_lowercase() == "error";
            vec![SdkMessageKind::ToolResult {
                name: read_string(message.get("name")),
                tool_use_id,
                content: stringify_unknown(message.get("result").cloned().unwrap_or_else(|| {
                    message
                        .get("content")
                        .cloned()
                        .unwrap_or_else(|| message.get("output").cloned().unwrap_or(Value::Null))
                })),
                is_error,
            }]
        }
        "status" => {
            let status = read_string(message.get("status"));
            let message = read_string(message.get("message"));
            let text = if message.is_empty() {
                format!("status: {status}")
            } else {
                format!("status: {status} - {message}")
            };
            vec![SdkMessageKind::System { text }]
        }
        "task" => {
            let text = read_string(message.get("text")).trim().to_owned();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![SdkMessageKind::System { text }]
            }
        }
        _ => vec![SdkMessageKind::Unknown(message.clone())],
    }
}

fn stringify_unknown(v: Value) -> String {
    match v {
        Value::String(s) => s,
        Value::Null => String::new(),
        v => serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()),
    }
}

/// Cursor Cloud stdout line → SdkMessageKind 列表（用于 transcript 解析）。
///
/// 顶层 JSON 包装（`type ∈ {init,status,message,result}`）会先把内部 message
/// 解包；其他 JSON / 非 JSON 行被视为 SdkMessageKind::System（stdout 透传）。
pub fn parse_cursor_cloud_stdout_line(line: &str) -> Vec<SdkMessageKind> {
    let trimmed = line.trim();
    let parsed: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => {
            return vec![SdkMessageKind::System {
                text: trimmed.to_owned(),
            }]
        }
    };
    let Some(obj) = as_record(&parsed) else {
        return vec![SdkMessageKind::System {
            text: trimmed.to_owned(),
        }];
    };
    let top_kind = read_string(obj.get("type"));
    match top_kind.as_str() {
        "cursor_cloud.init" => {
            let session_id = read_string(obj.get("sessionId"));
            let session_id = if session_id.is_empty() {
                read_string(obj.get("agentId"))
            } else {
                session_id
            };
            let model = read_string(obj.get("model"));
            let text = if model.is_empty() {
                "cursor_cloud".to_owned()
            } else {
                model.clone()
            };
            vec![SdkMessageKind::Assistant {
                text: format!("init session={session_id} model={text}"),
            }]
        }
        "cursor_cloud.status" => {
            let status = read_string(obj.get("status"));
            let message = read_string(obj.get("message"));
            let text = if message.is_empty() {
                status.clone()
            } else {
                format!("{status}: {message}")
            };
            vec![SdkMessageKind::System { text }]
        }
        "cursor_cloud.message" => match obj.get("message") {
            Some(inner) => parse_sdk_message_inner(inner),
            None => Vec::new(),
        },
        "cursor_cloud.result" => {
            let status = read_string(obj.get("status"));
            vec![SdkMessageKind::System {
                text: format!("result status={status}"),
            }]
        }
        _ => vec![SdkMessageKind::System {
            text: trimmed.to_owned(),
        }],
    }
}

/// 便捷构造：`init` event（session/agent/runId 已知时）。
pub fn init_event(
    session_id: impl Into<String>,
    agent_id: impl Into<String>,
    run_id: Option<&str>,
    model: Option<&str>,
) -> CursorCloudEvent {
    CursorCloudEvent::Init {
        session_id: session_id.into(),
        agent_id: agent_id.into(),
        run_id: run_id.map(str::to_owned),
        model: model.map(str::to_owned),
    }
}

/// 便捷构造：`status` event。
pub fn status_event(status: impl Into<String>, message: Option<&str>) -> CursorCloudEvent {
    CursorCloudEvent::Status {
        status: status.into(),
        message: message.map(str::to_owned),
    }
}

/// 便捷构造：`result` event。
pub fn result_event(
    status: impl Into<String>,
    result: Option<&str>,
    model: Option<&str>,
    duration_ms: Option<u64>,
    git: Option<Value>,
    error: Option<&str>,
) -> CursorCloudEvent {
    CursorCloudEvent::Result {
        status: status.into(),
        result: result.map(str::to_owned),
        model: model.map(str::to_owned),
        duration_ms,
        git,
        error: error.map(str::to_owned),
    }
}

/// 一行 message 包装（供 onLog 调用）。
pub fn message_event(message: Value) -> CursorCloudEvent {
    CursorCloudEvent::Message { message }
}

/// Helper：序列化纯 SDK message 字段到 JSON（不带外层）。
pub fn serialize_sdk_assistant(text: &str) -> Value {
    json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_line_includes_trailing_newline() {
        let line = event_line(&init_event("s1", "a1", Some("r1"), Some("gpt-4")));
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["type"], "cursor_cloud.init");
        assert_eq!(parsed["sessionId"], "s1");
        assert_eq!(parsed["agentId"], "a1");
        assert_eq!(parsed["runId"], "r1");
        assert_eq!(parsed["model"], "gpt-4");
    }

    #[test]
    fn event_line_skips_none_optional_fields_in_init() {
        let line = event_line(&init_event("s1", "a1", None, None));
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert!(parsed.get("runId").is_none());
        assert!(parsed.get("model").is_none());
    }

    #[test]
    fn event_line_includes_status_message_when_present() {
        let e = status_event("running", Some("started"));
        let v: Value = serde_json::from_str(event_line(&e).trim_end()).unwrap();
        assert_eq!(v["status"], "running");
        assert_eq!(v["message"], "started");
    }

    #[test]
    fn event_line_result_includes_error_when_present() {
        let e = result_event("error", None, None, None, None, Some("oops"));
        let v: Value = serde_json::from_str(event_line(&e).trim_end()).unwrap();
        assert_eq!(v["error"], "oops");
    }

    #[test]
    fn parse_stdout_line_init_emits_assistant_text() {
        let line = r#"{"type":"cursor_cloud.init","sessionId":"s1","agentId":"a1","runId":"r1","model":"gpt-4"}"#;
        let out = parse_cursor_cloud_stdout_line(line);
        assert!(matches!(out[0], SdkMessageKind::Assistant { .. }));
    }

    #[test]
    fn parse_stdout_line_status_includes_message() {
        let line = r#"{"type":"cursor_cloud.status","status":"running","message":"started"}"#;
        let out = parse_cursor_cloud_stdout_line(line);
        let SdkMessageKind::System { text } = &out[0] else {
            panic!("expected system");
        };
        assert_eq!(text, "running: started");
    }

    #[test]
    fn parse_stdout_line_message_assistant_text() {
        let sdk = serialize_sdk_assistant("hi");
        let line = json!({"type":"cursor_cloud.message","message":sdk}).to_string();
        let out = parse_cursor_cloud_stdout_line(&line);
        assert!(matches!(out[0], SdkMessageKind::Assistant { ref text } if text == "hi"));
    }

    #[test]
    fn parse_stdout_line_message_thinking() {
        let sdk = json!({"type":"thinking","text":"deep thoughts"});
        let line = json!({"type":"cursor_cloud.message","message":sdk}).to_string();
        let out = parse_cursor_cloud_stdout_line(&line);
        assert!(matches!(out[0], SdkMessageKind::Thinking { ref text } if text == "deep thoughts"));
    }

    #[test]
    fn parse_stdout_line_tool_call_running() {
        let sdk = json!({
            "type":"tool_call","status":"running","name":"grep","call_id":"c1",
            "args":{"path":"/tmp"}
        });
        let line = json!({"type":"cursor_cloud.message","message":sdk}).to_string();
        let out = parse_cursor_cloud_stdout_line(&line);
        assert!(
            matches!(out[0], SdkMessageKind::ToolCallRunning { ref name, .. } if name == "grep")
        );
    }

    #[test]
    fn parse_stdout_line_tool_call_completed() {
        let sdk = json!({
            "type":"tool_call","status":"completed","name":"grep","call_id":"c1",
            "result":{"hits":3}
        });
        let line = json!({"type":"cursor_cloud.message","message":sdk}).to_string();
        let out = parse_cursor_cloud_stdout_line(&line);
        assert!(
            matches!(out[0], SdkMessageKind::ToolCallCompleted { ref name, is_error, .. } if name == "grep" && !is_error)
        );
    }

    #[test]
    fn parse_stdout_line_tool_call_error_marks_is_error() {
        let sdk = json!({
            "type":"tool_call","status":"error","name":"bash","call_id":"c2",
            "result":"permission denied"
        });
        let line = json!({"type":"cursor_cloud.message","message":sdk}).to_string();
        let out = parse_cursor_cloud_stdout_line(&line);
        assert!(matches!(
            out[0],
            SdkMessageKind::ToolCallCompleted { is_error: true, .. }
        ));
    }

    #[test]
    fn parse_stdout_line_tool_result_with_explicit_error_flag() {
        let sdk = json!({
            "type":"tool_result","name":"bash","call_id":"c3",
            "is_error":true,"result":{"k":"v"}
        });
        let line = json!({"type":"cursor_cloud.message","message":sdk}).to_string();
        let out = parse_cursor_cloud_stdout_line(&line);
        assert!(matches!(
            out[0],
            SdkMessageKind::ToolResult { is_error: true, .. }
        ));
    }

    #[test]
    fn parse_stdout_line_status_sdk_emits_status_text() {
        let sdk = json!({"type":"status","status":"running","message":"go"});
        let line = json!({"type":"cursor_cloud.message","message":sdk}).to_string();
        let out = parse_cursor_cloud_stdout_line(&line);
        let SdkMessageKind::System { text } = &out[0] else {
            panic!()
        };
        assert_eq!(text, "status: running - go");
    }

    #[test]
    fn parse_stdout_line_unknown_top_type_returns_system_text() {
        let line = "free-form stdout line";
        let out = parse_cursor_cloud_stdout_line(line);
        assert!(matches!(out[0], SdkMessageKind::System { ref text } if text == line));
    }

    #[test]
    fn parse_stdout_line_result_event() {
        let line = r#"{"type":"cursor_cloud.result","status":"finished","result":"done","model":"gpt-4","durationMs":1234}"#;
        let out = parse_cursor_cloud_stdout_line(line);
        let SdkMessageKind::System { text } = &out[0] else {
            panic!()
        };
        assert_eq!(text, "result status=finished");
    }

    #[test]
    fn message_event_round_trips_through_parse() {
        let sdk = serialize_sdk_assistant("hello");
        let e = message_event(sdk.clone());
        let line = event_line(&e);
        let out = parse_cursor_cloud_stdout_line(&line);
        assert!(matches!(out[0], SdkMessageKind::Assistant { ref text } if text == "hello"));
    }
}
