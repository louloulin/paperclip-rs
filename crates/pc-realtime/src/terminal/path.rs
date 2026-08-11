//! Terminal WebSocket URL path parser.
//!
//! R628 复刻 paperclip Node `parseTerminalPath` 行为：
//!   /api/environment-custom-image-setup-sessions/{setupSessionId}/terminal/ws
//!
//! 设计：
//! - 纯函数，无 IO，零 unsafe
//! - `setupSessionId` URL 解码失败返回 Err（与 Node 端 decodeURIComponent 行为一致）
//! - 空 setupSessionId 视为非法
//! - 路径前后允许任意字符串（不强制 leading `/`），与 Node regex 行为一致

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TerminalPathError {
    #[error("path does not match terminal ws pattern: {0}")]
    NoMatch(String),
    #[error("setupSessionId is empty after url-decode")]
    EmptySetupSessionId,
    #[error("setupSessionId url-decode failed: {0}")]
    UrlDecodeError(String),
}

const PATTERN: &str = "/api/environment-custom-image-setup-sessions/";

/// 解析 WS upgrade URL，提取 `setupSessionId`。
///
/// 接受 `pathname`（如 `/api/environment-custom-image-setup-sessions/abc/terminal/ws`），
/// 不接受 query string（调用方自行 strip）。
pub fn parse_terminal_path(pathname: &str) -> Result<String, TerminalPathError> {
    let after = match pathname.strip_prefix(PATTERN) {
        Some(s) => s,
        None => return Err(TerminalPathError::NoMatch(pathname.into())),
    };
    // 期望 `{id}/terminal/ws`
    let (raw_id, rest) = after
        .split_once('/')
        .ok_or_else(|| TerminalPathError::NoMatch(pathname.into()))?;
    if rest != "terminal/ws" {
        return Err(TerminalPathError::NoMatch(pathname.into()));
    }
    let id = percent_decode(raw_id)
        .map_err(|e| TerminalPathError::UrlDecodeError(e.to_string()))?;
    if id.is_empty() {
        return Err(TerminalPathError::EmptySetupSessionId);
    }
    Ok(id)
}

/// 极简 percent-decode：处理 `%XX` → byte。错误返回 InvalidSequence。
/// 不处理 `+`（path 中不出现），不处理 UTF-8 完整性（保留为原始 bytes 字符串，
/// 与 Node 端 `decodeURIComponent` 行为有差异但 setupSessionId 通常是 UUID / ulid 等
/// ASCII-safe 字符）。
fn percent_decode(s: &str) -> Result<String, &'static str> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_digit(bytes[i + 1])?;
                let lo = hex_digit(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "invalid utf-8")
}

fn hex_digit(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("invalid hex digit"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_path() {
        let id = parse_terminal_path(
            "/api/environment-custom-image-setup-sessions/setup-123/terminal/ws",
        )
        .unwrap();
        assert_eq!(id, "setup-123");
    }

    #[test]
    fn parses_uuid_id() {
        let id = parse_terminal_path(
            "/api/environment-custom-image-setup-sessions/550e8400-e29b-41d4-a716-446655440000/terminal/ws",
        )
        .unwrap();
        assert_eq!(id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn percent_decoded_id() {
        let id = parse_terminal_path(
            "/api/environment-custom-image-setup-sessions/abc%20def/terminal/ws",
        )
        .unwrap();
        assert_eq!(id, "abc def");
    }

    #[test]
    fn rejects_missing_prefix() {
        let err = parse_terminal_path("/api/wrong/foo/terminal/ws").unwrap_err();
        assert!(matches!(err, TerminalPathError::NoMatch(_)));
    }

    #[test]
    fn rejects_wrong_suffix() {
        let err = parse_terminal_path(
            "/api/environment-custom-image-setup-sessions/abc/terminal/ssh",
        )
        .unwrap_err();
        assert!(matches!(err, TerminalPathError::NoMatch(_)));
    }

    #[test]
    fn rejects_no_terminator() {
        let err =
            parse_terminal_path("/api/environment-custom-image-setup-sessions/abc").unwrap_err();
        assert!(matches!(err, TerminalPathError::NoMatch(_)));
    }

    #[test]
    fn rejects_empty_id() {
        // `//terminal/ws` → empty id after split_once
        let err =
            parse_terminal_path("/api/environment-custom-image-setup-sessions//terminal/ws")
                .unwrap_err();
        assert!(matches!(err, TerminalPathError::NoMatch(_) | TerminalPathError::EmptySetupSessionId));
    }

    #[test]
    fn rejects_invalid_percent_encoding() {
        let err = parse_terminal_path(
            "/api/environment-custom-image-setup-sessions/abc%ZZ/terminal/ws",
        )
        .unwrap_err();
        assert!(matches!(err, TerminalPathError::UrlDecodeError(_)));
    }
}
