//! M9 anchor（`LUM-1815`）建桩、**M9-9（`LUM-1824`）原地填充**：entitlement 平面的
//! **组合根适配器**。
//!
//! # 这一片解决什么问题
//!
//! 本仓**已有**一个为这件事准备的接缝：`crates/mc-autopilot/src/quota.rs` 的
//! `QuotaPolicyProvider` trait + `install_policy_provider`（M5-1 落下），模块头逐字写着
//! 「M9/云侧装自己的实现即可让同一批调用点变成按工作区下发策略」。
//! `mc-entitlement` 提供 `Provider`/`Decision`/`Stub`；**适配器**（把云侧策略翻译成本仓的
//! `QuotaPolicy` 并安装）就只能在这里 —— `apps/mc-server` 是唯一同时看得见两者的地方
//! （`docs/62` §9.8 的裁定：不落 `mc-authz`、不并进 `mc-cloud`）。
//!
//! # 三个挂载点，**一个** `Arc`
//!
//! | 消费者 | 读什么 | 上游的形状 |
//! | --- | --- | --- |
//! | `GET /api/autopilots/usage` + M5-4 的准入 | [`mc_autopilot::quota::policy_for`] | `service/autopilot_quota.go` 的 `quotaPolicy` |
//! | `GET /api/issues/limit-usage` | [`mc_entitlement::client::provider`] 的 `gate(ws, IssueCount)` | `handler/issue_limit.go` 的 `ResolveIssueCountPolicy` |
//!
//! 🔴 两个槽装的是**同一个** `Arc<EntitlementPlane>` ⇒ 两个消费者永远看到同一份缓存、
//! 同一份判决（`docs/62` §9.8 判据 ③：「不得出现第二份判定」）。这不是靠约定维持的，
//! 是靠 [`mc_autopilot::quota::install_policy_provider`] 与
//! [`mc_entitlement::client::install_provider`] **都是进程级 `OnceLock`、且只装一次**。
//!
//! # 三态（本表就是 `start` 的行为表）
//!
//! | 部署状态 | 行为 | 配额面 | `limit-usage` |
//! | --- | --- | --- | --- |
//! | 未配 / 空 / 全空白 `MULTICA_CLOUD_URL` | `info` 一条，**不装平面**（正常路径） | 恒 `off` | **204** |
//! | 非空但非法 | `warn` 一条，**不装平面**（云代理面给 500） | 恒 `off` | **204** |
//! | 合法 | 装平面 + 起刷新器 | 按云侧策略 | 按云侧策略 |
//!
//! # 刷新器：为什么这里有一个「后台生命周期」
//!
//! 上游逐字「It has no goroutines or background lifecycle」—— 因为上游的 `Gate` **自己**
//! 同步发请求。本仓的 [`mc_entitlement::Provider::gate`] 是**同步、无 IO** 的（被
//! `mc_autopilot::quota::QuotaPolicyProvider` 的契约与冻结的类型形状同时要求），
//! 它只**记一笔需求**；兑现需求的那个任务落在本组合根。这正是本文件 `shutdown` 注释里
//! 预告的「刷新节流器」—— 停机链**不需要**因此改写。
//!
//! 🔴 **不得**在 `AppState::new` 里装平面（`install_policy_provider` 是**进程级一次性**，
//! 后装者被忽略）⇒ 装机点唯一且显式 = `main.rs` 的第 8.5 步。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mc_autopilot::quota::{QuotaAction, QuotaPolicy, QuotaPolicyProvider};
use mc_core::Id;
use mc_entitlement::client::Client;
use mc_entitlement::{Action, Decision, Gate, GateName, Provider, Reason};
use mc_http::state::cloud::EntitlementConfig;

#[cfg(test)]
mod tests;

/// 刷新器的轮询间隔（**不是** TTL/退避常量 —— 那三个数字由 `mc-entitlement::cache` 定死）。
///
/// 它的唯一作用是「把同步调用记下的需求尽快兑现」；定得太密只是白跑一次出站，
/// 定得太疏则只是让首次 `limit-usage` 多等一会儿。1s 与上游的 5s 失败退避同量级。
const REFRESH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// entitlement 面的装配结果。
pub struct EntitlementHandles {
    /// 云基址是否已配置且合法（⇒ 平面会被装上）。
    configured: bool,
    /// 平面是否**真的**装进了进程。
    wired: bool,
    /// 装上的平面（`None` ⇒ 没装）。
    // ⚠️ `dead_code`：`apps/mc-server` 是**二进制** crate，`pub` 不能豁免 dead_code，
    // 而本片没有第二个调用方（`main.rs` 是只读面）。删除时请连带删掉
    // [`EntitlementHandles::plane`] 与用例里对它的断言。
    #[allow(dead_code)]
    plane: Option<Arc<EntitlementPlane>>,
    /// 刷新器的停止信号 + 收尾句柄。
    refresher: Option<Refresher>,
}

