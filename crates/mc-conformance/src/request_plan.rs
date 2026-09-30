//! 把 fixture 变成一次真实 HTTP 请求（路径参数 / query 绑定 / 凭据装配）。
//!
//! 从 `lib.rs` 拆出（门 ⑩ 第 8 批）。对外符号由 crate 根 `pub use` 重导出，路径不变。

use anyhow::Result;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request};

use crate::bindings::Bindings;
use crate::requirements::actor_credential;
use crate::seed;
use crate::upstream_facts;
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
/// `agent` 档身份的任务令牌头（上游 `taskActorReq`；本仓解析面见
/// `routes/chat/task/history.rs::chat_history_scope`）。
const TASK_ID_HEADER: &str = "X-Task-ID";

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
        // 🔴 `§297` 实测回退：这里曾有一段「`browser_session_cookie` ⇒ 注入种子用户的真
        // 会话」的注入（与 `Agent` 分支同形）。它让那条 fixture 进了判定，**然后落到
        // `unmounted`**：`/users/me` 在本仓根本没挂。⇒ 注入本身无害也无用，真正的缺口在
        // 路由面（而 ⑦ `known_gap = 0` ⇒ 本仓不许新增注册路由）。
        // 保留这条注释是承重：没有它，下一个人会以为「agent 那族能注入，cookie 这族只是
        // 忘了写」—— 而两者的差别在**路由是否存在**，不在凭据。
        ActorKind::Anonymous => {}
        ActorKind::Member => {
            let session = bindings.resolve(
                group,
                identity
                    .remove("X-User-ID")
                    .as_deref()
                    .unwrap_or("$testUserID"),
            )?;
            push_session_identity(&mut headers, session);
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
        // `§297`：agent 身份在本仓**有**解析面 —— 任务作用域令牌
        // （`X-Actor-Source: task_token` + `X-Task-ID`，上游 `taskActorReq`），
        // 解析面在 `routes/chat/task/history.rs::chat_history_scope`，它会去查
        // `agent_task_queue`。所以这一档要做的不是「伪造一枚凭据」，而是
        // （a）让 `X-Task-ID` 指向**本仓种出来的那一行**（`crate::task_token`），
        // （b）把上游逐字写下的其余身份头（`X-Actor-Source` / `X-Agent-ID` /
        // `X-Workspace-ID`）原样转发 —— 那道 actor 闸必须是**被 handler 判出来的**，
        // 装置替它答了就等于把 `TestGetChatHistory_RejectsForgedTaskID` 的 403 洗成绿。
        //
        // `X-User-ID` 走与 `member` 同一套装配：写面（`POST /api/issues` 等）在本仓
        // 只认 session 成员身份。🔴 这是本仓与上游的一处**真实落差**，登记在
        // `crate::task_token` 模块头：本仓没有 `resolveActor` 那一面，
        // 所以这一族 fixture 断言的仍然只是状态码。
        ActorKind::Agent => {
            // `X-Task-ID` 在 fixture 里是**逐字字面量**（借来的行 id），而装置按分组
            // 各建一行 —— 所以这里做一次绑定：点名的那枚令牌 ⇒ 该分组真种出来的那一行。
            // 拿不到绑定就**保留字面量**：那时得到的是 handler 自己判出来的 404
            // （装置不编，也不静默换一个 id）。
            if let Some(task) = bindings.task_token_task_for(group) {
                if let Some(slot) = identity.get_mut(TASK_ID_HEADER) {
                    *slot = task.to_string();
                    notes.push(format!(
                        "actor agent: X-Task-ID 的字面量绑到本分组的任务令牌行 {task} \
                         (crate::task_token)"
                    ));
                }
            }
            if let Some(raw) = identity.remove("X-User-ID") {
                let session = bindings.resolve(group, &raw)?;
                push_session_identity(&mut headers, session);
            }
            notes.push(
                "actor agent: 任务作用域令牌原样转发（X-Task-ID 指向本仓种出的那一行任务，\
                 见 crate::task_token）；写面另按 session 成员身份注入"
                    .into(),
            );
        }
        // system 是内部专用档，本仓没有任何解析面。
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
            let token = bindings
                .daemon_token_for_fixture(group, &fx.source)
                .ok_or_else(|| {
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
            if upstream_facts::is_foreign_daemon_request(&fx.source.test, fx.source.line) {
                notes.push(
                    "上游本请求用的是一个**不拥有该资源**的 workspace 身份（见 \
                     upstream_facts::FOREIGN_DAEMON_REQUESTS）；此处用 outsider 令牌"
                        .into(),
                );
            }
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

/// 把一个用户 id 装成**两个**身份头（本仓两条链路都要：见上面那个常量对的注释）。
///
/// 抽成函数是因为它现在有**两个**调用点（`member` 与 `§297` 的 `agent`），而两个常量
/// 都是编译期已知的合法 header 名 ⇒ 用 [`HeaderName::from_static`] 而不是
/// `from_bytes(..).unwrap()`：前者把「这个名字合法吗」变成编译期事实。
fn push_session_identity(headers: &mut Vec<(HeaderName, String)>, session: String) {
    headers.push((HeaderName::from_static(SESSION_HEADER), session.clone()));
    headers.push((HeaderName::from_static(DEV_USER_HEADER), session));
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
