//! `refresh.rs` 的测试道具（**只在 `cfg(test)` 下编译**）。
//!
//! 四件东西：
//!
//! 1. [`HttpDouble`] + [`github_double`] —— **离线 GraphQL 替身**（`docs/61` §4.2 的 M8-5 行）。
//!    它是一台**真的 HTTP/1.1 服务端**（手写在 `TcpListener` 上，因为本 crate 的依赖边被 anchor
//!    冻结、加不了测试用 web 框架），替的**只有平台 wire**：`/app/installations/{id}/access_tokens`
//!    与 `/graphql` 两个端点，其余路径 404。断言链走真 `reqwest` + 真 JWT 签名 + 真分页循环
//!    ⇒ 出站请求的**头与体**逐字段可比对（替身三条纪律之二）。
//! 2. [`FixedStore`] —— [`SnapshotStore`] 的记录型替身（管道用例不碰数据库）。
//! 3. [`RecordingTimer`] —— 捕获退避**序列**并手动触发（`DoD`：不许 sleep 真实时间）。
//! 4. 抓取器替身：[`FetcherFn`]（任意行为）、[`ConcurrencyFetcher`]（观测同时在飞数）、
//!    [`GatedFetcher`]（可控闸门，用来制造「在飞期间又来了一个事件」）。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::id::Id;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::{
    Address, ManagerOptions, PrRowRef, PrSnapshot, ResolvedTarget, SnapshotFetcher, SnapshotStore,
    StoreError, Timer, Tuning,
};
use crate::ghsnapshot::snapshot::SnapshotCheck;
use crate::ghsnapshot::Client;
use crate::rest::GithubError;

// ---------------------------------------------------------------------------
// 测试私钥（与 `app.rs` 单测里那枚**逐字同一枚**；不是生产密钥）
// ---------------------------------------------------------------------------

