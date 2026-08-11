//! OpenClaw Gateway wire frame codec — 对齐 Node
//! `packages/adapters/openclaw-gateway/src/server/execute.ts` 顶部
//! `GatewayRequestFrame` / `GatewayResponseFrame` / `GatewayEventFrame` 类型。
//!
//! WS 双向消息都是这三类帧的 JSON 序列化/反序列化。
//! 本模块专注**纯数据**（serde-compatible），不持有任何 IO 状态。

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::constants::frame_types;

/// Gateway 请求帧（client → server）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayRequestFrame {
    #[serde(rename = "type")]
    pub frame_type: String, // always "req"
    pub id: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub params: Option<Value>,
}

/// Gateway 响应帧（server → client），成功或失败两种。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayResponseFrame {
    #[serde(rename = "type")]
    pub frame_type: String, // always "res"
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<GatewayResponseErrorBody>,
}

/// 响应错误 body（code + message）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayResponseErrorBody {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub code: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<Value>,
}

/// Gateway 事件帧（server → client，state change notification）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayEventFrame {
    #[serde(rename = "type")]
    pub frame_type: String, // always "event"
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub seq: Option<u64>,
}

/// 顶层统一枚举（解析入口）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayFrame {
    #[serde(rename = "req")]
    Request(GatewayRequestFrame),
    #[serde(rename = "res")]
    Response(GatewayResponseFrame),
    #[serde(rename = "event")]
    Event(GatewayEventFrame),
}

/// Frame 类型守卫（无需解析整个 JSON 即可判断类型）。
pub fn frame_type_of(value: &Value) -> Option<&str> {
    value.get("type").and_then(|v| v.as_str())
}

/// `id` 提取（req / res 都必有；event 无 id）。
pub fn frame_id_of(value: &Value) -> Option<&str> {
    value.get("id").and_then(|v| v.as_str())
}

// ─── Constructors ─────────────────────────────────────────────────────

impl GatewayRequestFrame {
    pub fn new(method: impl Into<String>, id: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            frame_type: frame_types::REQ.to_owned(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

impl GatewayResponseFrame {
    pub fn ok(id: impl Into<String>, payload: Option<Value>) -> Self {
        Self {
            frame_type: frame_types::RES.to_owned(),
            id: id.into(),
            ok: true,
            payload,
            error: None,
        }
    }
    pub fn error(id: impl Into<String>, code: Option<Value>, message: Option<Value>) -> Self {
        Self {
            frame_type: frame_types::RES.to_owned(),
            id: id.into(),
            ok: false,
            payload: None,
            error: Some(GatewayResponseErrorBody { code, message }),
        }
    }
}

impl GatewayEventFrame {
    pub fn new(event: impl Into<String>, payload: Option<Value>) -> Self {
        Self {
            frame_type: frame_types::EVENT.to_owned(),
            event: event.into(),
            payload,
            seq: None,
        }
    }
    pub fn with_seq(mut self, seq: u64) -> Self {
        self.seq = Some(seq);
        self
    }
}

// ─── JSON helpers ─────────────────────────────────────────────────────

/// `frame_to_value` — 把任意 frame 序列化为 generic `serde_json::Value`。
/// 适合要传给通用 onLog/sink 的场景。
pub fn frame_to_value(frame: &GatewayFrame) -> Value {
    Value::Object(match frame {
        GatewayFrame::Request(r) => json_to_map(serde_json::to_value(r).unwrap_or(Value::Null)),
        GatewayFrame::Response(r) => json_to_map(serde_json::to_value(r).unwrap_or(Value::Null)),
        GatewayFrame::Event(e) => json_to_map(serde_json::to_value(e).unwrap_or(Value::Null)),
    })
}

/// `parse_any_frame` — 从 `serde_json::Value` 解析为顶层 enum。
pub fn parse_any_frame(value: &Value) -> Result<GatewayFrame, FrameParseError> {
    let Some(kind) = value.get("type").and_then(|v| v.as_str()) else {
        return Err(FrameParseError::MissingType);
    };
    match kind {
        "req" => serde_json::from_value::<GatewayRequestFrame>(value.clone())
            .map(GatewayFrame::Request)
            .map_err(|e| FrameParseError::Json(e.to_string())),
        "res" => serde_json::from_value::<GatewayResponseFrame>(value.clone())
            .map(GatewayFrame::Response)
            .map_err(|e| FrameParseError::Json(e.to_string())),
        "event" => serde_json::from_value::<GatewayEventFrame>(value.clone())
            .map(GatewayFrame::Event)
            .map_err(|e| FrameParseError::Json(e.to_string())),
        other => Err(FrameParseError::UnknownType(other.to_owned())),
    }
}

/// `parse_request_frame` —— 解析请求帧（严格类型）。
pub fn parse_request_frame(value: &Value) -> Result<GatewayRequestFrame, FrameParseError> {
    serde_json::from_value(value.clone()).map_err(|e| FrameParseError::Json(e.to_string()))
}

/// `parse_response_frame` —— 解析响应帧。
pub fn parse_response_frame(value: &Value) -> Result<GatewayResponseFrame, FrameParseError> {
    serde_json::from_value(value.clone()).map_err(|e| FrameParseError::Json(e.to_string()))
}

/// `parse_event_frame` —— 解析事件帧。
pub fn parse_event_frame(value: &Value) -> Result<GatewayEventFrame, FrameParseError> {
    serde_json::from_value(value.clone()).map_err(|e| FrameParseError::Json(e.to_string()))
}

/// 把响应 Frame 序列化为 JSON 行（带 `\\n` 终止）。
pub fn response_to_line(frame: &GatewayResponseFrame) -> String {
    format!(
        "{}\n",
        serde_json::to_string(frame).unwrap_or_else(|_| "{}".to_owned())
    )
}

/// 把请求 Frame 序列化为 JSON 行（带 `\\n` 终止）。
pub fn request_to_line(frame: &GatewayRequestFrame) -> String {
    format!(
        "{}\n",
        serde_json::to_string(frame).unwrap_or_else(|_| "{}".to_owned())
    )
}

/// 把事件 Frame 序列化为 JSON 行（带 `\\n` 终止）。
pub fn event_to_line(frame: &GatewayEventFrame) -> String {
    format!(
        "{}\n",
        serde_json::to_string(frame).unwrap_or_else(|_| "{}".to_owned())
    )
}

fn json_to_map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Frame 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameParseError {
    MissingType,
    UnknownType(String),
    Json(String),
}

impl std::fmt::Display for FrameParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameParseError::MissingType => write!(f, "missing frame 'type' field"),
            FrameParseError::UnknownType(t) => write!(f, "unknown frame type: {t}"),
            FrameParseError::Json(e) => write!(f, "json parse error: {e}"),
        }
    }
}

