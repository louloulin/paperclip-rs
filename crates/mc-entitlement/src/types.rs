//! entitlement 的**完整类型形状**（上游 `internal/entitlement/types.go`，128 行）。
//!
//! 本文件由 **M9-0 anchor 定形、此后冻结**：M9-9 只填 [`crate::cache`] / [`crate::client`]
//! / [`crate::stub`]，**不改**这里的枚举与结构（改了会让三处消费者各自漂移，
//! `docs/62` §2.1 的「完整类型形状」就是这个意思）。
//!
//! # 三条类型级事实（读一遍能省一次线上事故）
//!
//! 1. **[`Action::Observe`] 不是线上取值**：云侧的 `action` 只有 `off` / `enforce`
//!    （上游 `normalizeGate` 逐字 `case ActionEnforce: default: return ErrInvalidPolicy`）。
//!    `observe` **只**由本地在「策略陈旧」时对 `enforce` 降级产生 ——
//!    见 [`Gate::downgraded_when_stale`]。
//! 2. **`off` 是**唯一**的 fail-open 形状**：`Action::Off` 时其余字段**全 `None`**
//!    （上游 `Gate{Action: ActionOff}`，不填 limit/period/notifications）。
//!    消费者判「是否要拦」时只看 `action`，不要看 `limit` 是否为 `Some`。
//! 3. **`limit` 是 `i64` 而不是 `i32`**：上游 wire 是 `*int`，本仓用 `i64` 与
//!    `mc_autopilot::quota::QuotaPolicy::limit` 对齐（适配器 M9-9 零转换）。

use std::fmt;

use mc_core::{Id, Timestamp};

/// 上游 `entitlement.SchemaVersion`（策略协议的 schema 版本）。
pub const SCHEMA_VERSION: i32 = 1;

/// 上游 `NotificationFirstRejectionPerPeriod`：**唯一**被接受的 `on_rejection` 取值。
pub const NOTIFICATION_FIRST_REJECTION_PER_PERIOD: &str = "first_rejection_per_period";

/// 通用 enforcement point 的名字（上游 `GateName`）。
///
/// 两个取值就是上游 `valid()` 接受的全部（其余 ⇒ [`Reason::UnknownGate`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateName {
    /// `issue_count` —— issue 创建面的配额。
    IssueCount,
    /// `autopilot_runs` —— autopilot 运行面的配额。
    AutopilotRuns,
}

impl GateName {
    /// 全部取值（上游 `normalizePolicy` 逐字遍历这两个）。
    pub const ALL: [Self; 2] = [Self::IssueCount, Self::AutopilotRuns];

    /// 线上字符串（= 上游的 `GateName` 取值）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IssueCount => "issue_count",
            Self::AutopilotRuns => "autopilot_runs",
        }
    }

    /// 解析（未知值 ⇒ `None`；**不** panic）。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "issue_count" => Some(Self::IssueCount),
            "autopilot_runs" => Some(Self::AutopilotRuns),
            _ => None,
        }
    }

    /// 上游 `GateName.valid()`。
    #[must_use]
    pub fn is_valid(self) -> bool {
        Self::ALL.contains(&self)
    }
}

impl fmt::Display for GateName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一个 enforcement point 的**有效指令**（上游 `Action`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Action {
    /// 关掉（= 不拦）—— 也是**唯一的 fail-open 形状**。
    #[default]
    Off,
    /// 观测：记录/通知，但**不**拦。
    ///
    /// ⚠️ **只能由本地产生**（陈旧降级），云侧的 `action=observe` 会被判
    /// [`Reason::InvalidPolicy`]（上游 `normalizeGate` 的 `default` 分支）。
    Observe,
    /// 强制拦截。
    Enforce,
}

impl Action {
    /// 线上字符串 / 指标标签（`RecordEntitlementDecision` 的第二个参数）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }

    /// 解析**线上**取值（只认 `off` / `enforce` —— `observe` 不是线上取值）。
    #[must_use]
    pub fn parse_wire(raw: &str) -> Option<Self> {
        match raw {
            "off" => Some(Self::Off),
            "enforce" => Some(Self::Enforce),
            _ => None,
        }
    }

