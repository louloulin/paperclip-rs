//! 出站传输：**唯一的** `mc-cloud` 客户端（`anchor` 唯一"实现"的部分）。
//!
//! 逐字对照上游 `server/internal/cloudruntime/client.go`（255 行）。四条代理簇
//! （cloud-runtime / billing / subscriptions / stripe webhook）**共用这一份**客户端
//! —— 上游 `cloud_billing.go:16` 逐字：「Fleet and Billing share `:8080`」，拆成两处
//! 会立刻出现第二份 base-URL 解析与错误映射（`docs/62` §2.2 判据 3）。
//!
//! # 上游 → 本仓的逐条对照
//!
//! | 上游 | 本仓 | 备注 |
//! | --- | --- | --- |
//! | `Config{BaseURL,Timeout,HTTPClient,Recorder}` | [`Config`] | builder 形态；`base_url` 在 `Debug` 里脱敏 |
//! | `Request{Method,Path,Query,Body,UserID,RequestID,Op,Headers}` | [`Request`] | `Headers` 用 `Vec<(String,String)>` 保序保多值（`http.Header` 语义） |
//! | `Response{StatusCode,Header,Body}` | [`Response`] | 同上用 `Vec` 保多值 |
//! | `NewClient(cfg) *Client` | [`Client::new`] → `Result` | 本仓**提前校验**基址（上游把校验留给 `doInner`） |
//! | `Enabled()` | [`Client::enabled`] | 逐字：规范化后的 base URL 非空 |
//! | `Do(ctx, req)` | [`Client::send`] | ⚠️ `do` 是 Rust **保留字** ⇒ 改名 `send` |
//! | `inferCloudRuntimeOp` | [`infer_op`] | 桶完全一致（`billing`/`gateway`/`provision`/`terminate`/`status`/`fleet`） |
//! | `requestStatusBucket` | [`status_bucket`] | 取值域一致 `{ok,4xx,5xx,timeout,error}` |
//! | `maxResponseBodySize = 1<<20` | [`MAX_RESPONSE_BODY_SIZE`] | 超限 ⇒ [`CloudError::ResponseTooLarge`] |
//! | `defaultTimeout = 35s` | [`DEFAULT_TIMEOUT`] | 逐字 |
//!
//! # 四条**刻意不照抄**的地方（全部登记 `docs/32` §9.13）
//!
//! 1. **错误文本不带 URL / 响应体**（上游 `fmt.Errorf("%w: %s", …)` 拼了 `baseURL`）
//!    —— 见 [`crate::error`] 的模块头；
//! 2. **`X-User-ID` / `X-Request-ID` 由本客户端盖章**，调用方在 [`Request::headers`] 里
//!    传的同名头**被丢掉**（上游逐字：「must not be overridable by the caller」）；
//!    `Accept` / `Content-Type` **可以**被覆盖（stripe 转发要保 `Content-Type`）。
//!    实现是上游的「先删同名、再逐值 add」——所以同名多值（`Header.Values`）全保留；
//!    ⚠️ 唯一的外观差异：线上的头名是**小写**（`stripe-signature`），Go 侧是规范大小写
//!    （`Stripe-Signature`）。HTTP/1.1 头名大小写不敏感，语义等价（登记 `docs/32` §9.13）；
//! 3. **出站体上限**：上游只限**响应**体（`maxResponseBodySize`），请求体的 1 MiB 上限
//!    由 handler 的 `MaxBytesReader` 管（`cloud_runtime.go:31`）⇒ 本客户端**不限**请求体，
//!    避免出现第二处上限；
//! 4. **重定向策略保持 reqwest 默认**（最多 10 跳），与上游 `http.Client{}` 一致
//!    —— `mc-entitlement` 那边才是"不跟随跨源重定向"（上游 `CheckRedirect`）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use url::Url;

use crate::error::CloudError;

/// 上游 `cloudruntime.defaultTimeout`（35s）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(35);

/// 上游 `cloudruntime.maxResponseBodySize`（1 MiB）。
pub const MAX_RESPONSE_BODY_SIZE: usize = 1 << 20;

/// 上游 `cloudruntime.RequestRecorder`：每个出站请求一条观测。
///
/// 上游注释逐字：nil recorder 安全 no-op。本仓用 `Option<Arc<dyn …>>` 表达同一件事。
/// 生产把它接到业务指标收集器；测试留 `None`（或接一个计数替身）。
pub trait RequestRecorder: Send + Sync {
    /// `op` = [`infer_op`] 的桶；`status` ∈ `{ok,4xx,5xx,timeout,error}`；
    /// `duration` = 整次调用的墙钟秒数。
    fn record_cloud_runtime_request(&self, op: &str, status: &str, duration_seconds: f64);
}