/// `app.rs` 的 `TEST_KEY_PEM` 是它测试模块的私有常量（那个文件是 M8-1 的写集，本片只读）⇒
/// 这里复制同一枚密钥。它没有任何别的消费者，只为让 `Client::sign_app_jwt` 真的签出一枚 JWT
/// （替身不验签，但签名路径必须真跑过）。
pub(crate) const TEST_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQC8Lko4B+10Dj7o
dZedrDE3ZbHRBAsSKACHGOsI00EzEEUh8di1MVB1O/dYC2ZwlsteuwkZFF+aQDuT
3gZTzQVkq5menpNmJ5BVKfQr6jy7mQAhhgUEG69Afu2QNC3hFe3S+lcjdt9agM3r
rbuK35OrPrhaeW33bO+1ip0r5gnnw0j6IaW+Tnndzusv1H7tBJj9R6IA7E7tPGIr
UbdWADlP2XDPC/g5R37XhY5H5sg9B3dJptrVBD532eHG0nK7UTQybq7QMHOygYn5
KfbgZZLnYwpV40SE56DhR9jzxcIdo8okGMeoIlbwebC09spzLXwi66bPW6jE7eKY
cYwfvixlAgMBAAECggEACpr7QNQljERfRD+YV2EEdxBKqLJ3I0NQ4ExFtr4dLxEM
LGESawfH9ot2Iaam09KTzJdy6FBvIOTc1rUNGzzzQFyxcDCUsw2owzv1kGIHoTT6
vmjssHIU+ugMYHOoYEaZnCnSrmN9K/8VW+JzLtzx2BVVU3gDfA3OJqeUuwwgY8jT
bSrNe8LKiS6sj5IF2hIpBIXFTuE6SKU/64T3kiKG2AbnLLtF34LNl1sbw5TTjEAs
hGnDvNJqHgCtDqGDXaAiNbTrPWeWMJW4RhfLDzF1fpZXSw3f5z+zVMzgg6JSBZCj
Pf5FAsU4db9cGW+yLjb5NqNrBX/c24OxZ2yNcPRSkQKBgQDsraU02Mf2uFj22T0r
HBp3q11IcpuA7YU2lBP6o4aKlc6cCyj8U75yS7hu/kUoEtAUnxyyCXOpM7lhGbmz
dtIqM6r74FlIr8mPi2TLMloUGmtY42sUNtciQgd+Ds4RjWNC2/1FRe5XMFm1bP/f
6jMZEr0k0d0Cz7au5sZg4Jwg6QKBgQDLixe4xIOvQ7wvKpKngKbStW0MIAoYnvSB
hIj3xxasXc6V/qp9ANTnG28yk9tsCajUMQSfEX0uIx1YSAB0MbaIrAhsqvUaknDx
vkUtaeBWE8e0H3U+9KVnVWLZoPNHFIVeGhWMXJKpbQiZiDreELaAeltgIzU5JfAP
VRJ8eoOiHQKBgQCopqQOoFr9aCec3vhDe+cwVyBFu8UrfhVq6uHBvDznDBEKCLnP
9CzFbUejb/T/tUgpKahdBXcxnvX+R0KYq5bfE6pHiXqV3Q2YCBBu6xZdNOZBlOx8
nwd2Fe8Y2JvmzgVpYzF653YLEx0Ztu4uNMjsmPnG/vSqSDE5OKEr72HR4QKBgQDF
jWKgulr1KNDlFnTwjjVcHSqRsicabmzxqCkoE9s1wHZZrqraWIxLIp1ygX9eBKIQ
EONjYB4XQY2huYB3Rijbzdz/W445FBj7CKkrwq8x3FDfygiJ6fj/qigfAdAdFRW8
l6SCbvcJ6gGGwmogTihT2m4FiSaHKQMuXmtq1Z4dIQKBgQCBdv0m4+OX6Rxjx3cd
cpr1nFZBZqPqOdDLakWXRko+K0eNFOKCF4Smf3OUVZz5yoAAGG9HcXEwUO09ZzCD
6FZCMLwGRY4IXmRObxEfD5k/ZlzL9+/rNIKtQjObwppciqPW0NoPt5qJvMJXq7Dw
C2CZCWlMaEGLpAGiVraQnRQlPw==
-----END PRIVATE KEY-----
";

// ---------------------------------------------------------------------------
// 离线 HTTP 替身（真的 HTTP/1.1，只替平台 wire）
// ---------------------------------------------------------------------------

/// 一次到达替身的请求（逐字段可比对）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WireRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl WireRequest {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 请求体的 JSON（非 JSON ⇒ `Null`）。
    pub(crate) fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// 替身的一条响应。
#[derive(Debug, Clone)]
pub(crate) struct WireResponse {
    pub status: u16,
    pub body: String,
    pub extra_headers: Vec<(String, String)>,
}

impl WireResponse {
    pub(crate) fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            body: body.to_string(),
            extra_headers: Vec::new(),
        }
    }

    pub(crate) fn status(status: u16) -> Self {
        Self {
            status,
            body: String::new(),
            extra_headers: Vec::new(),
        }
    }

    pub(crate) fn with_header(mut self, name: &str, value: &str) -> Self {
        self.extra_headers
            .push((name.to_string(), value.to_string()));
        self
    }
}

/// 手写的 HTTP/1.1 替身：`TcpListener` + 一次请求一条响应（`connection: close`）。
pub(crate) struct HttpDouble {
    base_url: String,
    requests: Arc<Mutex<Vec<WireRequest>>>,
    task: JoinHandle<()>,
}

