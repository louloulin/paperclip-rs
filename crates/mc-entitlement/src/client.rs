//! 策略客户端的**端点契约** —— 实现（单飞刷新 + 超时 + `Provider`）归 **M9-9**。
//!
//! anchor 期本文件只钉**一件事**：出站端点的**路径形状**，因为它是唯一一个
//! 「上游事实、不由实现自定」的字符串。逐字对照上游 `internal/entitlement/client.go:241`：
//!
//! ```text
//! u.Path = strings.TrimRight(c.baseURL.Path, "/") + "/api/v1/internal/entitlement-policies/" + workspaceID.String()
//! ```
//!
//! # M9-9 要填什么（上游 `client.go` 422 行）
//!
//! 1. `Client::new(cfg)` 的**三态**：空基址 ⇒ 禁用的客户端（`Ok`，**不是**错误，
//!    上游 `if strings.TrimSpace(cfg.BaseURL) == "" { return &Client{…}, nil }`）；
//!    非空非法 ⇒ `Err`（上游 `ErrInvalidConfig`，三件套 = 绝对 URL + 无凭据 + 无 query/fragment
//!    —— **复用 `mc_cloud::config::validate`**，别写第二份，`docs/62` §2.2 判据 3）；
//!    非空合法 ⇒ 启用的客户端；
//! 2. **单飞刷新**（每工作区一个 in-flight；Rust 侧用 `tokio::sync::Mutex` +
//!    「拿锁的人刷新、其余人等结果」的形态即可 —— 不引入 `singleflight` 依赖）；
//! 3. **不跟随跨源重定向**（上游 `CheckRedirect` 逐字返回 `http.ErrUseLastResponse`）；
//! 4. **三档时效**（[`crate::cache`] 的常量）：新鲜 ⇒ 直接用；陈旧 ⇒ `enforce` 降级为
//!    `observe`（[`crate::Gate::downgraded_when_stale`]）；退避期 ⇒ 不重试；
//! 5. **校验**（上游 `normalizePolicy` / `normalizeGate`）：`schema_version == 1`、
//!    `policy_revision > 0`、`subscription_version >= 0`、`valid_until` 非零、
//!    `0 < valid_for_seconds <= MAX_POLICY_TTL`、两个 gate **都必须在 `gates` 里**、
//!    每个 gate 的 `action ∈ {off, enforce}`、`limit >= 0`、
//!    period 三字段「全有或全无」、`autopilot_runs` **必须**有 period 三字段、
//!    `period_start < period_end` 且 `period_start < reset_at`；
//! 6. **版本回退不写缓存**（[`crate::cache::MAX_ENTRIES`] 段的第 3 条纪律）；
//! 7. **`Observer` 的四个调用点**（[`crate::Observer`]，全部低基数）。
//!
//! # 为什么 anchor 不写一个 `todo!()` 的 `Client`
//!
//! 一个 `pub fn new() -> Self { todo!() }` 在**编译期**看着像接好了、在**运行期**是 panic
//! （`docs/37` 反复登记的那类"静默假接入"的镜像）。anchor 的交付形态是：**端点路径可用**
//! （本文件的 [`policy_endpoint_path`]）+ **适配器诚实空跑**
//! （`apps/mc-server/src/entitlement.rs` 在配了基址时**不装平面并 warn**）。
//! ⇒ 没有任何路径能走进未实现的代码。
//!
//! # M9-9（`LUM-1824`）落地：两条**形状不同**的路径
//!
//! 上游把「读缓存」与「发请求」写在同一个同步方法 `Gate` 里。本仓**写不了**那个形状：
//!
//! - [`crate::Provider::gate`] 是**同步、无 IO** 的（`types.rs` 冻结，且
//!   `mc_autopilot::quota::QuotaPolicyProvider` 逐字要求「同步、无 IO」）——它会被 axum 的
//!   handler 直接调用，**在 async 上下文里**；
//! - 本 crate 的依赖边被锚点冻结（`Cargo.toml`：`M9-9 的写者不得再新增三方依赖`）⇒ **没有**
//!   `tokio`，也就没有「在 `gate()` 里阻塞等一个 tokio future」这种写法。
//!
//! ⇒ 拆成两条：
//!
//! | 路径 | 签名 | 职责 |
//! | --- | --- | --- |
//! | [`Provider::gate`] | 同步、无 IO | 读缓存判一格（新鲜 / 陈旧 / 不可达）+ **记一笔需求** |
//! | [`Client::refresh`] | `async` | 单飞 + 出站 + 校验 + 写缓存（由组合根驱动） |
//!
//! 🔴 **由此产生的一条硬纪律**：`gate()` **绝不**自己发请求。冷启动后的第一次调用返回
//! **fail-open 的 `off`**，随后由组合根的刷新器把策略装进缓存，第二次调用即命中。
//! 「同步阻塞版」与「需求驱动的异步版」在**每格结论上的唯一差别**就是这一拍延迟，
//! 而 fail-open 让这拍延迟不产生事故。登记在 `docs/32` §9.13（编号起手复核）。

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use mc_core::{Id, Timestamp};
use serde::Deserialize;
use url::Url;