/// 客户端构造参数（上游 `cloudruntime.Config`）。
#[derive(Clone, Default)]
pub struct Config {
    base_url: String,
    timeout: Option<Duration>,
    http_client: Option<reqwest::Client>,
    recorder: Option<Arc<dyn RequestRecorder>>,
}

impl Config {
    /// 只给基址（`""` ⇒ 禁用客户端；非法 ⇒ [`Client::new`] 报错）。
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            ..Self::default()
        }
    }

    /// 出站超时（≤0 / 不设 ⇒ [`DEFAULT_TIMEOUT`]）。
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// 注入 HTTP client（测试替身 / 自定义 TLS 装配点）。
    #[must_use]
    pub fn with_http_client(mut self, http_client: reqwest::Client) -> Self {
        self.http_client = Some(http_client);
        self
    }

    /// 注入计量口。
    #[must_use]
    pub fn with_recorder(mut self, recorder: Arc<dyn RequestRecorder>) -> Self {
        self.recorder = Some(recorder);
        self
    }

    /// 原始基址（**未**规范化）。
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl std::fmt::Debug for Config {
    /// 手写脱敏：基址不进 `Debug`（判据 ①，`docs/62` §2.4）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field(
                "base_url",
                &if self.base_url.is_empty() {
                    None
                } else {
                    Some("<redacted>")
                },
            )
            .field("timeout", &self.timeout)
            .field("http_client", &self.http_client.is_some())
            .field("recorder", &self.recorder.is_some())
            .finish()
    }
}

/// 一次出站请求（上游 `cloudruntime.Request`）。
///
/// ⚠️ **手写 `Debug`**（判据 ①，`docs/62` §2.4）：本类型承载 `Idempotency-Key`、
/// `Stripe-Signature` 与出站体 —— 三者都不得进日志。`Debug` 只暴露形状与长度。
#[derive(Clone)]
pub struct Request {
    /// HTTP 方法。
    pub method: reqwest::Method,
    /// **以 `/` 开头**的绝对路径（上游 `doInner` 的断言）。
    pub path: String,
    /// 查询参数（保序、保多值；上游 `url.Values`）。
    pub query: Vec<(String, String)>,
    /// 请求体（`None` / 空 ⇒ 不设 `Content-Type`）。
    pub body: Option<Vec<u8>>,
    /// 身份（盖章成 `X-User-ID`；`None` ⇒ 不注入 —— stripe 转发就是这样）。
    pub user_id: Option<mc_core::Id>,
    /// 盖章成 `X-Request-ID`。
    pub request_id: Option<String>,
    /// 计量标签；空 ⇒ 由路径推导（[`infer_op`]）。
    pub op: Option<String>,
    /// 调用方要**逐字转发**的头（在默认头之后应用；`X-User-ID`/`X-Request-ID` 除外）。
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("query_keys", &self.query.len())
            .field("body_len", &self.body.as_ref().map_or(0, Vec::len))
            .field("user_id", &self.user_id)
            .field("request_id", &self.request_id)
            .field("op", &self.op)
            // 头的**值**一律不打印（`Idempotency-Key` / `Stripe-Signature` 在这里）。
            .field("header_names", &header_names(&self.headers))
            .finish_non_exhaustive()
    }
}

/// 头名列表（`Debug` 用；**不回显值**）。
fn header_names(headers: &[(String, String)]) -> Vec<&str> {
    headers.iter().map(|(name, _)| name.as_str()).collect()
}

impl Default for Request {
    fn default() -> Self {
        Self {
            method: reqwest::Method::GET,
            path: String::new(),
            query: Vec::new(),
            body: None,
            user_id: None,
            request_id: None,
            op: None,
            headers: Vec::new(),
        }
    }
}

impl Request {
    /// `GET <path>`。
    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    /// `POST <path>`。
    #[must_use]
    pub fn post(path: impl Into<String>) -> Self {
        Self {
            method: reqwest::Method::POST,
            path: path.into(),
            ..Self::default()
        }
    }

    /// 方法。
    #[must_use]
    pub fn with_method(mut self, method: reqwest::Method) -> Self {
        self.method = method;
        self
    }

    /// 请求体（原样字节；**不做**任何 `trim` / 重编码 —— stripe 转发的硬要求）。
    #[must_use]
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// 查询参数。
    #[must_use]
    pub fn with_query(mut self, query: Vec<(String, String)>) -> Self {
        self.query = query;
        self
    }

