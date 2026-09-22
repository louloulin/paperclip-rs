//! `mc-conformance` —— 上游 golden fixture 的本地回放器。
//!
//! # 这个 crate 解决什么问题
//!
//! 本仓是上游 `multica`（Go）的 Rust 重写。**"同样的输入产生上游等价的输出"**
//! 如果只靠人读代码，就是不可证伪的声明。本 crate 把上游测试里**可判定的**
//! 请求/响应断言抽成语言无关的 golden fixture（由
//! `scripts/extract_upstream_fixtures.py` 生成，见 `contracts/golden/`），
//! 再用 `tower::ServiceExt::oneshot` 打到本仓真实的 axum router 上重放：
//!
//! ```text
//! upstream Go 测试  --extract-->  contracts/golden/**.json  --replay-->  this repo's router
//! ```
//!
//! 于是「契约等价」变成一个可以按 fixture 数出来的比例（`docs/27-W0-GOLDEN-FIXTURES.md`）。
//!
//! # 判定口径（不猜、不掩盖）
//!
//! 每个 fixture **恰好**产生一行结果，落进下面五类之一：
//!
//! | outcome | 含义 |
//! |---|---|
//! | `pass` | 状态码一致，且 `expect.json_subset` 是响应 body 的子集 |
//! | `mismatch` | 打到了已实现的路由，但状态码/字段与上游断言不符（**这才是缺陷**）|
//! | `unmounted` | 本仓没有这条路由（404 空 body / 405）—— 属"未实现"，单独计数 |
//! | `placeholder` | 路由存在但是 M0 占位实现（501 / `{"code":"not_implemented"}`）|
//! | `unevaluable` | 本仓无法构造这次请求（如 `Authorization` 令牌型 actor 没有绑定）|
//!
//! `unmounted` / `placeholder` / `unevaluable` **不算失败**，但一定出现在报告里 ——
//! 分子分母都从报告行里数出来，所以"等价率"不可能靠遮掉难看的行来变好看。
//!
//! # 两层回放（tier）
//!
//! - **stateless**：`Db` 用 `connect_lazy` 指向一个不可达端口，不建任何连接。
//!   匿名断言（401 一类）在这一层就能判定，且完全确定 —— CI 里跑的就是这层，
//!   所以 `report.json` 可以 `--check` 字节比对。
//! - **database**：真库 + 迁移 + 一个种子用户/workspace owner 成员，
//!   `X-Multica-Session` 用种子用户 id 注入。`member` actor 的 fixture 需要这层。
//!
//! 同一 fixture 可能两层都跑，报告里两层的 outcome 都留着；汇总时取**更强**的那层
//! （`pass` > `mismatch` > `unmounted`/`placeholder`/`unevaluable`），并在
//! `tier` 字段里写明这个结论来自哪一层。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod harness;

use anyhow::{anyhow, Context, Result};
use axum::body::{to_bytes, Body};
use axum::http::{HeaderName, HeaderValue, Request};
use axum::Router;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;
use uuid::Uuid;

/// fixture 文件格式版本；与 `scripts/extract_upstream_fixtures.py::SCHEMA_VERSION` 对齐。
pub const SCHEMA_VERSION: u32 = 1;

/// stateless 层的身份：固定值，保证报告可字节复现（这一层没有数据库，
/// 身份只用来走 401 判定与把 UUID 填进路径段，不需要真实存在）。
pub const STATELESS_USER_ID: u128 = 1;
/// 见 [`STATELESS_USER_ID`]。
pub const STATELESS_WORKSPACE_ID: u128 = 2;

// ---------------------------------------------------------------------------
// fixture 模型
// ---------------------------------------------------------------------------

/// actor 类型：上游是怎么把自己"介绍"给 handler 的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// 没带任何身份 header。
    Anonymous,
    /// 带 `X-User-ID`。
    Member,
    /// 带 `X-Agent-ID` / `X-Task-ID`。
    Agent,
    /// 带 `Authorization`（个人访问令牌）。
    Token,
    /// 系统内部调用。
    System,
}