impl Drop for HttpDouble {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HttpDouble {
    pub(crate) async fn start<F>(handler: F) -> Self
    where
        F: Fn(&WireRequest) -> WireResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind double");
        let addr = listener.local_addr().expect("double addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(handler);
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let seen = seen.clone();
                tokio::spawn(async move {
                    serve_connection(socket, &handler, &seen).await;
                });
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            requests,
            task,
        }
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn requests(&self) -> Vec<WireRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    pub(crate) fn requests_to(&self, suffix: &str) -> Vec<WireRequest> {
        self.requests()
            .into_iter()
            .filter(|request| request.path.ends_with(suffix))
            .collect()
    }
}

async fn serve_connection<F>(
    mut socket: TcpStream,
    handler: &Arc<F>,
    seen: &Arc<Mutex<Vec<WireRequest>>>,
) where
    F: Fn(&WireRequest) -> WireResponse + Send + Sync,
{
    let Ok(Some(request)) = read_request(&mut socket).await else {
        return;
    };
    seen.lock().expect("requests lock").push(request.clone());
    let response = handler(&request);
    let mut raw = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    for (name, value) in &response.extra_headers {
        let _ = write!(raw, "{name}: {value}\r\n");
    }
    raw.push_str("\r\n");
    raw.push_str(&response.body);
    let _ = socket.write_all(raw.as_bytes()).await;
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

async fn read_request(socket: &mut TcpStream) -> std::io::Result<Option<WireRequest>> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            break position;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok(Some(WireRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 替身对 `/graphql` 的三种回答（token 端点由 [`github_double`] 自动处理）。
#[derive(Debug, Clone)]
pub(crate) enum Wire {
    /// `{"data": <data>}`。
    Data(Value),
    /// `403` + `retry-after: N`（二级限流的真实形状）。
    RateLimited { retry_after_secs: u64 },
}

/// **离线 GitHub 替身**：`/app/installations/{id}/access_tokens` 回 201 真形状的 token；
/// `/graphql` 由 `answer` 决定；其余 404。
pub(crate) async fn github_double<F>(answer: F) -> HttpDouble
where
    F: Fn(&WireRequest) -> Wire + Send + Sync + 'static,
{
    HttpDouble::start(move |request| {
        if request.path.ends_with("/access_tokens") {
            let expires_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
            return WireResponse::json(
                201,
                &json!({"token": "ghs_installation_token", "expires_at": expires_at}),
            );
        }
        if request.path.ends_with("/graphql") {
            return match answer(request) {
                Wire::Data(data) => WireResponse::json(200, &json!({"data": data})),
                Wire::RateLimited { retry_after_secs } => WireResponse::status(403)
                    .with_header("retry-after", &retry_after_secs.to_string()),
            };
        }
        WireResponse::status(404)
    })
    .await
}

/// 指向替身的**已启用**客户端（真 App id + 真私钥 ⇒ 真走 JWT → token → GraphQL 三步）。
pub(crate) fn enabled_client(base_url: &str) -> Client {
    Client::new(Some("123".into()), Some(TEST_KEY_PEM.into())).with_api_base(base_url)
}

// ---------------------------------------------------------------------------
// 存储替身
// ---------------------------------------------------------------------------

/// [`SnapshotStore`] 的记录型替身：行为由字段摆布，调用被记下来供断言。
#[derive(Default)]
pub(crate) struct FixedStore {
    target: Mutex<Option<ResolvedTarget>>,
    target_by_number: Mutex<HashMap<i32, ResolvedTarget>>,
    rows: Mutex<Vec<PrRowRef>>,
    /// `true` = head-SHA 守卫通过（`apply_snapshot` 回 `Ok(true)`）。
    apply_ok: AtomicBool,
    applied: Mutex<Vec<(Id, PrSnapshot, i64)>>,
    sweep_rows: Mutex<Vec<Address>>,
    sweep_calls: Mutex<Vec<(i64, Address, i32)>>,
    sweep_fails: AtomicBool,
    pub(crate) resolve_calls: AtomicUsize,
    pub(crate) list_calls: AtomicUsize,
}

impl FixedStore {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(Some(ResolvedTarget {
                installation_id: 7,
                snapshot_fetched_at: None,
            })),
            apply_ok: AtomicBool::new(true),
            ..Self::default()
        })
    }

    pub(crate) fn with_rows(self: &Arc<Self>, rows: Vec<PrRowRef>) -> Arc<Self> {
        *self.rows.lock().expect("rows lock") = rows;
        self.clone()
    }

    /// 指定编号的解析结果（view TTL 用例要按 PR 区分「刚抓过 / 陈旧 / 从未」）。
    pub(crate) fn set_target_for(&self, number: i32, target: ResolvedTarget) {
        self.target_by_number
            .lock()
            .expect("target by number lock")
            .insert(number, target);
    }

    pub(crate) fn set_apply_ok(&self, ok: bool) {
        self.apply_ok.store(ok, Ordering::SeqCst);
    }

    pub(crate) fn set_sweep_rows(&self, rows: Vec<Address>) {
        *self.sweep_rows.lock().expect("sweep lock") = rows;
    }

    pub(crate) fn set_sweep_fails(&self, fails: bool) {
        self.sweep_fails.store(fails, Ordering::SeqCst);
    }

    pub(crate) fn applied(&self) -> Vec<(Id, PrSnapshot, i64)> {
        self.applied.lock().expect("applied lock").clone()
    }

    pub(crate) fn sweep_calls(&self) -> Vec<(i64, Address, i32)> {
        self.sweep_calls.lock().expect("sweep calls lock").clone()
    }
}

#[async_trait]
impl SnapshotStore for FixedStore {
    async fn resolve_installation(
        &self,
        _workspace_id: Id,
        _owner: &str,
        _repo: &str,
        number: i32,
    ) -> Result<Option<ResolvedTarget>, StoreError> {
        self.resolve_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(target) = self
            .target_by_number
            .lock()
            .expect("target by number lock")
            .get(&number)
        {
            return Ok(Some(target.clone()));
        }
        Ok(self.target.lock().expect("target lock").clone())
    }