    /// `X-User-ID`。
    #[must_use]
    pub fn with_user_id(mut self, user_id: mc_core::Id) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// `X-Request-ID`。
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// 计量标签。
    #[must_use]
    pub fn with_op(mut self, op: impl Into<String>) -> Self {
        self.op = Some(op.into());
        self
    }

    /// 追加一个逐字转发的头（可重复调用，保留多值）。
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// 云侧响应（上游 `cloudruntime.Response`）。
///
/// ⚠️ **手写 `Debug`**（判据 ①，`docs/62` §2.4）：本类型承载**出站响应体** ——
/// 云侧体可能含租户内部标识，`Debug` 一律不打印它（只给字节数）。需要体时走
/// [`Response::json`] / [`Response::body_lossy`]，由调用方自行决定去处。
#[derive(Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP 状态码（**原样**透传给客户端，含 5xx —— 见 `docs/32` §9.13 的口径订正）。
    pub status: u16,
    /// 全部响应头（保序、保多值）。
    pub headers: Vec<(String, String)>,
    /// 响应体（未做任何转换）。
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("header_names", &header_names(&self.headers))
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl Response {
    /// 首个同名头的值（上游 `http.Header.Get` 的语义：取第一条）。
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 全部同名头的值（`http.Header.Values`）。
    #[must_use]
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// 状态桶是否为 `ok`（2xx / 3xx）—— 与 [`status_bucket`] 同判据。
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..400).contains(&self.status)
    }

    /// 体是否为空（上游 `bytes.TrimSpace(resp.Body)` 的判空步）。
    #[must_use]
    pub fn is_body_blank(&self) -> bool {
        self.body.iter().all(u8::is_ascii_whitespace)
    }

    /// 体的有损文本（**只用于日志/诊断**，不得回显给客户端）。
    #[must_use]
    pub fn body_lossy(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// 反序列化响应体。
    ///
    /// # Errors
    ///
    /// 体不是合法 JSON ⇒ [`CloudError::InvalidJson`]（**不**携带体内容）。
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, CloudError> {
        serde_json::from_slice(&self.body).map_err(|_| CloudError::InvalidJson)
    }
}

/// 出站客户端（上游 `cloudruntime.Client`）。`Clone` 廉价（内部 `Arc`）。
#[derive(Clone)]
pub struct Client {
    /// `None` = 禁用（上游 `baseURL == ""`）。
    base_url: Option<Url>,
    http: reqwest::Client,
    recorder: Option<Arc<dyn RequestRecorder>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("enabled", &self.enabled())
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("recorder", &self.recorder.is_some())
            // `http` 不入 `Debug`（`finish_non_exhaustive` 即「还有未展示的字段」）。
            .finish_non_exhaustive()
    }
}