use crate::cache::MAX_RESPONSE_BODY_SIZE;
use crate::cache::{PolicyCache, PolicySnapshot, PutOutcome};
use crate::types::{
    off_decision, Action, CacheOutcome, Decision, Gate, GateName, NotificationPolicy, Observer,
    Provider, Reason, RefreshOutcome, SCHEMA_VERSION,
};

/// 上游 `client.go:241` 拼出的端点路径（**不含** base URL，也不含任何 query）。
///
/// `workspace_id` 是**唯一**的输入 —— 端点不回显、也不接受别的租户维度
/// （上游注释逐字：缓存键「is never inferred from response data」）。
#[must_use]
pub fn policy_endpoint_path(workspace_id: Id) -> String {
    format!("/api/v1/internal/entitlement-policies/{workspace_id}")
}

/// 端点路径前缀（给「是不是我们的策略端点」这类诊断用）。
pub const POLICY_ENDPOINT_PREFIX: &str = "/api/v1/internal/entitlement-policies/";

// ---------------------------------------------------------------------------
// wire：云侧的响应形状（上游 `wirePolicy` / `wireGate` / `wireNotificationPolicy`）
// ---------------------------------------------------------------------------

/// 云侧策略响应（上游 `wirePolicy`）。
///
/// 逐字保留可空形态（`limit` / 三个 period 字段都是指针），**校验不靠 serde**：
/// 上游是 `json.Unmarshal` 之后逐字段判，我们用 `Option` + [`normalize_policy`] 复刻。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WirePolicy {
    /// 协议版本（必须 == [`SCHEMA_VERSION`]）。
    pub schema_version: i32,
    /// 策略修订号（必须 > 0）。
    pub policy_revision: i64,
    /// 订阅版本号（必须 >= 0）。
    pub subscription_version: i64,
    /// 云侧有效期。
    pub valid_until: Option<Timestamp>,
    /// 回话的 TTL 秒数（必须 `0 < v <= MAX_POLICY_TTL`）。
    pub valid_for_seconds: i64,
    /// 两个 gate（**必须都在**，否则 `InvalidPolicy`）。
    pub gates: std::collections::HashMap<String, WireGate>,
}

/// 一个 gate 的 wire 形态（上游 `wireGate`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireGate {
    /// `off` / `enforce`（**`observe` 会被判非法**，逐字上游 `normalizeGate`）。
    pub action: String,
    /// 上限（`enforce` 时必须给且 >= 0）。
    pub limit: Option<i64>,
    /// 周期起点。
    pub period_start: Option<Timestamp>,
    /// 周期终点。
    pub period_end: Option<Timestamp>,
    /// 重置时刻。
    pub reset_at: Option<Timestamp>,
    /// 展示策略（听不听得懂**不影响** enforcement）。
    pub notifications: Option<WireNotificationPolicy>,
}

/// 通知策略的 wire 形态（上游 `wireNotificationPolicy`）。
#[derive(Debug, Clone, Deserialize)]
pub struct WireNotificationPolicy {
    /// 唯一被接受的取值。
    pub on_rejection: String,
}

/// 一次成功刷新的产物：快照 + 回话的 TTL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPolicy {
    /// 快照。
    pub snapshot: PolicySnapshot,
    /// `valid_for_seconds`（已过 `0 < v <= MAX_POLICY_TTL` 校验）。
    pub valid_for: Duration,
}

/// 策略响应**不合法**（上游 `ErrInvalidPolicy`）。
///
/// 单独一个类型而不是 `anyhow` 风格的字符串：它会被 `Reason::InvalidPolicy` 直接消费。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPolicy;

