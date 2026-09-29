use super::*;

/// Go `url.QueryEscape`：非保留字符里空格变 `+`（表单编码），其余不合法字节 `%XX`。
///
/// 只覆盖 `A-Za-z0-9-_.~` 放行 —— 与 Go 的 `shouldEscape(encodeQueryComponent)` 一致
/// （`+` 本身也要转成 `%2B`，否则会被对端读成空格）。
pub(crate) fn query_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push('+'),
            other => {
                // `write!` 到 `String` 不会失败（`fmt::Write` 的 `String` 实现是 Infallible）
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Go `url.PathEscape`（`encodePathSegment`）：`/ ; , ?` 要转义，`$ & + : = @` 放行。
pub(crate) fn path_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'$'
            | b'&'
            | b'+'
            | b':'
            | b'='
            | b'@' => out.push(*byte as char),
            other => {
                // `write!` 到 `String` 不会失败（`fmt::Write` 的 `String` 实现是 Infallible）
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 502（上游 SearchSkills 的扁平错误体）
// ---------------------------------------------------------------------------

/// 上游 `writeJSON(w, http.StatusBadGateway, map[string]string{"code":…, "error":…})`。
/// ⚠️ 这条**故意**不是本仓的 `{"error":{…}}` 形状：契约就是扁平的，故返回裸 `Response`。
pub(crate) fn upstream_unavailable(message: &str) -> Response {
    (
        axum::http::StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({
            "code": "upstream_unavailable",
            "error": message,
        })),
    )
        .into_response()
}