impl Client {
    /// 构造。
    ///
    /// 三态（与 [`crate::config::CloudSettings`] 一一对应）：
    ///
    /// | `base_url` | 结果 | [`enabled`](Self::enabled) |
    /// | --- | --- | :-: |
    /// | 空 / 全空白 / 只有斜杠 | `Ok`（**禁用的客户端**） | `false` |
    /// | 非空且合法 | `Ok` | `true` |
    /// | 非空但非法（含 userinfo / query / fragment） | [`CloudError::InvalidBaseUrl`] | — |
    ///
    /// # Errors
    ///
    /// [`CloudError::InvalidBaseUrl`] —— 上游把这一步推迟到 `doInner`，本仓提前到构造
    /// （`docs/62` §2.4 判据 ① 要求"在校验阶段拒绝"）。
    pub fn new(config: Config) -> Result<Self, CloudError> {
        let timeout = config
            .timeout
            .filter(|t| !t.is_zero())
            .unwrap_or(DEFAULT_TIMEOUT);
        let http = config.http_client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_default()
        });
        let Some(normalized) = crate::config::normalize(&config.base_url) else {
            return Ok(Self {
                base_url: None,
                http,
                recorder: config.recorder,
            });
        };
        let parsed = crate::config::validate(&normalized)?;
        // `validate` 已保证无 query / fragment / userinfo，但规范化与解析之间
        // 不允许出现第二种口径 ⇒ 这里再规范一次 path 的尾斜杠（上游 `doInner` 每请求做）。
        Ok(Self {
            base_url: Some(parsed),
            http,
            recorder: config.recorder,
        })
    }

    /// 直接建一个**禁用**的客户端（无基址）。
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            base_url: None,
            http: reqwest::Client::default(),
            recorder: None,
        }
    }

    /// 上游 `Enabled()`：规范化后的基址非空。
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.base_url.is_some()
    }

    /// 计量口（诊断用）。
    #[must_use]
    pub fn has_recorder(&self) -> bool {
        self.recorder.is_some()
    }

    /// 一次出站调用（上游 `Do`）。
    ///
    /// # Errors
    ///
    /// [`CloudError::Disabled`]（未配置 ⇒ **零出站**）/
    /// [`CloudError::InvalidPath`] / [`CloudError::Timeout`] /
    /// [`CloudError::Transport`] / [`CloudError::ResponseTooLarge`]。
    ///
    /// ⚠️ 云侧的 **4xx / 5xx 不是错误** —— 它们原样出现在 `Ok(Response)` 里
    /// （上游 `doInner` 的语义，`docs/32` §9.13 登记了这与 §2.6 表注的差异）。
    pub async fn send(&self, request: Request) -> Result<Response, CloudError> {
        let Some(base) = self.base_url.as_ref() else {
            // 禁用 ⇒ **在构造 URL 之前**返回：这条早退是"未配置时不发出站请求"的唯一保证。
            return Err(CloudError::Disabled);
        };
        let op = infer_op(request.op.as_deref(), &request.method, &request.path);
        let started = Instant::now();
        let result = self.send_inner(base, &request).await;
        if let Some(recorder) = &self.recorder {
            recorder.record_cloud_runtime_request(
                &op,
                status_bucket(&result),
                started.elapsed().as_secs_f64(),
            );
        }
        result
    }

    async fn send_inner(&self, base: &Url, request: &Request) -> Result<Response, CloudError> {
        if !request.path.starts_with('/') {
            return Err(CloudError::InvalidPath);
        }
        let target = build_target(base, request);
        let headers = build_headers(request)?;
        let mut builder = self
            .http
            .request(request.method.clone(), target)
            .headers(headers);
        let has_body = request.body.as_ref().is_some_and(|body| !body.is_empty());
        if has_body {
            // 只在**有体**时挂 body：上游 `if len(req.Body) > 0 { … }`。
            builder = builder.body(request.body.clone().unwrap_or_default());
        }

        let response = builder
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = read_bounded_body(response).await?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// 拼目标 URL：`base.path` 去尾斜杠 + `request.path`，再挂查询串。
///
/// 逐字对照上游 `doInner`：`u.Path = TrimRight(base.Path, "/") + req.Path` +
/// `u.RawQuery = req.Query.Encode()`（本仓用 [`Url::query_pairs_mut`] 得到同一编码）。
fn build_target(base: &Url, request: &Request) -> Url {
    let mut target = base.clone();
    let joined = format!("{}{}", base.path().trim_end_matches('/'), request.path);
    target.set_path(&joined);
    target.set_query(None);
    if !request.query.is_empty() {
        let mut pairer = target.query_pairs_mut();
        for (key, value) in &request.query {
            pairer.append_pair(key, value);
        }
        drop(pairer);
    }
    target
}

/// 拼出站头（上游 `doInner` 的 `Header.Set` / `Del`+`Add` 语义，逐条一致）。
///
/// 三层，**顺序就是语义**：
///
/// 1. **默认头** `Accept: application/json`（+ 有体时的 `Content-Type: application/json`）；
/// 2. **调用方头**：先 `Del(同名)` 再逐值 `Add` —— 所以**能覆盖**默认的 `Accept`/
///    `Content-Type`，且同名多值（`Header.Values`）在一次分组里全部保留；
/// 3. **盖章头** `X-User-ID` / `X-Request-ID`：最后 `Set`，调用方传了也**被丢掉**。
///
/// # Errors
///
/// 头名 / 头值无法表示 ⇒ [`CloudError::InvalidHeader`]（只报**名字**，不报值）。
fn build_headers(request: &Request) -> Result<reqwest::header::HeaderMap, CloudError> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if request.body.as_ref().is_some_and(|body| !body.is_empty()) {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    for (name, values) in group_headers(&request.headers) {
        if is_stamped_header(&name) {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| CloudError::InvalidHeader { name: name.clone() })?;
        let mut parsed = Vec::with_capacity(values.len());
        for value in &values {
            parsed.push(
                HeaderValue::from_str(value)
                    .map_err(|_| CloudError::InvalidHeader { name: name.clone() })?,
            );
        }
        // 与上游同款：**先删后加**（这是 `Accept` 可被覆盖、`X-User-ID` 不可被覆盖的机制）。
        headers.remove(&header_name);
        for value in parsed {
            headers.append(header_name.clone(), value);
        }
    }
    if let Some(user_id) = request.user_id {
        let value =
            HeaderValue::from_str(&user_id.to_string()).map_err(|_| CloudError::InvalidHeader {
                name: "x-user-id".into(),
            })?;
        headers.insert("X-User-ID", value);
    }
    if let Some(request_id) = &request.request_id {
        let value = HeaderValue::from_str(request_id).map_err(|_| CloudError::InvalidHeader {
            name: "x-request-id".into(),
        })?;
        headers.insert("X-Request-ID", value);
    }
    Ok(headers)
}

/// 把调用方头按**名字**分组（大小写不敏感，保首次出现顺序）。
///
/// 上游 `http.Header` 是 `map[string][]string` ⇒ 同名多值在**一次** `Del`+`Add` 里处理。
/// `Request::headers` 是扁平 `Vec`（保序保多值）⇒ 拼头前先分组，否则第二值会删掉第一值。
fn group_headers(headers: &[(String, String)]) -> Vec<(String, Vec<String>)> {
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for (name, value) in headers {
        let key = name.to_ascii_lowercase();
        match grouped.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, values)) => values.push(value.clone()),
            None => grouped.push((key, vec![value.clone()])),
        }
    }
    grouped
}

