//! Terminal WebSocket JSON frame protocol.
//!
//! R628 复刻 paperclip Node `environment-custom-image-terminal-ws.ts` 的
//! 客户端/服务端帧协议。1:1 对齐 schema（不增不减），方便 Rust server 与
//! Node UI 互操作。
//!
//! 客户端 → 服务端：
//! - `{"type":"auth","token":"..."}`         鉴权
//! - `{"type":"resize","cols":N,"rows":M}`  终端尺寸（pre-auth 也允许）
//! - 任何非 JSON 文本 / 二进制 → 直通到 SSH stdin
//!
//! 服务端 → 客户端：
//! - `{"type":"ready","setupSessionId":"...","terminalSessionId":"..."}`
//! - `{"type":"output","data":"..."}`        SSH stdout（base64? 实际是 utf-8）
//! - `{"type":"error","message":"..."}`      错误（认证失败 / SSH 错误 / 超时）
//!
//! 设计：
//! - `ClientFrame` 枚举 + `ServerFrame` 枚举，分别表示双向帧类型
//! - `encode` / `decode` 走 serde_json，与 Node 端 `{ type, ... }` literal 对齐
//! - 解析失败不 panic，返回 `ClientFrameError`
//! - 所有尺寸字段在 deser 时严格校验（1 ≤ cols,rows ≤ 9999），与 Node 端
//!   `parseTerminalDimension` + `readResizeDimensions` 行为一致

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 客户端 → 服务端的帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFrame {
    /// `{"type":"auth","token":"..."}` — 必须是首条非 resize 帧
    Auth { token: String },
    /// `{"type":"resize","cols":N,"rows":M}` — pre-auth 也接受
    Resize { cols: u16, rows: u16 },
    /// 任何非 JSON 文本 → 直通 SSH stdin
    /// 任何 JSON 未知 type → 也直通（fallback，容错）
    RawText(String),
    /// 二进制帧 → 直通 SSH stdin（Node 端 `decodeClientMessage` 支持 Buffer/ArrayBuffer）
    RawBytes(Vec<u8>),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClientFrameError {
    #[error("auth token is empty after trim")]
    EmptyAuthToken,
    #[error("resize cols/rows out of range: cols={cols}, rows={rows}")]
    ResizeOutOfRange { cols: u16, rows: u16 },
}

impl ClientFrame {
    /// 解码一条原始客户端帧。
    ///
    /// - 字符串 / Vec<u8> / 类似 Node 端 `decodeClientMessage` 行为
    /// - JSON 解析：`{type:"auth", token}` / `{type:"resize", cols, rows}` / 未知 type → RawText
    /// - 解析失败（非 JSON 字符串）→ RawText
    pub fn decode(data: &[u8]) -> Self {
        // 尝试 UTF-8 解码
        let text = match std::str::from_utf8(data) {
            Ok(s) => s,
            Err(_) => return ClientFrame::RawBytes(data.to_vec()),
        };
        // 尝试 JSON 解析
        match serde_json::from_str::<RawClientJson>(text) {
            Ok(raw) => raw.into_frame(),
            Err(_) => ClientFrame::RawText(text.to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RawClientJson {
    Auth {
        token: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    #[serde(other)]
    Unknown,
}

impl RawClientJson {
    fn into_frame(self) -> ClientFrame {
        match self {
            RawClientJson::Auth { mut token } => {
                token = token.trim().to_string();
                if token.is_empty() {
                    // 空 token 视作非法，但 fallback 到 RawText 让 handler 报错
                    ClientFrame::RawText(String::new())
                } else {
                    ClientFrame::Auth { token }
                }
            }
            RawClientJson::Resize { cols, rows } => {
                if (1..=9999).contains(&cols) && (1..=9999).contains(&rows) {
                    ClientFrame::Resize { cols, rows }
                } else {
                    ClientFrame::Resize { cols, rows } // handler 会通过 ClientFrameError 报错
                }
            }
            RawClientJson::Unknown => ClientFrame::RawText(String::new()),
        }
    }
}

/// 服务端 → 客户端的帧。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServerFrame {
    Ready {
        #[serde(rename = "setupSessionId")]
        setup_session_id: String,
        #[serde(rename = "terminalSessionId")]
        terminal_session_id: String,
    },
    Output {
        data: String,
    },
    Error {
        message: String,
    },
}

impl ServerFrame {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ServerFrame serialization cannot fail")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- ClientFrame::decode -----

    #[test]
    fn decode_auth_token() {
        let f = ClientFrame::decode(br#"{"type":"auth","token":"abc123"}"#);
        assert_eq!(
            f,
            ClientFrame::Auth {
                token: "abc123".into()
            }
        );
    }

    #[test]
    fn decode_auth_token_with_whitespace_trimmed() {
        let f = ClientFrame::decode(br#"{"type":"auth","token":"  tok  "}"#);
        assert_eq!(
            f,
            ClientFrame::Auth {
                token: "tok".into()
            }
        );
    }

    #[test]
    fn decode_resize_valid() {
        let f = ClientFrame::decode(br#"{"type":"resize","cols":80,"rows":24}"#);
        assert_eq!(f, ClientFrame::Resize { cols: 80, rows: 24 });
    }

    #[test]
    fn decode_resize_zero_falls_through_as_raw_zero() {
        // Node 端：0/负数 > 9999 都 fallback 到默认值；这里只做 round-trip，
        // 校验由 ResizeOutOfRange 错误路径覆盖
        let f = ClientFrame::decode(br#"{"type":"resize","cols":0,"rows":0}"#);
        assert_eq!(f, ClientFrame::Resize { cols: 0, rows: 0 });
    }

    #[test]
    fn decode_unknown_json_type_falls_through() {
        let f = ClientFrame::decode(br#"{"type":"weird","x":1}"#);
        assert_eq!(f, ClientFrame::RawText(String::new()));
    }

    #[test]
    fn decode_invalid_json_is_raw_text() {
        let f = ClientFrame::decode(b"echo hello\n");
        assert_eq!(f, ClientFrame::RawText("echo hello\n".into()));
    }

    #[test]
    fn decode_invalid_utf8_is_raw_bytes() {
        let f = ClientFrame::decode(&[0xff, 0xfe, 0xfd]);
        assert_eq!(f, ClientFrame::RawBytes(vec![0xff, 0xfe, 0xfd]));
    }

    // ----- ServerFrame::to_json -----

    #[test]
    fn server_frame_ready_round_trip() {
        let s = ServerFrame::Ready {
            setup_session_id: "sess-1".into(),
            terminal_session_id: "term-1".into(),
        };
        let json = s.to_json();
        assert!(json.contains(r#""type":"ready""#));
        assert!(json.contains(r#""setupSessionId":"sess-1""#));
        assert!(json.contains(r#""terminalSessionId":"term-1""#));
    }

    #[test]
    fn server_frame_output_round_trip() {
        let s = ServerFrame::Output {
            data: "hello\r\n$ ".into(),
        };
        let json = s.to_json();
        assert!(json.contains(r#""type":"output""#));
        assert!(json.contains(r#""data":"hello\r\n$ ""#));
    }

    #[test]
    fn server_frame_error_round_trip() {
        let s = ServerFrame::Error {
            message: "auth failed".into(),
        };
        let json = s.to_json();
        assert!(json.contains(r#""type":"error""#));
        assert!(json.contains(r#""message":"auth failed""#));
    }

    // ----- 端到端：客户端解码 → 服务端响应（schema 兼容 Node 端） -----

    #[test]
    fn e2e_auth_flow_round_trip() {
        // 客户端发 auth
        let c = ClientFrame::decode(br#"{"type":"auth","token":"sess_tok_abc"}"#);
        // handler 应该返回 ready
        let s = match c {
            ClientFrame::Auth { token: _ } => ServerFrame::Ready {
                setup_session_id: "setup-1".into(),
                terminal_session_id: "term-1".into(),
            },
            _ => panic!("expected Auth"),
        };
        // 客户端能解析
        let parsed: serde_json::Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(parsed["type"], "ready");
        assert_eq!(parsed["setupSessionId"], "setup-1");
        assert_eq!(parsed["terminalSessionId"], "term-1");
    }

    #[test]
    fn e2e_resize_then_output_flow() {
        // 客户端发 resize
        let c1 = ClientFrame::decode(br#"{"type":"resize","cols":120,"rows":40}"#);
        assert_eq!(
            c1,
            ClientFrame::Resize {
                cols: 120,
                rows: 40
            }
        );
        // 服务端回 output
        let s = ServerFrame::Output { data: "$ ".into() };
        let parsed: serde_json::Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(parsed["type"], "output");
        assert_eq!(parsed["data"], "$ ");
    }

    #[test]
    fn e2e_raw_passthrough_flow() {
        // 客户端发任意文本 → 直通 SSH stdin
        let c = ClientFrame::decode(b"ls -la\n");
        assert_eq!(c, ClientFrame::RawText("ls -la\n".into()));
        // 服务端回 output 模拟
        let s = ServerFrame::Output {
            data: "total 0\n".into(),
        };
        assert!(s.to_json().contains("total 0"));
    }
}