    /// 是否「会拦」（`enforce` 才是；`observe` 与 `off` 都不拦）。
    #[must_use]
    pub fn is_enforcing(self) -> bool {
        matches!(self, Self::Enforce)
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 判决**为什么**长这样（上游 `Reason`）。
///
/// 上游注释逐字：「carries enough source information for consumers to audit why a gate was
/// enforced, observed, or disabled without exposing Cloud's private inputs」⇒ 每一个取值
/// 都必须能区分「云侧说的」与「本地推的」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// 客户端未启用（`MULTICA_CLOUD_URL` 为空）。
    Disabled,
    /// `uuid.Nil` 工作区（本仓用 [`Id::is_nil`]）。
    InvalidWorkspace,
    /// 未知 gate 名。
    UnknownGate,
    /// 命中**新鲜**缓存。
    CacheFresh,
    /// 本次真的刷新成功了。
    Refreshed,
    /// 缓存**陈旧**但仍在宽限期内（`enforce` 已被降级为 `observe`）。
    Stale,
    /// 陈旧且已过宽限期，或退避期内无策略可用。
    Unavailable,
    /// 云侧响应不合法（schema / 版本 / period 组合 / 体超限 / JSON 坏）。
    InvalidPolicy,
    /// 订阅版本**回退**（本地拒绝降级写缓存）。
    VersionRegression,
    /// 来自替身（[`crate::stub`]），不是真实平面。
    Stub,
}

impl Reason {
    /// 指标标签（`RecordEntitlementDecision` 的第三个参数）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::InvalidWorkspace => "invalid_workspace",
            Self::UnknownGate => "unknown_gate",
            Self::CacheFresh => "cache_fresh",
            Self::Refreshed => "refreshed",
            Self::Stale => "stale",
            Self::Unavailable => "unavailable",
            Self::InvalidPolicy => "invalid_policy",
            Self::VersionRegression => "version_regression",
            Self::Stub => "stub",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 拦截时的**展示**策略（上游 `NotificationPolicy`）。
///
/// 上游注释逐字：通知是**附加的展示**政策 —— 一个坏的通知对象**绝不能**让 enforcement
/// gate 失效（`normalizeNotificationPolicy` 独立丢弃它）。本仓的 `None` 就是「说了但听不懂」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPolicy {
    /// 当前只有 [`NOTIFICATION_FIRST_REJECTION_PER_PERIOD`] 一个合法值。
    pub on_rejection: String,
}

impl NotificationPolicy {
    /// 合法的唯一构造（别的取值上游会丢弃整个对象）。
    #[must_use]
    pub fn first_rejection_per_period() -> Self {
        Self {
            on_rejection: NOTIFICATION_FIRST_REJECTION_PER_PERIOD.to_string(),
        }
    }
}

/// 一个 enforcement point 的有效指令（上游 `Gate`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Gate {
    /// 指令。
    pub action: Action,
    /// 上限（`None` = 没有上限 —— 只有 `off` 时合法）。
    pub limit: Option<i64>,
    /// 周期起点（三个 period 字段**要么全 `None`、要么全 `Some`**，上游 `periodFields` 判据）。
    pub period_start: Option<Timestamp>,
    /// 周期终点。
    pub period_end: Option<Timestamp>,
    /// 额度重置时刻。
    pub reset_at: Option<Timestamp>,
    /// 展示策略（可缺省）。
    pub notifications: Option<NotificationPolicy>,
}

impl Gate {
    /// fail-open 形状：`off` + 其余全 `None`（上游 `Gate{Action: ActionOff}`）。
    #[must_use]
    pub const fn off() -> Self {
        Self {
            action: Action::Off,
            limit: None,
            period_start: None,
            period_end: None,
            reset_at: None,
            notifications: None,
        }
    }

    /// 上游 `decisionFromEntry` 的降级步：**陈旧**策略把 `enforce` 降成 `observe`。
    ///
    /// 逐字：`if stale && gate.Action == ActionEnforce { gate.Action = ActionObserve }`
    /// —— 这是「过期策略还能用于观测、但不能用于拦截」的唯一实现点。
    #[must_use]
    pub fn downgraded_when_stale(mut self, stale: bool) -> Self {
        if stale && self.action == Action::Enforce {
            self.action = Action::Observe;
        }
        self
    }

    /// 三个 period 字段是否**全有或全无**（上游 `periodFields` 判据）。
    #[must_use]
    pub fn period_is_complete_or_absent(&self) -> bool {
        let present = [
            self.period_start.is_some(),
            self.period_end.is_some(),
            self.reset_at.is_some(),
        ]
        .iter()
        .filter(|present| **present)
        .count();
        present == 0 || present == 3
    }
}

/// 一次判决 + 它的来源信息（上游 `Decision`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// 指令。
    pub gate: Gate,
    /// 为什么。
    pub reason: Reason,
    /// 云侧策略修订号（审计用；`0` = 没有策略）。
    pub policy_revision: i64,
    /// 订阅版本号（版本回退的判据）。
    pub subscription_version: i64,
    /// 云侧给的策略有效期（`valid_until`）。
    pub cloud_valid_until: Timestamp,
}

impl Default for Decision {
    fn default() -> Self {
        Self {
            gate: Gate::off(),
            reason: Reason::Disabled,
            policy_revision: 0,
            subscription_version: 0,
            cloud_valid_until: Timestamp::default(),
        }
    }
}