/// 是否「由本客户端盖章」的头（调用方传了也丢）—— 上游逐字比较
/// `http.CanonicalHeaderKey(k) == "X-User-Id" || == "X-Request-Id"`。
fn is_stamped_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("x-user-id") || name.eq_ignore_ascii_case("x-request-id")
}

/// 有界读体：超过 [`MAX_RESPONSE_BODY_SIZE`] 立刻放弃（上游 `LimitReader(max+1)` 的等价物）。
async fn read_bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, CloudError> {
    let mut body: Vec<u8> = Vec::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|error| classify_transport(&error))?;
        let Some(chunk) = chunk else {
            return Ok(body);
        };
        if body.len() + chunk.len() > MAX_RESPONSE_BODY_SIZE {
            return Err(CloudError::ResponseTooLarge {
                limit: MAX_RESPONSE_BODY_SIZE,
            });
        }
        body.extend_from_slice(&chunk);
    }
}

/// `reqwest::Error` → [`CloudError`]（**只带类别**，绝不带底层文本——它内嵌 URL）。
fn classify_transport(error: &reqwest::Error) -> CloudError {
    if error.is_timeout() {
        CloudError::Timeout
    } else {
        CloudError::Transport
    }
}

/// 上游 `inferCloudRuntimeOp`：显式 `op` 优先，否则由路径推导桶。
///
/// 六个桶逐字保留：`billing` / `gateway` / `provision` / `terminate` / `status` / `fleet`。
/// ⚠️ `billing` 这一支只匹配 `/billing`；`subscriptions` 的路径**不含** `/billing`
/// ⇒ 它落 `fleet`，与上游一致（上游也是这样，不因为"看起来该叫 billing"而改）。
#[must_use]
pub fn infer_op(op: Option<&str>, method: &reqwest::Method, path: &str) -> String {
    if let Some(explicit) = op {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return trimmed.to_ascii_lowercase();
        }
    }
    if path.contains("/billing") {
        return "billing".to_string();
    }
    if path.contains("/gateway") || path.contains("/proxy") || path.contains("/exec") {
        return "gateway".to_string();
    }
    if path.contains("/start") || path.contains("/provision") || path.contains("/nodes/create") {
        return "provision".to_string();
    }
    if path.contains("/stop") || path.contains("/terminate") || path.contains("/reboot") {
        return "terminate".to_string();
    }
    if path.contains("/status") || path.contains("/health") || path.contains("/ready") {
        return "status".to_string();
    }
    if path.contains("/nodes") {
        return match *method {
            reqwest::Method::POST => "provision",
            reqwest::Method::DELETE => "terminate",
            _ => "status",
        }
        .to_string();
    }
    "fleet".to_string()
}

/// 上游 `requestStatusBucket`：`(response, error)` → 固定状态枚举。
#[must_use]
pub fn status_bucket(result: &Result<Response, CloudError>) -> &'static str {
    match result {
        Err(error) => error.status_bucket(),
        Ok(response) => match response.status {
            200..=399 => "ok",
            400..=499 => "4xx",
            500..=599 => "5xx",
            _ => "error",
        },
    }
}

#[cfg(test)]
mod tests;