impl std::fmt::Display for InvalidPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 🔴 **不回显任何云侧字段**（`docs/62` §2.4）：错误路径上不得把响应体带出来。
        formatter.write_str("entitlement: invalid policy response")
    }
}

impl std::error::Error for InvalidPolicy {}

/// `wirePolicy` → 快照（上游 `normalizePolicy` + `normalizeGate`，逐条）。
///
/// # Errors
///
/// 任何一条不满足 ⇒ [`InvalidPolicy`]。
pub fn normalize_policy(wire: &WirePolicy) -> Result<FetchedPolicy, InvalidPolicy> {
    let valid_until = wire.valid_until.ok_or(InvalidPolicy)?;
    if wire.schema_version != SCHEMA_VERSION
        || wire.policy_revision <= 0
        || wire.subscription_version < 0
        || valid_until.as_unix() < 0
        || wire.valid_for_seconds <= 0
        || wire.valid_for_seconds > max_ttl_seconds()
    {
        return Err(InvalidPolicy);
    }
    // 顺序逐字对应 `GateName::ALL`（`PolicySnapshot::gate` 按同序下标取）。
    let gates = [
        normalize_gate(GateName::IssueCount, wire.gate(GateName::IssueCount)?)?,
        normalize_gate(GateName::AutopilotRuns, wire.gate(GateName::AutopilotRuns)?)?,
    ];
    Ok(FetchedPolicy {
        snapshot: PolicySnapshot {
            policy_revision: wire.policy_revision,
            subscription_version: wire.subscription_version,
            cloud_valid_until: valid_until,
            gates,
        },
        valid_for: Duration::from_secs(
            u64::try_from(wire.valid_for_seconds).map_err(|_| InvalidPolicy)?,
        ),
    })
}

/// `WirePolicy::gate`：**缺一个就是 `InvalidPolicy`**（逐字上游 `ok` 判据）。
trait WireGateLookup {
    fn gate(&self, name: GateName) -> Result<&WireGate, InvalidPolicy>;
}

impl WireGateLookup for WirePolicy {
    fn gate(&self, name: GateName) -> Result<&WireGate, InvalidPolicy> {
        self.gates.get(name.as_str()).ok_or(InvalidPolicy)
    }
}

/// 单个 gate 的规范化（上游 `normalizeGate`）。
fn normalize_gate(name: GateName, wire: &WireGate) -> Result<Gate, InvalidPolicy> {
    let action = Action::parse_wire(&wire.action).ok_or(InvalidPolicy)?;
    if action == Action::Off {
        // 上游逐字：`case ActionOff: return Gate{Action: ActionOff}, nil` —— limit / period
        // 一律不看（多给了也不报错，但也不填）。
        return Ok(Gate::off());
    }
    let limit = wire
        .limit
        .filter(|value| *value >= 0)
        .ok_or(InvalidPolicy)?;
    let present = [
        wire.period_start.is_some(),
        wire.period_end.is_some(),
        wire.reset_at.is_some(),
    ]
    .iter()
    .filter(|present| **present)
    .count();
    if present != 0 && present != 3 {
        return Err(InvalidPolicy);
    }
    if name == GateName::AutopilotRuns && present != 3 {
        return Err(InvalidPolicy);
    }
    let mut gate = Gate {
        action,
        limit: Some(limit),
        period_start: wire.period_start,
        period_end: wire.period_end,
        reset_at: wire.reset_at,
        notifications: normalize_notification_policy(wire.notifications.as_ref()),
    };
    if present == 3 {
        // 逐字：`!PeriodStart.Before(*PeriodEnd) || !PeriodStart.Before(*ResetAt)`。
        if gate.period_start >= gate.period_end || gate.period_start >= gate.reset_at {
            return Err(InvalidPolicy);
        }
    } else {
        gate.period_start = None;
        gate.period_end = None;
        gate.reset_at = None;
    }
    Ok(gate)
}

/// 通知策略：**听不听得懂与 enforcement 无关**（上游注释逐字：它坏掉不得让 gate 失效）。
fn normalize_notification_policy(
    wire: Option<&WireNotificationPolicy>,
) -> Option<NotificationPolicy> {
    wire.and_then(|policy| {
        (policy.on_rejection == crate::types::NOTIFICATION_FIRST_REJECTION_PER_PERIOD)
            .then(NotificationPolicy::first_rejection_per_period)
    })
}