/// 刷新器的生命周期句柄（`main.rs` 的停机链通过 [`EntitlementHandles::shutdown`] 收它）。
#[derive(Debug)]
struct Refresher {
    stop: Arc<AtomicBool>,
    joined: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for EntitlementHandles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EntitlementHandles")
            .field("configured", &self.configured)
            .field("wired", &self.wired)
            .field("plane", &self.plane.is_some())
            .field("refresher", &self.refresher.is_some())
            .finish()
    }
}

impl EntitlementHandles {
    /// 云基址是否已配置且合法。
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// 策略平面是否已安装（`false` ⇒ quota 恒 `off`、`limit-usage` 恒 204）。
    #[must_use]
    pub fn is_wired(&self) -> bool {
        self.wired
    }

    /// 装上的平面（`None` ⇒ 没装；给诊断与测试用）。
    #[must_use]
    #[allow(dead_code)]
    pub fn plane(&self) -> Option<Arc<EntitlementPlane>> {
        self.plane.clone()
    }

    /// 停机：停掉刷新器（平面本身没有别的后台生命周期）。
    ///
    /// 停机链（`main.rs` 第 8.5 步之后）**不需要**为刷新器改写 —— 这正是锚点期
    /// 保留这个 `async` 空签名时预告的那一天。
    pub async fn shutdown(mut self) {
        if let Some(mut refresher) = self.refresher.take() {
            refresher.stop.store(true, Ordering::SeqCst);
            if let Some(joined) = refresher.joined.take() {
                let _ = joined.await;
            }
        }
    }
}

/// 组合根适配器：把 [`mc_entitlement::Provider`] 翻译成 [`QuotaPolicyProvider`]。
///
/// `policy()` 是**同步、无 IO** 的（逐字满足接缝的契约）：它只读缓存，冷启动时答 `None`
/// （= 上游的 `quotaPolicy` 在「门关着」时的形状，**不**读配额表）。
#[derive(Debug)]
pub struct EntitlementPlane {
    client: Arc<Client>,
}

impl EntitlementPlane {
    /// 用一个**已构造**的客户端装一个平面（生产与测试走同一条路）。
    #[must_use]
    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }

    /// 底层的策略客户端（`limit-usage` 面与诊断读的是**同一个**它）。
    #[must_use]
    #[allow(dead_code)]
    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    /// issue-count 面的判决（上游 `service.ResolveIssueCountPolicy` 的输入那一半）。
    ///
    /// 与 [`QuotaPolicyProvider::policy`] 走**同一个** [`Provider::gate`] 调用面
    /// ⇒ 同一格里两个消费者不可能给出两个不同结论。
    #[must_use]
    #[allow(dead_code)]
    pub fn issue_count(&self, workspace_id: Id) -> Decision {
        self.client.gate(workspace_id, GateName::IssueCount)
    }

    /// 刷新器的一轮：兑现当前所有待办需求（`None` ⇒ 没人要刷新）。
    ///
    /// 逐个串行（上游的 singleflight 是**按工作区**的，而待办集合本身已去重）。
    pub async fn serve_demands(&self) {
        for workspace_id in self.client.take_demands() {
            self.client.refresh(workspace_id).await;
        }
    }
}

impl QuotaPolicyProvider for EntitlementPlane {
    /// 逐字复刻上游 `service/autopilot_quota.go` 的 `quotaPolicy`：
    ///
    /// - `off` ⇒ `None`（**且不读配额表** —— 注释里点名的 fail-open 分支）；
    /// - 形状不合法（`limit` 缺失/为负、period 三字段不齐、`period_start >= period_end`）
    ///   ⇒ 同样 `None`（`None` 是唯一的 fail-open 形状，绝不**编**一个策略出来）；
    /// - 否则给出 `observe` / `enforce` + 周期 + 审计用的两个版本号。
    fn policy(&self, workspace_id: Id) -> Option<QuotaPolicy> {
        if workspace_id.is_nil() {
            return None;
        }
        let decision = self.client.gate(workspace_id, GateName::AutopilotRuns);
        quota_policy_from(&decision)
    }
}

impl Provider for EntitlementPlane {
    /// 与 [`QuotaPolicyProvider::policy`] **同一个**调用面（`client.gate`）——
    /// 两个消费者共用一个 `Arc` 的收益就在这里兑现。
    fn gate(&self, workspace_id: Id, name: GateName) -> Decision {
        self.client.gate(workspace_id, name)
    }

    fn is_enabled(&self) -> bool {
        self.client.is_enabled()
    }
}

/// `Decision` → [`QuotaPolicy`]（**唯一**的翻译点，两个 autopilot 消费者共用它）。
#[must_use]
pub fn quota_policy_from(decision: &Decision) -> Option<QuotaPolicy> {
    let gate: &Gate = &decision.gate;
    if gate.action == Action::Off {
        return None;
    }
    let action = match gate.action {
        Action::Observe => QuotaAction::Observe,
        Action::Enforce => QuotaAction::Enforce,
        Action::Off => return None,
    };
    let limit = gate.limit.filter(|value| *value >= 0)?;
    let (period_start, period_end, reset_at) =
        (gate.period_start?, gate.period_end?, gate.reset_at?);
    if period_start >= period_end {
        return None;
    }
    Some(QuotaPolicy {
        action,
        limit,
        period_start,
        period_end,
        reset_at,
        policy_revision: decision.policy_revision,
        subscription_version: decision.subscription_version,
    })
}