    async fn list_rows(&self, _address: &Address) -> Result<Vec<PrRowRef>, StoreError> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.rows.lock().expect("rows lock").clone())
    }

    async fn apply_snapshot(
        &self,
        pr_id: Id,
        snapshot: &PrSnapshot,
        now_unix: i64,
    ) -> Result<bool, StoreError> {
        if !self.apply_ok.load(Ordering::SeqCst) {
            return Ok(false);
        }
        self.applied
            .lock()
            .expect("applied lock")
            .push((pr_id, snapshot.clone(), now_unix));
        Ok(true)
    }

    async fn list_stale_undecided(
        &self,
        older_than_unix: i64,
        after: &Address,
        max_rows: i32,
    ) -> Result<Vec<Address>, StoreError> {
        self.sweep_calls.lock().expect("sweep calls lock").push((
            older_than_unix,
            after.clone(),
            max_rows,
        ));
        if self.sweep_fails.load(Ordering::SeqCst) {
            return Err(StoreError::Db("sweep exploded".into()));
        }
        Ok(self.sweep_rows.lock().expect("sweep lock").clone())
    }
}

// ---------------------------------------------------------------------------
// 定时器 / 抓取器替身
// ---------------------------------------------------------------------------

/// 记录型定时器：捕获延迟序列并可**手动**触发（默认实现会真的睡）。
#[derive(Default)]
pub(crate) struct RecordingTimer {
    delays: Mutex<Vec<Duration>>,
    callbacks: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl Timer for RecordingTimer {
    fn schedule(&self, delay: Duration, callback: Box<dyn FnOnce() + Send + 'static>) {
        self.delays.lock().expect("delays lock").push(delay);
        self.callbacks
            .lock()
            .expect("callbacks lock")
            .push(callback);
    }
}

impl RecordingTimer {
    pub(crate) fn delays(&self) -> Vec<Duration> {
        self.delays.lock().expect("delays lock").clone()
    }

