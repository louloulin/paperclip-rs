//! 把 fixture 变成一次真实 HTTP 请求（路径参数 / query 绑定 / 凭据装配）。
//!
//! 从 `lib.rs` 拆出（门 ⑩ 第 8 批）。对外符号由 crate 根 `pub use` 重导出，路径不变。

use anyhow::Result;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request};

use crate::bindings::Bindings;
use crate::requirements::actor_credential;
use crate::seed;
use crate::{ActorKind, Fixture};

// ---------------------------------------------------------------------------
// 请求构造
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RequestPlan {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(HeaderName, String)>,
    pub body: Option<Vec<u8>>,
    /// 这次回放做了哪些"翻译"，写进报告便于人复核。
    pub notes: Vec<String>,
}

impl RequestPlan {
    pub fn to_http(&self) -> Result<Request<Body>> {
        let mut builder = Request::builder()
            .method(self.method.as_str())
            .uri(&self.uri);
        for (k, v) in &self.headers {
            builder = builder.header(k, HeaderValue::from_str(v)?);
        }
        let body = match &self.body {
            Some(bytes) => Body::from(bytes.clone()),
            None => Body::empty(),
        };
        Ok(builder.body(body)?)
    }
}

/// 本仓的 session 中间件把 `X-Multica-Session` 解析成用户，注入 `AuthUser` extension；
/// 而 M1 dev-mode 的 `AuthUser` 提取器直接读 `X-Multica-User-Id`。两个都发，
/// 才能让同一个 `member` fixture 同时覆盖这两条链路。
const SESSION_HEADER: &str = "x-multica-session";
const DEV_USER_HEADER: &str = "x-multica-user-id";

/// 把 fixture 变成一次真实请求。`Err` ⇒ 这次回放是 `unevaluable`（附原因）。
///
/// 🔴 每个符号都按**这条 fixture 的分组**解析（`Fixture.source.test`，§213）：同一个
/// `$testIssueID` 在两条测试里指两行，分组键因此不是可省的默认参数。
// 本片（§216）把凭据面的拒绝理由改成取凭据表那一句，函数因此跨过 clippy 的 100 行线；
// 这是**允许清单**而不是删掉检查 —— `LUM-2482` 的 `supports()` 同例。
#[allow(clippy::too_many_lines)] // 见上一行：本仓把 `build_request` 拆出去会动 R7 基线
pub fn plan(fx: &Fixture, bindings: &Bindings) -> Result<RequestPlan, String> {
    let group = seed::group_of(fx);
    let mut notes = Vec::new();

    // ---- 路径：`{name}` 用 path_params 的取值填进去 --------------------------
    let mut path = fx.path.clone();
    for (name, raw) in &fx.path_params {
        let value = bindings.resolve(group, raw)?;
        let placeholder = format!("{{{name}}}");
        if !path.contains(&placeholder) {
            return Err(format!(
                "path_params.{name} declared but {placeholder} absent from path"
            ));
        }
        path = path.replace(&placeholder, &urlencode(&value));
    }
    if path.contains('{') {
        return Err(format!("path still has an unfilled placeholder: {path}"));
    }

    // ---- 查询串：只有显式声明的 key 才带上（不替上游补默认值）----------------
    if !fx.query.is_empty() {
        let mut pairs = Vec::with_capacity(fx.query.len());
        for (k, raw) in &fx.query {
            let v = bindings.resolve(group, raw)?;
            pairs.push(format!("{}={}", urlencode(k), urlencode(&v)));
        }
        path = format!("{path}?{}", pairs.join("&"));
        notes.push(format!("query: {}", pairs.join("&")));
    }

    // ---- header：fixture 自带的 + actor 身份的翻译 ---------------------------
    let mut headers: Vec<(HeaderName, String)> = Vec::new();
    for (k, raw) in &fx.headers {
        let v = bindings.resolve(group, raw)?;
        headers.push((
            HeaderName::from_bytes(k.as_bytes()).map_err(|e| e.to_string())?,
            v,
        ));
    }

    let mut identity = fx.actor.upstream_identity.clone();
    match fx.actor.kind {
        ActorKind::Anonymous => {}
        ActorKind::Member => {
            let session = bindings.resolve(
                group,
                identity
                    .remove("X-User-ID")
                    .as_deref()
                    .unwrap_or("$testUserID"),
            )?;
            headers.push((
                HeaderName::from_bytes(SESSION_HEADER.as_bytes()).unwrap(),
                session.clone(),
            ));
            headers.push((
                HeaderName::from_bytes(DEV_USER_HEADER.as_bytes()).unwrap(),
                session,
            ));
            notes.push("actor member: 以 X-Multica-Session + X-Multica-User-Id 注入身份".into());
        }
        // agent 身份在本仓没有解析面（`X-Agent-ID` 只是上游的 context 注入），
        // 令牌（`mul_` / `mcn_`）需要另一条签发面 —— 两者都仍然**不伪造**。
        // 没有任何层有签发面的档：`supports` 已在它那一侧答了否。
        ActorKind::Agent | ActorKind::Token | ActorKind::System => {
            let c = actor_credential(fx.actor.kind)?;
            return Err(if c.satisfied_by.is_empty() {
                c.detail.to_string()
            } else {
                format!(
                    "actor kind {:?} needs a real credential; this runner does not fabricate one",
                    fx.actor.kind
                )
            });
        }
        // §201.2 子根因 A 的正主：上游把 daemon 身份放在**请求 context** 里，而本仓
        // 把它放在 `Authorization: Bearer mdt_…` 里，解析面要查 `daemon_token` 表。
        // 所以「补一个 header」只有在 **database 层现场签发过一枚**时才成立
        // （`harness::database_router` → [`daemon_token::register`]）；stateless 层
        // 拿不到令牌，这里照旧说「不伪造」，而不是发一个注定 401 的假头。
        ActorKind::Daemon => {
            let token = bindings.daemon_token_for(group).ok_or_else(|| {
                "actor kind Daemon needs an mdt_ credential this tier did not mint \
                 (the database tier mints and registers one per replay)"
                    .to_string()
            })?;
            headers.push((
                HeaderName::from_static("authorization"),
                format!("Bearer {token}"),
            ));
            notes.push(
                "actor daemon: 以 Authorization: Bearer mdt_… 注入本次回放现场登记的身份".into(),
            );
        }
    }
    // 剩下的身份 header（X-Workspace-ID 等）按上游原样转发。
    for (k, raw) in identity {
        let v = bindings.resolve(group, &raw)?;
        headers.push((
            HeaderName::from_bytes(k.as_bytes()).map_err(|e| e.to_string())?,
            v,
        ));
    }

    let body = match &fx.body {
        Some(v) => Some(serde_json::to_vec(v).map_err(|e| e.to_string())?),
        None => None,
    };
    if body.is_some() && !headers.iter().any(|(k, _)| k.as_str() == "content-type") {
        headers.push((
            HeaderName::from_static("content-type"),
            "application/json".into(),
        ));
    }

    Ok(RequestPlan {
        method: fx.method.clone(),
        uri: path,
        headers,
        body,
        notes,
    })
}

/// 极简 percent-encode：只编码会破坏 URL 结构的字符（id 是 UUID，query 里可能出现
/// 空格/`&`）。不做完整 RFC 3986，避免引入额外依赖。
fn urlencode(raw: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'@' => {
                out.push(char::from(b));
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}