impl Decision {
    /// 上游 `recordDecision` 的审计面：是否**真的会拦**。
    ///
    /// 消费者（M9-9 的适配器）只看这一个谓词，不看 [`Reason`] —— 这样 `stale` 的
    /// `observe` 自动化地变成「不拦」。
    #[must_use]
    pub fn is_enforcing(&self) -> bool {
        self.gate.action.is_enforcing()
    }
}

/// 上游 `offDecision(reason)`：fail-open 判决。
#[must_use]
pub fn off_decision(reason: Reason) -> Decision {
    Decision {
        gate: Gate::off(),
        reason,
        policy_revision: 0,
        subscription_version: 0,
        cloud_valid_until: Timestamp::default(),
    }
}

/// 上游 `Provider`：issue-count 与 autopilot 两个消费者**唯一**需要的接口。
///
/// 上游注释逐字：「It deliberately has no error return: every failure is represented by a
/// fail-open Decision with `ActionOff` (or `ActionObserve` for a bounded stale snapshot that can
/// no longer enforce).」⇒ 本仓同样**不**返回 `Result`。
///
/// 🔴 **同步、无 `ctx`**（与 `mc_autopilot::quota::QuotaPolicyProvider` 同形）：
/// 见本 crate 模块头的偏离登记 1。
pub trait Provider: Send + Sync {
    /// 给 `workspace_id` 的 `name` 判一个 [`Decision`]。
    ///
    /// 实现者必须保证：**同一个工作区在两个消费者眼里看到同一份事实**
    /// （`docs/62` §9.8 的矩阵判据 ③：「不得出现第二份判定」）。
    fn gate(&self, workspace_id: Id, name: GateName) -> Decision;

    /// 平面是否真的启用（上游 `Client.Enabled()`；替身恒 `true`）。
    ///
    /// 默认实现给 `true` —— 只读诊断用，不参与判决。
    fn is_enabled(&self) -> bool {
        true
    }
}

/// 上游 `Observer`：低基数计量口。
///
/// 上游注释逐字：「Implementations must not attach workspace IDs or policy values as metric
/// labels.」⇒ 本仓的四个方法**都不接受 `Id`**（类型层面就堵住了）。
pub trait Observer: Send + Sync {
    /// 缓存结局（[`CacheOutcome`] 的词表）。
    fn record_entitlement_cache(&self, outcome: CacheOutcome);
    /// 刷新结局（[`RefreshOutcome`] 的词表）+ 耗时秒。
    fn record_entitlement_refresh(&self, outcome: RefreshOutcome, duration_seconds: f64);
    /// 判决（gate / action / reason 三个**低基数**字符串）。
    fn record_entitlement_decision(&self, gate: &str, action: Action, reason: Reason);
    /// 版本回退事件（无标签）。
    fn record_entitlement_version_regression(&self);
}

/// 上游 `recordCache(outcome)` 的词表（四个取值逐字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheOutcome {
    /// 命中且新鲜。
    Hit,
    /// 处于失败退避期，抑制了刷新。
    RetrySuppressed,
    /// 有条目但已过新鲜期。
    Expired,
    /// 无条目。
    Miss,
}

impl CacheOutcome {
    /// 指标标签。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::RetrySuppressed => "retry_suppressed",
            Self::Expired => "expired",
            Self::Miss => "miss",
        }
    }
}

/// 上游 `recordRefresh(outcome, …)` 的词表。
///
/// 由 `refreshOutcome(err)`（fetch 类）+ `statusOutcome(status)`（HTTP 类）+
/// `version_regression` 三处汇集而成 —— 本仓把**全部**取值收进一个枚举，
/// 免得 M9-9 自己在两处拼字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefreshOutcome {
    /// 刷新成功。
    Ok,
    /// 请求构建失败。
    Request,
    /// 超时（含 `context.DeadlineExceeded`）。
    Timeout,
    /// 网络错误。
    Network,
    /// 读体失败。
    Read,
    /// 体超限 / JSON 坏 / schema 不合法。
    InvalidPolicy,
    /// 订阅版本回退。
    VersionRegression,
    /// 其余错误。
    Error,
    /// HTTP 401。
    Unauthorized,
    /// HTTP 404。
    NotFound,
    /// HTTP 5xx。
    Status5xx,
    /// HTTP 4xx（非 401/404）。
    Status4xx,
    /// 其余非 200 状态。
    Status,
}