impl ActorKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Member => "member",
            Self::Agent => "agent",
            Self::Token => "token",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    #[serde(default)]
    pub upstream_identity: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Expect {
    pub status: u16,
    #[serde(default)]
    pub json_subset: serde_json::Value,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub file: String,
    pub line: u64,
    pub test: String,
    pub site: String,
    #[serde(default)]
    pub via: String,
    #[serde(default)]
    pub commit: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extraction {
    #[serde(default)]
    pub notes: Vec<String>,
    /// `$symbol -> 语义`，由抽取器声明；回放器只接受自己会绑定的语义。
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    pub schema_version: u32,
    pub id: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub path_params: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub actor: Actor,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    pub expect: Expect,
    pub source: Source,
    #[serde(default)]
    pub extraction: Extraction,
}

impl Fixture {
    /// fixture 自己的完整性检查 —— 坏 fixture 必须在加载时报错，不能悄悄跳过。
    pub fn verify(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(anyhow!(
                "{}: schema_version {} != {}",
                self.id,
                self.schema_version,
                SCHEMA_VERSION
            ));
        }
        if self.id.trim().is_empty() || self.method.trim().is_empty() || self.path.trim().is_empty()
        {
            return Err(anyhow!("fixture has empty id/method/path"));
        }
        if !self.path.starts_with('/') {
            return Err(anyhow!("{}: path must be absolute: {}", self.id, self.path));
        }
        // 每个 `{name}` 都必须有 path_params 提供取值，否则请求会带着字面量 `{name}` 打出去。
        for seg in self.path.split('/') {
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                if !self.path_params.contains_key(name) {
                    return Err(anyhow!(
                        "{}: path uses {{{name}}} but path_params has no entry",
                        self.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// fixture 的领域（`contracts/golden/<domain>/<case>.json` 的 `<domain>`）。
    #[must_use]
    pub fn domain(&self) -> String {
        self.id.split('/').next().unwrap_or("_").to_string()
    }
}

/// 读单个 fixture 文件。
pub fn load_file(path: &Path) -> Result<Fixture> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let fx: Fixture =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    fx.verify()
        .with_context(|| format!("verify {}", path.display()))?;
    Ok(fx)
}

/// 读一个 golden 目录下所有 `*/**.json`，按 id 排序（顺序稳定 ⇒ 报告可复现）。
pub fn load_dir(dir: &Path) -> Result<Vec<Fixture>> {
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        for sub in std::fs::read_dir(entry.path())? {
            let sub = sub?;
            let p = sub.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut out = Vec::with_capacity(files.len());
    for p in files {
        out.push(load_file(&p)?);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

// ---------------------------------------------------------------------------
// 绑定：把 fixture 里的 `$symbol` 变成这次回放用的真值
// ---------------------------------------------------------------------------

/// 一次回放用的身份。抽取器只声明两类可绑定语义（见 `BINDABLE`）：
/// 除它们以外的符号已在抽取期被 skip 成 `value_unresolved`，所以这里的
/// `resolve` 对未知符号直接报错而不是猜一个值。
#[derive(Debug, Clone, Copy)]
pub struct Bindings {
    pub user_id: Uuid,
    pub workspace_id: Uuid,
}

impl Bindings {
    #[must_use]
    pub fn stateless() -> Self {
        Self {
            user_id: Uuid::from_u128(STATELESS_USER_ID),
            workspace_id: Uuid::from_u128(STATELESS_WORKSPACE_ID),
        }
    }

    #[must_use]
    pub fn new(user_id: Uuid, workspace_id: Uuid) -> Self {
        Self {
            user_id,
            workspace_id,
        }
    }

    fn lookup(&self, sym: &str) -> Option<String> {
        match sym {
            "$testUserID" => Some(self.user_id.to_string()),
            "$testWorkspaceID" => Some(self.workspace_id.to_string()),
            _ => None,
        }
    }

    /// 解析一个 fixture 里的取值：`$symbol` 走绑定表，其余当字面量。
    pub fn resolve(&self, raw: &str) -> Result<String, String> {
        let trimmed = raw.trim();
        if trimmed.starts_with('$') {
            return self
                .lookup(trimmed)
                .ok_or_else(|| format!("unbound symbol {trimmed}"));
        }
        Ok(trimmed.to_string())
    }
}

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
pub fn plan(fx: &Fixture, bindings: &Bindings) -> Result<RequestPlan, String> {
    let mut notes = Vec::new();

    // ---- 路径：`{name}` 用 path_params 的取值填进去 --------------------------
    let mut path = fx.path.clone();
    for (name, raw) in &fx.path_params {
        let value = bindings.resolve(raw)?;
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
            let v = bindings.resolve(raw)?;
            pairs.push(format!("{}={}", urlencode(k), urlencode(&v)));
        }
        path = format!("{path}?{}", pairs.join("&"));
        notes.push(format!("query: {}", pairs.join("&")));
    }

    // ---- header：fixture 自带的 + actor 身份的翻译 ---------------------------
    let mut headers: Vec<(HeaderName, String)> = Vec::new();
    for (k, raw) in &fx.headers {
        let v = bindings.resolve(raw)?;
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
        // 令牌 / agent 身份需要真令牌或 agent 行，当前回放器不伪造它们。
        ActorKind::Token | ActorKind::Agent => {
            return Err(format!(
                "actor kind {:?} needs a real credential; this runner does not fabricate one",
                fx.actor.kind
            ));
        }
        ActorKind::System => return Err("actor kind system is internal-only".into()),
    }
    // 剩下的身份 header（X-Workspace-ID 等）按上游原样转发。
    for (k, raw) in identity {
        let v = bindings.resolve(&raw)?;
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

// ---------------------------------------------------------------------------
// 判定
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// 与上游断言一致。
    Pass,
    /// 打到了已实现的路由，但与上游断言不符。
    Mismatch,
    /// 本仓没有这条路由。
    Unmounted,
    /// 路由在，但只是 M0 占位实现。
    Placeholder,
    /// 本仓无法构造这次请求。
    Unevaluable,
}

impl Outcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Mismatch => "mismatch",
            Self::Unmounted => "unmounted",
            Self::Placeholder => "placeholder",
            Self::Unevaluable => "unevaluable",
        }
    }

    /// 取"更强"的结论：变体声明次序即强度次序（`Pass` 最前 = 最强），
    /// 所以 "更好" 就是较小的那个。合并两层时用 `a < b` 直接比较即可。
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Mismatch => 1,
            Self::Placeholder => 2,
            Self::Unmounted => 3,
            Self::Unevaluable => 4,
        }
    }
}

/// 一次回放的原始观察。
#[derive(Debug, Clone)]
pub struct Observed {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

/// 每个 fixture 一行的结论。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureOutcome {
    pub outcome: Outcome,
    pub tier: String,
    pub detail: String,
    pub status_observed: Option<u16>,
    pub offline: Option<Outcome>,
    pub database: Option<Outcome>,
}

/// `expect.json_subset` 是响应 body 的"子集"（逐层递归，数组按下标对应）。
///
/// `Err` 里带回第一个不匹配的路径，方便直接看出是哪个字段。
pub fn json_subset(actual: &serde_json::Value, expected: &serde_json::Value) -> Result<(), String> {
    use serde_json::Value;
    fn go(actual: &Value, expected: &Value, at: &str) -> Result<(), String> {
        match (actual, expected) {
            (Value::Object(a), Value::Object(e)) => {
                for (k, ev) in e {
                    match a.get(k) {
                        Some(av) => go(av, ev, &format!("{at}.{k}"))?,
                        None => return Err(format!("{at}.{k}: missing in response")),
                    }
                }
                Ok(())
            }
            (Value::Array(a), Value::Array(e)) => {
                if a.len() < e.len() {
                    return Err(format!(
                        "{at}: response array has {} items, expected at least {}",
                        a.len(),
                        e.len()
                    ));
                }
                for (i, ev) in e.iter().enumerate() {
                    go(&a[i], ev, &format!("{at}[{i}]"))?;
                }
                Ok(())
            }
            (a, e) => {
                if a == e {
                    Ok(())
                } else {
                    Err(format!("{at}: response {a} != expected {e}"))
                }
            }
        }
    }
    go(actual, expected, "$")
}

/// 判定一次回放：状态码 + 可选 `json_subset`。
pub fn judge(fx: &Fixture, observed: &Observed) -> (Outcome, String) {
    let empty_body = observed.body.is_empty();
    let json = observed
        .content_type
        .as_deref()
        .is_some_and(|c| c.contains("application/json"));
    let expected = fx.expect.status;

    if observed.status == expected {
        if fx.expect.json_subset.is_null()
            || fx
                .expect
                .json_subset
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        {
            return (Outcome::Pass, "status matched".into());
        }
        let parsed: serde_json::Value = match serde_json::from_slice(&observed.body) {
            Ok(v) => v,
            Err(e) => {
                return (
                    Outcome::Mismatch,
                    format!("status matched but body is not json: {e}"),
                )
            }
        };
        return match json_subset(&parsed, &fx.expect.json_subset) {
            Ok(()) => (Outcome::Pass, "status + json_subset matched".into()),
            Err(e) => (Outcome::Mismatch, format!("json_subset mismatch at {e}")),
        };
    }

    // 状态码不同：先分辨"没实现"与"实现错了"。
    if observed.status == 404 && empty_body && !json {
        return (
            Outcome::Unmounted,
            format!("no route: 404 with empty body (axum fallback), expected {expected}"),
        );
    }
    if observed.status == 405 {
        return (
            Outcome::Unmounted,
            format!("path exists but method is not mounted (405), expected {expected}"),
        );
    }
    if observed.status == 501 {
        return (
            Outcome::Placeholder,
            format!("route is a declared stub (501), expected {expected}"),
        );
    }
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&observed.body) {
        if v.get("code").and_then(|c| c.as_str()) == Some("not_implemented") {
            return (
                Outcome::Placeholder,
                format!(
                    "route returns the M0 placeholder envelope (status {}), expected {expected}",
                    observed.status
                ),
            );
        }
    }
    (
        Outcome::Mismatch,
        format!("status {} != expected {expected}", observed.status),
    )
}

/// 打一次请求并判定。
pub async fn replay_one(app: &Router, fx: &Fixture, bindings: &Bindings) -> FixtureOutcome {
    match plan(fx, bindings) {
        Err(reason) => FixtureOutcome {
            outcome: Outcome::Unevaluable,
            tier: "none".into(),
            detail: reason,
            status_observed: None,
            offline: None,
            database: None,
        },
        Ok(p) => {
            let notes = p.notes.join("; ");
            let request = match p.to_http() {
                Ok(r) => r,
                Err(e) => {
                    return FixtureOutcome {
                        outcome: Outcome::Unevaluable,
                        tier: "none".into(),
                        detail: format!("cannot build request: {e}"),
                        status_observed: None,
                        offline: None,
                        database: None,
                    }
                }
            };
            let response = match app.clone().oneshot(request).await {
                Ok(r) => r,
                Err(e) => {
                    return FixtureOutcome {
                        outcome: Outcome::Unevaluable,
                        tier: "none".into(),
                        detail: format!("router refused the request: {e}"),
                        status_observed: None,
                        offline: None,
                        database: None,
                    }
                }
            };
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = to_bytes(response.into_body(), 1 << 20)
                .await
                .map(|b| b.to_vec())
                .unwrap_or_default();
            let observed = Observed {
                status,
                body,
                content_type,
            };
            let (outcome, mut detail) = judge(fx, &observed);
            if !notes.is_empty() {
                detail = format!("{detail}; {notes}");
            }
            FixtureOutcome {
                outcome,
                tier: "stateless".into(),
                detail,
                status_observed: Some(status),
                offline: Some(outcome),
                database: None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 报告
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub domain: String,
    pub method: String,
    pub path: String,
    pub actor: String,
    pub via: String,
    pub status_expected: u16,
    pub source: String,
    pub outcome: Outcome,
    pub tier: String,
    pub status_observed: Option<u16>,
    pub offline: Option<Outcome>,
    pub database: Option<Outcome>,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub fixtures: usize,
    pub pass: usize,
    pub mismatch: usize,
    pub unmounted: usize,
    pub placeholder: usize,
    pub unevaluable: usize,
    pub by_actor: BTreeMap<String, BTreeMap<String, usize>>,
    pub by_via: BTreeMap<String, BTreeMap<String, usize>>,
    pub tiers: BTreeMap<String, BTreeMap<String, usize>>,
}

/// 报告的头部：口径写进产物本身，读者不必去翻文档才能解释数字。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub golden_dir: String,
    pub bindings: BTreeMap<String, String>,
    pub totals: Totals,
    /// 契约等价率 = pass / fixtures（打不到 = 未实现，仍留在分母里）。
    pub contract_equivalence_rate: f64,
    /// 已接入路由等价率 = pass / (pass + mismatch)：只问"实现了的路由对不对"。
    pub mounted_equivalence_rate: Option<f64>,
    /// 离线可判定的 fixture 数与其中 pass 的数量（`actor.kind == "anonymous"`）。
    pub offline_decidable: OfflineSplit,
    pub fixtures: Vec<Row>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OfflineSplit {
    pub fixtures: usize,
    pub pass: usize,
}

impl Report {
    #[must_use]
    pub fn from_rows(golden_dir: &Path, bindings: &Bindings, mut rows: Vec<Row>) -> Self {
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        let mut totals = Totals {
            fixtures: rows.len(),
            ..Totals::default()
        };
        for r in &rows {
            match r.outcome {
                Outcome::Pass => totals.pass += 1,
                Outcome::Mismatch => totals.mismatch += 1,
                Outcome::Unmounted => totals.unmounted += 1,
                Outcome::Placeholder => totals.placeholder += 1,
                Outcome::Unevaluable => totals.unevaluable += 1,
            }
            *totals
                .by_actor
                .entry(r.actor.clone())
                .or_default()
                .entry(r.outcome.as_str().to_string())
                .or_insert(0) += 1;
            *totals
                .by_via
                .entry(r.via.clone())
                .or_default()
                .entry(r.outcome.as_str().to_string())
                .or_insert(0) += 1;
            *totals
                .tiers
                .entry(r.tier.clone())
                .or_default()
                .entry(r.outcome.as_str().to_string())
                .or_insert(0) += 1;
        }
        let eq = if totals.fixtures == 0 {
            0.0
        } else {
            ratio(totals.pass, totals.fixtures)
        };
        let mounted_den = totals.pass + totals.mismatch;
        let mounted = if mounted_den == 0 {
            None
        } else {
            Some(ratio(totals.pass, mounted_den))
        };
        let offline_rows: Vec<&Row> = rows.iter().filter(|r| r.actor == "anonymous").collect();
        let offline_decidable = OfflineSplit {
            fixtures: offline_rows.len(),
            pass: offline_rows
                .iter()
                .filter(|r| r.outcome == Outcome::Pass)
                .count(),
        };
        let mut binding_map = BTreeMap::new();
        binding_map.insert("user_id".into(), bindings.user_id.to_string());
        binding_map.insert("workspace_id".into(), bindings.workspace_id.to_string());
        Self {
            schema_version: SCHEMA_VERSION,
            golden_dir: golden_dir.display().to_string(),
            bindings: binding_map,
            totals,
            contract_equivalence_rate: eq,
            mounted_equivalence_rate: mounted,
            offline_decidable,
            fixtures: rows,
        }
    }

    /// 稳定的 JSON 文本（`--check` 就是拿它做字节比对）。
    pub fn to_json(&self) -> Result<String> {
        let mut s = serde_json::to_string_pretty(self)?;
        s.push('\n');
        Ok(s)
    }

    pub fn render_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "golden: {}  fixtures: {}",
            self.golden_dir, self.totals.fixtures
        );
        let _ = writeln!(
            out,
            "  pass {}  mismatch {}  unmounted {}  placeholder {}  unevaluable {}",
            self.totals.pass,
            self.totals.mismatch,
            self.totals.unmounted,
            self.totals.placeholder,
            self.totals.unevaluable
        );
        let _ = writeln!(
            out,
            "  契约等价率 = {}/{} = {:.1}%",
            self.totals.pass,
            self.totals.fixtures,
            self.contract_equivalence_rate * 100.0
        );
        match self.mounted_equivalence_rate {
            Some(r) => {
                let _ = writeln!(
                    out,
                    "  已接入路由等价率 = {}/{} = {:.1}%",
                    self.totals.pass,
                    self.totals.pass + self.totals.mismatch,
                    r * 100.0
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "  已接入路由等价率 = n/a（没有打到已实现路由的 fixture）"
                );
            }
        }
        let _ = writeln!(
            out,
            "  离线可判定（anonymous）= {}/{} pass",
            self.offline_decidable.pass, self.offline_decidable.fixtures
        );
        let _ = writeln!(out, "  --- 非 pass ---");
        for r in &self.fixtures {
            if r.outcome != Outcome::Pass {
                let _ = writeln!(
                    out,
                    "  {:<11} {:<6} {:>3} {:<44} {}",
                    r.outcome.as_str(),
                    r.method,
                    r.status_expected,
                    r.path,
                    r.id
                );
            }
        }
        out
    }

    /// 汇总一行（给 CI 日志 / issue 评论用）。
    #[must_use]
    pub fn summary_line(&self) -> String {
        format!(
            "pass {}/{} ({:.1}%) · mismatch {} · unmounted {} · placeholder {} · unevaluable {}",
            self.totals.pass,
            self.totals.fixtures,
            self.contract_equivalence_rate * 100.0,
            self.totals.mismatch,
            self.totals.unmounted,
            self.totals.placeholder,
            self.totals.unevaluable
        )
    }
}

#[allow(clippy::cast_precision_loss)] // 计数远小于 2^53，比例精度足够。
fn ratio(num: usize, den: usize) -> f64 {
    (num as f64) / (den as f64)
}

// ---------------------------------------------------------------------------
// 回放驱动
// ---------------------------------------------------------------------------

/// 回放层次。
///
/// 层与 fixture 的匹配规则是**显式**的：stateless 层只判定 `anonymous` fixture
/// （它的全部结论都能在没有数据库时得出）；其余 fixture 在这一层标 `unevaluable`
/// 并写明原因 —— 不用"没库导致的 500"冒充 `mismatch`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// 无数据库连接（懒连接池指向不可达端口），只判定匿名断言。
    Stateless,
    /// 真库 + 迁移 + 种子身份，全部 fixture 都可判定。
    Database,
}

impl Tier {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stateless => "stateless",
            Self::Database => "database",
        }
    }

    #[must_use]
    pub fn supports(self, fx: &Fixture) -> bool {
        match self {
            Self::Stateless => fx.actor.kind == ActorKind::Anonymous,
            Self::Database => true,
        }
    }
}