    /// 触发最早的一条（FIFO）。
    pub(crate) fn fire_next(&self) -> bool {
        let callback = {
            let mut callbacks = self.callbacks.lock().expect("callbacks lock");
            if callbacks.is_empty() {
                return false;
            }
            callbacks.remove(0)
        };
        callback();
        true
    }
}

/// 闭包抓取器（测试里最常用）。
///
/// 闭包收的是**按值**的 [`Address`]（不是引用）：`Fn(&Address)` 会要求闭包对任意生命周期
/// 都成立（`for<'a> Fn(&'a Address)`），而测试里的 `|_| …` 会被推断成某个具体生命周期 ⇒
/// 编译器报「implementation of `Fn` is not general enough」。按值传就绕开了这条 HRTB。
pub(crate) struct FetcherFn<F>(pub(crate) F);

#[async_trait]
impl<F> SnapshotFetcher for FetcherFn<F>
where
    F: Fn(Address) -> Result<PrSnapshot, GithubError> + Send + Sync,
{
    async fn fetch(
        &self,
        _client: &Client,
        address: &Address,
        _now_unix: i64,
    ) -> Result<PrSnapshot, GithubError> {
        (self.0)(address.clone())
    }
}

/// 计数 + 观测**同时在飞**数的抓取器（worker 池并发上限与租户隔离用）。
pub(crate) struct ConcurrencyFetcher {
    live: AtomicUsize,
    max_live: AtomicUsize,
    calls: AtomicUsize,
    per_installation: Mutex<HashMap<i64, usize>>,
    snapshot: PrSnapshot,
}

impl ConcurrencyFetcher {
    pub(crate) fn new(snapshot: PrSnapshot) -> Arc<Self> {
        Arc::new(Self {
            live: AtomicUsize::new(0),
            max_live: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            per_installation: Mutex::new(HashMap::new()),
            snapshot,
        })
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(crate) fn max_live(&self) -> usize {
        self.max_live.load(Ordering::SeqCst)
    }

    pub(crate) fn calls_for(&self, installation_id: i64) -> usize {
        self.per_installation
            .lock()
            .expect("per installation lock")
            .get(&installation_id)
            .copied()
            .unwrap_or(0)
    }
}

#[async_trait]
impl SnapshotFetcher for ConcurrencyFetcher {
    async fn fetch(
        &self,
        _client: &Client,
        address: &Address,
        _now_unix: i64,
    ) -> Result<PrSnapshot, GithubError> {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_live.fetch_max(live, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self
            .per_installation
            .lock()
            .expect("per installation lock")
            .entry(address.installation_id)
            .or_insert(0) += 1;
        tokio::time::sleep(Duration::from_millis(25)).await;
        self.live.fetch_sub(1, Ordering::SeqCst);
        Ok(self.snapshot.clone())
    }
}

/// 带闸门的抓取器：每次进入都挂住，直到测试显式放行 —— 用来制造
/// 「一次抓取还在飞，同地址又来了一个事件」的**确定性**时序。
pub(crate) struct GatedFetcher {
    release: Notify,
    calls: AtomicUsize,
    live: AtomicUsize,
    max_live: AtomicUsize,
    snapshot: PrSnapshot,
}

impl GatedFetcher {
    pub(crate) fn new(snapshot: PrSnapshot) -> Arc<Self> {
        Arc::new(Self {
            release: Notify::new(),
            calls: AtomicUsize::new(0),
            live: AtomicUsize::new(0),
            max_live: AtomicUsize::new(0),
            snapshot,
        })
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(crate) fn max_live(&self) -> usize {
        self.max_live.load(Ordering::SeqCst)
    }

    /// 等第 `count` 次抓取真的进来（默认 2 秒上限）。
    pub(crate) async fn wait_entered(&self, count: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while self.calls() < count {
            assert!(
                tokio::time::Instant::now() < deadline,
                "第 {count} 次抓取没在 2 秒内进入（当前 {}）",
                self.calls()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// 放行一次抓取（`notify_one` 会**存一张许可** ⇒ 先放行后等待也不会丢）。
    pub(crate) fn release_all(&self) {
        self.release.notify_one();
    }
}

#[async_trait]
impl SnapshotFetcher for GatedFetcher {
    async fn fetch(
        &self,
        _client: &Client,
        _address: &Address,
        _now_unix: i64,
    ) -> Result<PrSnapshot, GithubError> {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_live.fetch_max(live, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.release.notified().await;
        self.live.fetch_sub(1, Ordering::SeqCst);
        Ok(self.snapshot.clone())
    }
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 可推进的假时钟（unix 秒）。
#[derive(Clone, Default)]
pub(crate) struct FakeClock(Arc<AtomicI64>);

impl FakeClock {
    pub(crate) fn at(now: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now)))
    }

    pub(crate) fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    pub(crate) fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }

    /// 注入给 [`ManagerOptions`] 的闭包。
    pub(crate) fn source(&self) -> Box<dyn Fn() -> i64 + Send + Sync> {
        let clock = self.clone();
        Box::new(move || clock.now())
    }
}

/// 已决的绿快照（`MERGEABLE` / `SUCCESS` / 一条 completed 的 check）。
pub(crate) fn decided_snapshot() -> PrSnapshot {
    PrSnapshot {
        head_sha: "sha-current".into(),
        mergeable: Some("MERGEABLE".into()),
        merge_state_status: Some("CLEAN".into()),
        rollup_state: Some("SUCCESS".into()),
        has_checks: true,
        checks: vec![SnapshotCheck {
            name: "ci".into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
            details_url: None,
            is_status_context: false,
        }],
    }
}

/// 未决快照（CI 还在跑）—— chase 窗口的燃料。
pub(crate) fn running_snapshot(head: &str) -> PrSnapshot {
    PrSnapshot {
        head_sha: head.into(),
        mergeable: Some("MERGEABLE".into()),
        merge_state_status: Some("CLEAN".into()),
        rollup_state: Some("PENDING".into()),
        has_checks: true,
        checks: vec![SnapshotCheck {
            name: "ci".into(),
            status: "in_progress".into(),
            conclusion: None,
            details_url: None,
            is_status_context: false,
        }],
    }
}

/// 一行 open 的 PR（chase 的必要条件之一）。
///
/// `uuid` **不是**本 crate 的依赖边（anchor 冻结的 `Cargo.toml`），所以行 id 用 `Id::new()`
/// —— 用例只关心「被写了没有」，不关心具体值。
pub(crate) fn open_row() -> PrRowRef {
    PrRowRef {
        id: Id::new(),
        state: "open".into(),
    }
}

/// 轮询等待一个条件成立（**不**依赖具体 sleep 时长）。
pub(crate) async fn wait_until<F: Fn() -> bool>(condition: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if condition() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 测试用调参：sweeper 与抖动都关掉（用例自己调 `sweep_once`）。
pub(crate) fn quiet_tuning() -> Tuning {
    Tuning {
        sweep_interval: Duration::from_secs(3600),
        shutdown_grace: Duration::from_millis(500),
        ..Tuning::default()
    }
}

/// 组装一份「零抖动 + 假时钟 + 记录型定时器」的 options。
pub(crate) fn options(
    tuning: Tuning,
    clock: &FakeClock,
    timer: Arc<RecordingTimer>,
    fetcher: Arc<dyn SnapshotFetcher>,
) -> ManagerOptions {
    ManagerOptions {
        tuning,
        fetcher,
        timer,
        clock: clock.source(),
        jitter: Box::new(|| Duration::ZERO),
        on_applied: None,
    }
}

/// 一个固定的 workspace id（用例里只当键用）。
pub(crate) fn workspace_id() -> Id {
    Id::parse("11111111-1111-4111-8111-111111111111").expect("fixed workspace id")
}

/// 真实 unix 秒（wire 用例让 token 的 `expires_at` 落在未来）。
pub(crate) fn real_now() -> i64 {
    super::system_now_unix()
}

/// 把替身收到的 GraphQL 请求体里的 `variables` 取出来。
pub(crate) fn variables_of(request: &WireRequest) -> Value {
    request
        .json()
        .get("variables")
        .cloned()
        .unwrap_or(Value::Null)
}