impl RefreshOutcome {
    /// 指标标签（**逐字**取上游的拼写）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Request => "request",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Read => "read",
            Self::InvalidPolicy => "invalid_policy",
            Self::VersionRegression => "version_regression",
            Self::Error => "error",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::Status5xx => "5xx",
            Self::Status4xx => "4xx",
            Self::Status => "status",
        }
    }

    /// 上游 `statusOutcome(status)`：HTTP 状态 → 词表。
    #[must_use]
    pub fn from_status(status: u16) -> Self {
        match status {
            401 => Self::Unauthorized,
            404 => Self::NotFound,
            500..=599 => Self::Status5xx,
            400..=499 => Self::Status4xx,
            _ => Self::Status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_wire_vocabulary_excludes_observe() {
        assert_eq!(Action::parse_wire("off"), Some(Action::Off));
        assert_eq!(Action::parse_wire("enforce"), Some(Action::Enforce));
        // ⚠️ 线上**没有** observe（上游 normalizeGate 的 default 分支 ⇒ InvalidPolicy）。
        assert_eq!(Action::parse_wire("observe"), None);
        assert_eq!(Action::parse_wire(""), None);
        assert_eq!(Action::as_str(Action::Observe), "observe");
        assert_eq!(Action::default(), Action::Off);
    }

    #[test]
    fn gate_name_round_trips_and_rejects_unknown() {
        for name in GateName::ALL {
            assert_eq!(GateName::parse(name.as_str()), Some(name));
            assert!(name.is_valid());
        }
        assert_eq!(GateName::parse("seats"), None);
        assert_eq!(GateName::IssueCount.to_string(), "issue_count");
    }

    #[test]
    fn off_gate_is_the_fail_open_shape() {
        let gate = Gate::off();
        assert_eq!(gate.action, Action::Off);
        assert!(gate.limit.is_none());
        assert!(gate.period_start.is_none() && gate.period_end.is_none());
        assert!(gate.reset_at.is_none());
        assert!(gate.notifications.is_none());
        assert!(gate.period_is_complete_or_absent());
        assert!(!Decision::default().is_enforcing());
    }

    #[test]
    fn stale_downgrades_enforce_to_observe_but_leaves_off_alone() {
        let mut gate = Gate::off();
        gate.action = Action::Enforce;
        gate.limit = Some(50);
        gate.period_start = Some(Timestamp::default());
        gate.period_end = Some(Timestamp::default());
        gate.reset_at = Some(Timestamp::default());
        assert!(gate.period_is_complete_or_absent());
        let stale = gate.clone().downgraded_when_stale(true);
        assert_eq!(stale.action, Action::Observe);
        assert_eq!(stale.limit, Some(50), "降级只改 action，不改额度");
        // 新鲜时不动。
        assert_eq!(
            gate.clone().downgraded_when_stale(false).action,
            Action::Enforce
        );
        // `off` 本来就是不拦 ⇒ 降级是恒等（不会凭空变成 observe）。
        assert_eq!(Gate::off().downgraded_when_stale(true).action, Action::Off);
    }

    #[test]
    fn partial_period_is_detectable() {
        let mut gate = Gate::off();
        gate.period_start = Some(Timestamp::default());
        assert!(!gate.period_is_complete_or_absent());
        gate.period_end = Some(Timestamp::default());
        assert!(!gate.period_is_complete_or_absent());
        gate.reset_at = Some(Timestamp::default());
        assert!(gate.period_is_complete_or_absent());
    }

    #[test]
    fn observation_vocabularies_are_low_cardinality_strings() {
        assert_eq!(CacheOutcome::RetrySuppressed.as_str(), "retry_suppressed");
        assert_eq!(
            RefreshOutcome::from_status(401),
            RefreshOutcome::Unauthorized
        );
        assert_eq!(RefreshOutcome::from_status(404), RefreshOutcome::NotFound);
        assert_eq!(RefreshOutcome::from_status(503), RefreshOutcome::Status5xx);
        assert_eq!(RefreshOutcome::from_status(418), RefreshOutcome::Status4xx);
        assert_eq!(RefreshOutcome::from_status(302), RefreshOutcome::Status);
        assert_eq!(
            RefreshOutcome::VersionRegression.as_str(),
            "version_regression"
        );
        assert_eq!(Reason::VersionRegression.as_str(), "version_regression");
        assert_eq!(
            NotificationPolicy::first_rejection_per_period().on_rejection,
            NOTIFICATION_FIRST_REJECTION_PER_PERIOD
        );
        assert_eq!(SCHEMA_VERSION, 1);
        // 十个 Reason 全部有标签（漏一个会被这里抓到）。
        let all = [
            Reason::Disabled,
            Reason::InvalidWorkspace,
            Reason::UnknownGate,
            Reason::CacheFresh,
            Reason::Refreshed,
            Reason::Stale,
            Reason::Unavailable,
            Reason::InvalidPolicy,
            Reason::VersionRegression,
            Reason::Stub,
        ];
        assert_eq!(all.len(), 10);
        for reason in all {
            assert!(!reason.as_str().is_empty());
        }
    }
}