fn max_ttl_seconds() -> i64 {
    i64::try_from(crate::cache::MAX_POLICY_TTL.as_secs()).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------

/// 客户端构造/配置失败（上游 `ErrInvalidConfig`）。
///
/// ⚠️ **没有**「基址非法」这个变体：基址的三件套校验**只有一份**，在
/// `mc_cloud::config::validate`（经 `mc_http::state::cloud::EntitlementConfig` 到达组合根）。
/// 本仓**不写第二份** ⇒ 调用方只能传一个**已校验的** `Url`（`docs/62` §2.2 判据 3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientError {
    /// 出站 HTTP 客户端构造失败（TLS 后端缺失等启动期问题）。
    HttpClient,
    /// 基址串解析失败（**不含**三件套校验 —— 见 [`Client::from_validated_base_url`]）。
    BaseUrl,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HttpClient => {
                formatter.write_str("entitlement: failed to build the outbound HTTP client")
            }
            Self::BaseUrl => formatter.write_str("entitlement: the base URL did not parse"),
        }
    }
}

impl std::error::Error for ClientError {}

/// 策略客户端（上游 `entitlement.Client`）。
///
/// `Debug` **只暴露存在性**（`docs/62` §2.4 判据 ④：基址不进日志）。
#[derive(Clone)]
pub struct Client {
    enabled: bool,
    base_url: Option<Url>,
    cache: Arc<PolicyCache>,
    observer: Option<Arc<dyn Observer>>,
    http: Option<reqwest::Client>,
    /// 在飞的工作区（单飞）。
    in_flight: Arc<Mutex<HashSet<Id>>>,
    /// 尚未被刷新器满足的「需要一份策略」的工作区。
    demands: Arc<Mutex<HashSet<Id>>>,
    /// 测试可注入的时钟（上游的 `now func() time.Time` 字段）。
    clock: Arc<dyn Fn() -> Timestamp + Send + Sync>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("cached_workspaces", &self.cache.len())
            .field("pending_demands", &self.pending_demands())
            .field("demands", &self.demands)
            .field("in_flight", &self.in_flight_count())
            .field("http", &self.http.is_some())
            .field("observer", &self.observer.is_some())
            // 闭包不进 `Debug`（它没有有意义的可读形态），只报存在性。
            .field("clock", &"injected-or-system")
            .finish()
    }
}