/// 跑一层：`app` 是这一层的 router。
pub async fn run_tier(
    app: &Router,
    fixtures: &[Fixture],
    bindings: &Bindings,
    tier: Tier,
) -> Vec<FixtureOutcome> {
    let mut out = Vec::with_capacity(fixtures.len());
    for fx in fixtures {
        let mut got = if tier.supports(fx) {
            replay_one(app, fx, bindings).await
        } else {
            FixtureOutcome {
                outcome: Outcome::Unevaluable,
                tier: tier.as_str().into(),
                detail: format!(
                    "{} actor needs the database tier (rerun with --db-url); stateless tier cannot decide it",
                    fx.actor.kind.as_str()
                ),
                status_observed: None,
                offline: None,
                database: None,
            }
        };
        got.tier = tier.as_str().to_string();
        match tier {
            Tier::Stateless => got.offline = Some(got.outcome),
            Tier::Database => got.database = Some(got.outcome),
        }
        out.push(got);
    }
    out
}

/// 合并两层观察：取更强的一层，并记录结论来自哪层。
#[must_use]
pub fn merge(
    stateless: Option<FixtureOutcome>,
    database: Option<FixtureOutcome>,
) -> FixtureOutcome {
    match (stateless, database) {
        (None, None) => FixtureOutcome {
            outcome: Outcome::Unevaluable,
            tier: "none".into(),
            detail: "no tier ran this fixture".into(),
            status_observed: None,
            offline: None,
            database: None,
        },
        (Some(s), None) => s,
        (None, Some(d)) => d,
        (Some(s), Some(d)) => {
            let (winner, tier) = if d.outcome < s.outcome {
                (d.clone(), "database")
            } else {
                (s.clone(), "stateless")
            };
            FixtureOutcome {
                outcome: winner.outcome,
                tier: tier.into(),
                detail: winner.detail,
                status_observed: winner.status_observed,
                offline: s.offline.or(Some(s.outcome)),
                database: d.database.or(Some(d.outcome)),
            }
        }
    }
}

/// 把 fixture + 两层观察拼成报告行。
#[must_use]
pub fn to_row(fx: &Fixture, merged: &FixtureOutcome) -> Row {
    Row {
        id: fx.id.clone(),
        domain: fx.domain(),
        method: fx.method.clone(),
        path: fx.path.clone(),
        actor: fx.actor.kind.as_str().to_string(),
        via: if fx.source.via.is_empty() {
            "unknown".into()
        } else {
            fx.source.via.clone()
        },
        status_expected: fx.expect.status,
        source: format!("{}:{}", fx.source.file, fx.source.line),
        outcome: merged.outcome,
        tier: merged.tier.clone(),
        status_observed: merged.status_observed,
        offline: merged.offline,
        database: merged.database,
        detail: merged.detail.clone(),
    }
}