impl std::error::Error for FrameParseError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_frame_serializes_with_type_field() {
        let f = GatewayRequestFrame::new("device.connect", "r-1", Some(json!({"a": 1})));
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["type"], "req");
        assert_eq!(v["id"], "r-1");
        assert_eq!(v["method"], "device.connect");
        assert_eq!(v["params"]["a"], 1);
    }

    #[test]
    fn request_frame_omits_none_params() {
        let f = GatewayRequestFrame::new("ping", "r-2", None);
        let v = serde_json::to_value(&f).unwrap();
        assert!(v.get("params").is_none());
    }

    #[test]
    fn response_ok_frame_serializes_payload() {
        let f = GatewayResponseFrame::ok("r-1", Some(json!({"ok": true})));
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["type"], "res");
        assert_eq!(v["ok"], true);
        assert_eq!(v["payload"]["ok"], true);
        assert!(v.get("error").is_none());
    }

    #[test]
    fn response_error_frame_serializes_code_and_message() {
        let f = GatewayResponseFrame::error(
            "r-1",
            Some(json!("UNAUTHORIZED")),
            Some(json!("invalid api key")),
        );
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "UNAUTHORIZED");
        assert_eq!(v["error"]["message"], "invalid api key");
        assert!(v.get("payload").is_none());
    }

    #[test]
    fn event_frame_serializes_with_seq() {
        let f = GatewayEventFrame::new("state.changed", Some(json!({"k": "v"}))).with_seq(7);
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["type"], "event");
        assert_eq!(v["event"], "state.changed");
        assert_eq!(v["seq"], 7);
    }

    #[test]
    fn event_frame_omits_seq_when_none() {
        let f = GatewayEventFrame::new("state.changed", None);
        let v = serde_json::to_value(&f).unwrap();
        assert!(v.get("seq").is_none());
        assert!(v.get("payload").is_none());
    }

    #[test]
    fn parse_any_frame_dispatches_request() {
        let v = json!({"type":"req","id":"r-1","method":"foo","params":{"x":1}});
        let f = parse_any_frame(&v).unwrap();
        match f {
            GatewayFrame::Request(r) => {
                assert_eq!(r.id, "r-1");
                assert_eq!(r.method, "foo");
            }
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn parse_any_frame_dispatches_response() {
        let v = json!({"type":"res","id":"r-1","ok":true,"payload":{"a":1}});
        let f = parse_any_frame(&v).unwrap();
        match f {
            GatewayFrame::Response(r) => {
                assert!(r.ok);
                assert_eq!(r.payload.unwrap()["a"], 1);
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn parse_any_frame_dispatches_event() {
        let v = json!({"type":"event","event":"e","seq":3});
        let f = parse_any_frame(&v).unwrap();
        match f {
            GatewayFrame::Event(e) => {
                assert_eq!(e.event, "e");
                assert_eq!(e.seq, Some(3));
            }
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn parse_any_frame_missing_type_errors() {
        let err = parse_any_frame(&json!({"id":"r-1"})).unwrap_err();
        assert!(matches!(err, FrameParseError::MissingType));
    }

    #[test]
    fn parse_any_frame_unknown_type_errors() {
        let err = parse_any_frame(&json!({"type":"bogus"})).unwrap_err();
        assert!(matches!(err, FrameParseError::UnknownType(t) if t == "bogus"));
    }

    #[test]
    fn parse_any_frame_invalid_json_shape_errors() {
        let err = parse_any_frame(&json!({"type":"req","id":"r-1"})).unwrap_err();
        assert!(matches!(err, FrameParseError::Json(_)));
    }

    #[test]
    fn frame_type_of_returns_string_value() {
        assert_eq!(frame_type_of(&json!({"type":"req"})).unwrap(), "req");
        assert_eq!(frame_type_of(&json!({"type":"event"})).unwrap(), "event");
        assert!(frame_type_of(&json!({})).is_none());
        assert!(frame_type_of(&json!({"type":1})).is_none());
    }

    #[test]
    fn frame_id_of_extracts_request_and_response_ids() {
        assert_eq!(frame_id_of(&json!({"id":"r-1"})).unwrap(), "r-1");
        assert!(frame_id_of(&json!({})).is_none());
        assert!(frame_id_of(&json!({"id":42})).is_none());
    }

    #[test]
    fn request_to_line_includes_trailing_newline() {
        let f = GatewayRequestFrame::new("ping", "r-3", None);
        let line = request_to_line(&f);
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["type"], "req");
        assert_eq!(parsed["method"], "ping");
    }

    #[test]
    fn response_to_line_includes_trailing_newline() {
        let f = GatewayResponseFrame::ok("r-1", Some(json!({"x": 1})));
        let line = response_to_line(&f);
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["type"], "res");
    }

    #[test]
    fn event_to_line_includes_trailing_newline() {
        let f = GatewayEventFrame::new("hello", None);
        let line = event_to_line(&f);
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["type"], "event");
    }

    #[test]
    fn frame_to_value_round_trip_for_request() {
        let original = GatewayRequestFrame::new("foo", "r-1", Some(json!({"k":"v"})));
        let v = frame_to_value(&GatewayFrame::Request(original.clone()));
        let parsed = parse_any_frame(&v).unwrap();
        match parsed {
            GatewayFrame::Request(r) => assert_eq!(r, original),
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn frame_to_value_round_trip_for_response() {
        let original = GatewayResponseFrame::error("r-1", Some(json!("E")), Some(json!("msg")));
        let v = frame_to_value(&GatewayFrame::Response(original.clone()));
        let parsed = parse_any_frame(&v).unwrap();
        match parsed {
            GatewayFrame::Response(r) => {
                assert_eq!(r.id, original.id);
                assert_eq!(r.ok, original.ok);
                assert!(r.error.is_some());
            }
            _ => panic!("expected response"),
        }
    }

    #[test]
    fn frame_to_value_round_trip_for_event() {
        let original = GatewayEventFrame::new("e", Some(json!({"x":1}))).with_seq(42);
        let v = frame_to_value(&GatewayFrame::Event(original.clone()));
        let parsed = parse_any_frame(&v).unwrap();
        match parsed {
            GatewayFrame::Event(e) => assert_eq!(e, original),
            _ => panic!("expected event"),
        }
    }

    #[test]
    fn parse_request_frame_works_on_valid_input() {
        let v = json!({"type":"req","id":"r-1","method":"foo"});
        let f = parse_request_frame(&v).unwrap();
        assert_eq!(f.id, "r-1");
        assert_eq!(f.method, "foo");
    }

    #[test]
    fn parse_response_frame_errors_on_non_response() {
        let v = json!({"type":"req","id":"r-1","method":"foo"});
        let err = parse_response_frame(&v).unwrap_err();
        assert!(matches!(err, FrameParseError::Json(_)));
    }

    #[test]
    fn frame_parse_error_displays_meaningful_messages() {
        assert_eq!(
            format!("{}", FrameParseError::MissingType),
            "missing frame 'type' field"
        );
        assert_eq!(
            format!("{}", FrameParseError::UnknownType("bogus".to_owned())),
            "unknown frame type: bogus"
        );
        let j = FrameParseError::Json("expected colon".to_owned());
        assert!(format!("{j}").contains("expected colon"));
    }
}