impl Client {
    /// 三态之一：**未配置**（`None`）⇒ 禁用的客户端，**不是**错误（上游 `New` 的空基址分支）。
    ///
    /// `base_url` 必须**已通过** `mc_cloud::config::validate`（本 crate 不重复校验）。
    ///
    /// # Errors
    ///
    /// [`ClientError::HttpClient`]：出站客户端构造失败。
    pub fn new(
        base_url: Option<Url>,
        observer: Option<Arc<dyn Observer>>,
    ) -> Result<Self, ClientError> {
        let http = if base_url.is_some() {
            Some(
                reqwest::Client::builder()
                    // 逐字上游 `CheckRedirect`：**不跟随**重定向（内部端点不得被带到别的源）。
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|_| ClientError::HttpClient)?,
            )
        } else {
            None
        };
        Ok(Self {
            enabled: base_url.is_some(),
            base_url,
            cache: Arc::new(PolicyCache::new()),
            observer,
            http,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            demands: Arc::new(Mutex::new(HashSet::new())),
            clock: Arc::new(Timestamp::now),
        })
    }

    /// 禁用态的便捷构造（`None` 基址）。
    ///
    /// # Errors
    ///
    /// 恒 `Ok`（与 [`Client::new`] 的同一句「禁用不是错误」）。
    pub fn disabled() -> Result<Self, ClientError> {
        Self::new(None, None)
    }

    /// 从一个**已校验**的基址串构造。
    ///
    /// ⚠️ **本函数不做校验**：`Url::parse` 不是三件套校验（绝对 URL / 无凭据 / 无
    /// query / fragment —— 那一处只有 `mc_cloud::config::validate` 一份，
    /// `docs/62` §2.2 判据 3）。本仓的组合根拿到的是 `mc_http::state::cloud::EntitlementConfig`
    /// 的 `base_url()`，它**已经**在 `is_valid()` 上过滤过 ⇒ 到达这里的串必已合法。
    /// 之所以要这一层：组合根的 crate（`apps/mc-server`）**不依赖 `url`**，
    /// 也不该为了一个解析动作新增一条依赖边。
    ///
    /// # Errors
    ///
    /// [`ClientError::BaseUrl`]：基址根本解析不成（只有「未校验就传进来」才会发生）。
    pub fn from_validated_base_url(
        raw: &str,
        observer: Option<Arc<dyn Observer>>,
    ) -> Result<Self, ClientError> {
        let base_url = Url::parse(raw).map_err(|_| ClientError::BaseUrl)?;
        Self::new(Some(base_url), observer)
    }

    /// 注入时钟（上游的 `now` 字段；**只**给测试与「把三档时效做成可复现」用）。
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> Timestamp + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// 底层的缓存（诊断与 M9-10 的集成测试用）。
    #[must_use]
    pub fn cache(&self) -> &Arc<PolicyCache> {
        &self.cache
    }

    /// 平面是否真的启用（上游 `Enabled()`）。
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 当前时刻（走注入的时钟）。
    #[must_use]
    pub fn now(&self) -> Timestamp {
        (self.clock)()
    }

    /// 记一笔「这个工作区需要一份策略」（`gate()` 在**没命中**时调用）。
    ///
    /// 非阻塞、幂等（`HashSet`）⇒ 同步路径上**零** IO、零阻塞。
    pub fn request_refresh(&self, workspace_id: Id) {
        if !self.enabled {
            return;
        }
        lock(&self.demands).insert(workspace_id);
    }

    /// 取出并清空当前所有待办需求（组合根的刷新器循环调用）。
    #[must_use]
    pub fn take_demands(&self) -> Vec<Id> {
        if !self.enabled {
            return Vec::new();
        }
        let mut guard = lock(&self.demands);
        let mut pending: Vec<Id> = guard.drain().collect();
        pending.sort_by_key(|workspace_id| workspace_id.0);
        pending
    }

    /// 待办需求数（诊断用）。
    #[must_use]
    pub fn pending_demands(&self) -> usize {
        lock(&self.demands).len()
    }

    /// 把一份**已校验**的策略写进缓存（`refresh` 的写路径，也是测试与替身面的注入口）。
    ///
    /// 返回 [`PutOutcome::VersionRegression`] 时**保留**旧条目（逐字上游 `cache.put`）。
    pub fn store(&self, workspace_id: Id, fetched: &FetchedPolicy) -> PutOutcome {
        self.cache.put(
            workspace_id,
            fetched.snapshot.clone(),
            fetched.valid_for,
            self.now(),
        )
    }

    /// 记一次失败（推进退避期，逐字上游 `markFailure`）。
    pub fn store_failure(&self, workspace_id: Id) {
        let retry_after = mc_core::Timestamp::from_unix(
            self.now().as_unix()
                + i64::try_from(crate::cache::FAILURE_RETRY.as_secs()).unwrap_or(i64::MAX),
        );
        self.cache.mark_failure(workspace_id, retry_after);
    }

    /// 刷新一个工作区（**单飞** + 出站 + 校验 + 写缓存）。
    ///
    /// 逐条对齐上游：3s 超时、64 KiB 响应体上限、不跟随重定向、`Accept: application/json`、
    /// 非 200 ⇒ [`RefreshOutcome::from_status`]、体/JSON/schema 不合法 ⇒
    /// [`RefreshOutcome::InvalidPolicy`]、版本回退 ⇒ [`RefreshOutcome::VersionRegression`]
    /// 且**记一次版本回退事件**。
    pub async fn refresh(&self, workspace_id: Id) -> RefreshOutcome {
        let Some(http) = self.http.as_ref() else {
            return RefreshOutcome::Error;
        };
        let Some(base) = self.base_url.as_ref() else {
            return RefreshOutcome::Error;
        };
        // 单飞：同一工作区已有在飞刷新时不重复发请求。
        if !self.begin_flight(workspace_id) {
            return RefreshOutcome::Error;
        }
        let outcome = self.fetch(http, base, workspace_id).await;
        self.end_flight(workspace_id);
        outcome
    }

    /// 单个请求的完整往返（拆出来是为了让 `refresh` 的单飞结构一眼可见）。
    async fn fetch(&self, http: &reqwest::Client, base: &Url, workspace_id: Id) -> RefreshOutcome {
        let started = Instant::now();
        let mut url = base.clone();
        url.set_path(&format!(
            "{}{}",
            base.path().trim_end_matches('/'),
            policy_endpoint_path(workspace_id)
        ));
        url.set_query(None);
        url.set_fragment(None);

        let response = http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            // 逐字上游 `context.WithTimeout(ctx, c.timeout)`。
            .timeout(crate::cache::REQUEST_TIMEOUT)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                let outcome = if err.is_timeout() {
                    RefreshOutcome::Timeout
                } else if err.is_body() || err.is_decode() {
                    RefreshOutcome::Read
                } else {
                    RefreshOutcome::Network
                };
                self.store_failure(workspace_id);
                self.record_refresh(outcome, started);
                return outcome;
            }
        };
        if response.status() != reqwest::StatusCode::OK {
            let outcome = RefreshOutcome::from_status(response.status().as_u16());
            self.store_failure(workspace_id);
            self.record_refresh(outcome, started);
            return outcome;
        }
        // 体上限：上游是 `io.LimitReader(body, max+1)`；reqwest 的异步体只能整块读
        // ⇒ 先用 `Content-Length` 拒掉已知超限的（登记在 `docs/32`），读完再验一次。
        if response.content_length().is_some_and(|len| {
            usize::try_from(len).map_or(true, |len| len > MAX_RESPONSE_BODY_SIZE)
        }) {
            let outcome = RefreshOutcome::InvalidPolicy;
            self.store_failure(workspace_id);
            self.record_refresh(outcome, started);
            return outcome;
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                let outcome = if err.is_timeout() {
                    RefreshOutcome::Timeout
                } else {
                    RefreshOutcome::Read
                };
                self.store_failure(workspace_id);
                self.record_refresh(outcome, started);
                return outcome;
            }
        };
        if bytes.len() > MAX_RESPONSE_BODY_SIZE {
            let outcome = RefreshOutcome::InvalidPolicy;
            self.store_failure(workspace_id);
            self.record_refresh(outcome, started);
            return outcome;
        }
        self.absorb(workspace_id, parse_and_normalize(&bytes), started)
    }

    /// 把一次 fetch 的产物写进缓存并记指标（成功 / 版本回退两条分支）。
    fn absorb(
        &self,
        workspace_id: Id,
        parsed: Result<FetchedPolicy, InvalidPolicy>,
        started: Instant,
    ) -> RefreshOutcome {
        match parsed {
            Err(_) => {
                let outcome = RefreshOutcome::InvalidPolicy;
                self.store_failure(workspace_id);
                self.record_refresh(outcome, started);
                outcome
            }
            Ok(fetched) => match self.store(workspace_id, &fetched) {
                PutOutcome::Stored => {
                    self.record_refresh(RefreshOutcome::Ok, started);
                    RefreshOutcome::Ok
                }
                PutOutcome::VersionRegression => {
                    // 逐字：回退也要 `markFailure`（否则会形成无退避的重试风暴）。
                    self.store_failure(workspace_id);
                    if let Some(observer) = self.observer.as_ref() {
                        observer.record_entitlement_version_regression();
                    }
                    self.record_refresh(RefreshOutcome::VersionRegression, started);
                    RefreshOutcome::VersionRegression
                }
            },
        }
    }

    fn begin_flight(&self, workspace_id: Id) -> bool {
        lock(&self.in_flight).insert(workspace_id)
    }

    fn end_flight(&self, workspace_id: Id) {
        lock(&self.in_flight).remove(&workspace_id);
    }

    /// 当前在飞的刷新数（诊断用；单飞的证据）。
    #[must_use]
    pub fn in_flight_count(&self) -> usize {
        lock(&self.in_flight).len()
    }

    fn record_cache(&self, outcome: CacheOutcome) {
        if let Some(observer) = self.observer.as_ref() {
            observer.record_entitlement_cache(outcome);
        }
    }

    fn record_refresh(&self, outcome: RefreshOutcome, started: Instant) {
        if let Some(observer) = self.observer.as_ref() {
            observer.record_entitlement_refresh(outcome, started.elapsed().as_secs_f64());
        }
    }

    fn record_decision(&self, name: GateName, decision: &Decision) -> Decision {
        if let Some(observer) = self.observer.as_ref() {
            observer.record_entitlement_decision(
                name.as_str(),
                decision.gate.action,
                decision.reason,
            );
        }
        decision.clone()
    }
}

