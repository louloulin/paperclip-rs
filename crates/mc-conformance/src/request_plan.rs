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

    // 身份头里**借来的行 id** 先折成该分组的符号（`bindings::IDENTITY_ROW_HEADERS`）：
    // 抽取器只在 path / query / body 上做这件事，身份头是原样保留的。
    let mut identity = crate::bindings::normalize_identity(&fx.actor.upstream_identity);
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
        // 令牌（`mk_pat_`）与 daemon 是**同一个形态**：上游确实把它放上了线
        // （`newRenewRequest` 的 `Header.Set("Authorization", "Bearer "+raw)`），但明文是
        // `auth.GeneratePATToken()` 的随机值 —— 走查解不出、整个 header 被丢掉，于是契约
        // 里只剩 `actor.kind = token` 与 `$testPAT<State>` 这个符号（§233.4）。
        // 所以「补一个 header」也只有在 **database 层现场签发了那一档** 时才成立
        // （`harness::database_router` → [`crate::pat_token::register`]）；stateless 层拿不到明文，
        // `resolve` 直接报 `unbound symbol`，照旧是 unevaluable，而不是发一个注定 401 的假头。
        ActorKind::Token => {
            let raw = identity
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .map(|(_, v)| v.clone())
                .ok_or_else(|| {
                    "actor kind Token needs an `Authorization: Bearer mk_pat_…` binding, and this \
                     fixture declares none"
                        .to_string()
                })?;
            identity.retain(|k, _| !k.eq_ignore_ascii_case("authorization"));
            let secret = bindings.resolve(group, &raw)?;
            headers.push((
                HeaderName::from_static("authorization"),
                format!("Bearer {secret}"),
            ));
            notes.push(
                "actor token: 以 Authorization: Bearer mk_pat_… 注入本次回放现场签发的身份".into(),
            );
        }
        // agent：本仓的身份面是「AuthUser 只认 `X-Multica-User-Id`」（M1 dev-mode 契约，
        // `routes/auth_user.rs`）+「只有 `/api/chat/**` 读 `X-Actor-Source` / `X-Task-ID`」
        // （`routes/chat/task/history.rs:144-150`）。所以这里只做**一件**翻译：
        // `X-User-ID` → 本仓的会员会话（与 `Member` 档逐字同一句）；其余身份头
        // （`X-Actor-Source` / `X-Task-ID` / `X-Agent-ID` / `X-Workspace-ID`）按上游原样转发。
        //
        // 🔴 不伪造 `X-Agent-ID`：那张脸在本仓没有解析面（`routes/agents.rs:42` 逐字登记
        // 「agent actor … 本片不解析」），所以「上游发了什么就转发什么」是本档唯一忠实的
        // 做法 —— 伪造一个真 agent id 会把 `…RejectsForgedAgentIDHeader` 的**前提**从
        // 「伪造」改成「自证」。同理，回放器也不会替上游编一个 `X-Multica-User-Id`。
        //
        // `X-Task-ID` 已由 `Bindings::normalize_identity` 折到本分组种下的那一行：
        // 本仓的 `/api/chat/**` 真的读它，而它必须指到一行真的 `agent_task_queue`。
        ActorKind::Agent => {
            if let Some(raw) = identity.remove("X-User-ID") {
                let session = bindings.resolve(group, &raw)?;
                headers.push((
                    HeaderName::from_bytes(SESSION_HEADER.as_bytes()).unwrap(),
                    session.clone(),
                ));
                headers.push((
                    HeaderName::from_bytes(DEV_USER_HEADER.as_bytes()).unwrap(),
                    session,
                ));
                notes.push(
                    "actor agent: 以 X-Multica-Session + X-Multica-User-Id 注入上游 \
                     X-User-ID 那位主体；X-Actor-Source / X-Task-ID / X-Agent-ID 原样转发"
                        .into(),
                );
            }
        }
        // 没有任何层有签发面的档：`supports` 已在它那一侧答了否。
        ActorKind::System => {
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