/// `Decision` → issue-count 面的 `(是否强制限额, 上限)`。
///
/// 逐字上游 `ResolveIssueCountPolicy`：`off` ⇒ `(false, 0)`；`enforce` 且 `limit > 0`
/// ⇒ `(true, limit)`；其余（`observe`、缺 limit、`limit == 0`）**全部**折成 `(false, 0)`
/// —— 上游注释逐字「never infers unlimited access from cache or refresh reasons」。
#[must_use]
#[allow(dead_code)]
pub fn issue_limit_from(decision: &Decision) -> Option<i64> {
    if decision.reason == Reason::Stub {
        // 替身面的判决不得驱动生产路由（模块头第 1 条禁令的运行时兑现）。
        return None;
    }
    if decision.gate.action != Action::Enforce {
        return None;
    }
    decision.gate.limit.filter(|limit| *limit > 0)
}

/// 装配 entitlement 平面（`main.rs` 在**第 8 步之后、调度器之前**调用）。
///
/// `config` = `AppState::entitlement`（云基址的部署事实）。
///
/// **不**返回错误：配置缺失/非法都是**正常**部署形态，不是启动失败
/// （与 `channels::start` / `integrations::start` 同判例）。
pub fn start(config: &EntitlementConfig) -> EntitlementHandles {
    let Some(raw_base) = config.base_url() else {
        if config.settings().is_misconfigured() {
            tracing::warn!(
                "MULTICA_CLOUD_URL is set but invalid; the entitlement plane stays NOT \
                 installed (quota judgements return the off shape, and \
                 /api/issues/limit-usage keeps 204)"
            );
        } else {
            tracing::info!(
                "MULTICA_CLOUD_URL is not configured; entitlement plane stays off \
                 (quota judgements return the off shape, and /api/issues/limit-usage keeps 204)"
            );
        }
        return EntitlementHandles {
            configured: false,
            wired: false,
            plane: None,
            refresher: None,
        };
    };

    let client = match Client::from_validated_base_url(raw_base, None) {
        Ok(client) => Arc::new(client),
        Err(error) => {
            // 出站客户端构造失败 = **启动期**问题（TLS 后端等），但仍不 panic：
            // 与上面同判例，装不上就退化成「平面没装」。
            tracing::warn!(error = %error, "entitlement HTTP client could not be built; \
                 the plane stays NOT installed");
            return EntitlementHandles {
                configured: true,
                wired: false,
                plane: None,
                refresher: None,
            };
        }
    };
    let plane = Arc::new(EntitlementPlane::new(Arc::clone(&client)));
    // 同一个 Arc 装进两个槽 ⇒ 两个消费者同一份策略（模块头「三个挂载点」）。
    let installed_autopilot = mc_autopilot::quota::install_policy_provider(
        Arc::clone(&plane) as Arc<dyn QuotaPolicyProvider>
    );
    let installed_entitlement =
        mc_entitlement::client::install_provider(Arc::clone(&plane) as Arc<dyn Provider>);
    if !installed_autopilot || !installed_entitlement {
        // OnceLock 只装一次 ⇒ 第二个调用者拿到的是别人的平面。**不**覆盖、不告警成
        // 「装好了」—— 诚实报出「本次没装上」。
        tracing::warn!(
            installed_autopilot,
            installed_entitlement,
            "an entitlement plane was already installed in this process; this call is a no-op"
        );
    }
    let refresher = spawn_refresher(Arc::clone(&plane));
    tracing::info!(
        endpoint_prefix = mc_entitlement::client::POLICY_ENDPOINT_PREFIX,
        timeout_secs = mc_entitlement::cache::REQUEST_TIMEOUT.as_secs(),
        "entitlement policy plane installed"
    );
    EntitlementHandles {
        configured: true,
        // 「装上了」= 两个槽都接受了本次的平面。
        wired: installed_autopilot && installed_entitlement,
        plane: Some(plane),
        refresher,
    }
}

/// 起刷新器；**没有** tokio 运行时时（纯同步的测试上下文）安静地不起。
fn spawn_refresher(plane: Arc<EntitlementPlane>) -> Option<Refresher> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let joined = handle.spawn(async move {
        while !flag.load(Ordering::SeqCst) {
            plane.serve_demands().await;
            tokio::time::sleep(REFRESH_POLL_INTERVAL).await;
        }
        // 最后一轮：把停机前记下的需求兑现掉，避免停机瞬间丢一份策略。
        plane.serve_demands().await;
    });
    Some(Refresher {
        stop,
        joined: Some(joined),
    })
}