/// 体解析 + 规范化（拆出来是为了「错误路径不回显云侧响应体」可被单独验证）。
fn parse_and_normalize(body: &[u8]) -> Result<FetchedPolicy, InvalidPolicy> {
    let wire: WirePolicy = serde_json::from_slice(body).map_err(|_| InvalidPolicy)?;
    normalize_policy(&wire)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Provider for Client {
    /// 同步、无 IO 的**缓存**判决（见模块头「两条形状不同的路径」）。
    fn gate(&self, workspace_id: Id, name: GateName) -> Decision {
        if !self.enabled {
            return self.record_decision(name, &off_decision(Reason::Disabled));
        }
        if workspace_id.is_nil() {
            return self.record_decision(name, &off_decision(Reason::InvalidWorkspace));
        }
        if !name.is_valid() {
            return self.record_decision(name, &off_decision(Reason::UnknownGate));
        }
        let now = self.now();
        let cached = self.cache.get(workspace_id);
        if let Some(entry) = cached.as_ref() {
            if entry.is_fresh(now) {
                self.record_cache(CacheOutcome::Hit);
                return self.record_decision(
                    name,
                    &decision_from_entry(entry, name, Reason::CacheFresh, false),
                );
            }
            if entry.is_backing_off(now) {
                self.record_cache(CacheOutcome::RetrySuppressed);
                let decision = if entry.is_stale_usable(now) {
                    decision_from_entry(entry, name, Reason::Stale, true)
                } else {
                    off_decision(Reason::Unavailable)
                };
                return self.record_decision(name, &decision);
            }
            self.record_cache(if entry.has_policy {
                CacheOutcome::Expired
            } else {
                CacheOutcome::Miss
            });
        } else {
            self.record_cache(CacheOutcome::Miss);
        }
        // 未命中：不阻塞、不发请求（模块头的硬纪律）——只记一笔需求，由组合根刷新器兑现。
        self.request_refresh(workspace_id);
        self.record_decision(name, &off_decision(Reason::Unavailable))
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// 上游 `decisionFromEntry`：取 gate + （陈旧时）降级 + 带上审计信息。
fn decision_from_entry(
    entry: &crate::cache::CacheEntry,
    name: GateName,
    reason: Reason,
    stale: bool,
) -> Decision {
    let Some(snapshot) = entry.policy.get() else {
        return off_decision(reason);
    };
    Decision {
        gate: snapshot.gate(name).downgraded_when_stale(stale),
        reason,
        policy_revision: snapshot.policy_revision,
        subscription_version: snapshot.subscription_version,
        cloud_valid_until: snapshot.cloud_valid_until,
    }
}

// ---------------------------------------------------------------------------
// 进程级平面槽（与 `mc_autopilot::quota::install_policy_provider` 平行的一只）
// ---------------------------------------------------------------------------

/// 进程内安装的策略平面（装一次；后装者不覆盖）。
static PROVIDER: OnceLock<Arc<dyn Provider>> = OnceLock::new();

/// 禁用的默认平面（零 IO 的零尺寸常量）。
static DEFAULT_PLANE: DisabledPlane = DisabledPlane;

/// 进程里**没装**平面时的默认实现（等价于 `Client` 的禁用态，但**零分配**）。
#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledPlane;

impl Provider for DisabledPlane {
    fn gate(&self, _workspace_id: Id, _name: GateName) -> Decision {
        off_decision(Reason::Disabled)
    }

    fn is_enabled(&self) -> bool {
        false
    }
}

/// 安装策略平面（**唯一**实现点是组合根 `apps/mc-server/src/entitlement.rs`）。
///
/// 与 `mc_autopilot::quota::install_policy_provider` 是**同一时刻**装上的**同一个** `Arc`
/// ⇒ `GET /api/autopilots/usage` 与 `GET /api/issues/limit-usage` 读的是**同一份**策略
/// （`docs/62` §9.8 判据 ③：「不得出现第二份判定」）。
///
/// 返回 `false` = 已经装过，本次调用被忽略。
pub fn install_provider(provider: Arc<dyn Provider>) -> bool {
    PROVIDER.set(provider).is_ok()
}

/// 当前平面（默认 = 禁用的 [`Client`]，因此**不会** panic）。
#[must_use]
pub fn provider() -> &'static dyn Provider {
    match PROVIDER.get() {
        Some(installed) => installed.as_ref(),
        None => &DEFAULT_PLANE,
    }
}

#[cfg(test)]
mod tests;
