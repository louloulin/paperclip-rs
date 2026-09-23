//! Autopilot 领域类型（M5-0 anchor **重写**，不是扩充）。
//!
//! 类型来源 = `migrations/upstream/**` 的**真实列** + `contracts/upstream-schema.json`
//! （后者是 560 个上游迁移灌进真库后的 `pg_get_*` 快照，是最终形态的权威副本）。
//! 本文件覆盖 W5 的 autopilot 面 7 组实体：
//!
//! | 实体 | 表 | 建表迁移 | 追加迁移 | 列数 |
//! | --- | --- | --- | --- | ---: |
//! | [`Autopilot`] | `autopilot` | `042` | `043` / `058` / `096` / `097` / `251` | 16 |
//! | [`AutopilotTrigger`] | `autopilot_trigger` | `042` | `110` / `189` / `449` | 19 |
//! | [`AutopilotRun`] | `autopilot_run` | `042` | `043` / `079` / `096` / `124` / `176` / `352` | 18 |
//! | [`WebhookDelivery`] | `webhook_delivery` | `093` | `176` / `352`（另有纯索引迁移 `177` / `178` / `357`） | 28 |
//! | [`AutopilotCollaborator`] | `autopilot_collaborator` | `128` | — | 5 |
//! | [`AutopilotSubscriber`] | `autopilot_subscriber` | `120` | — | 4 |
//! | [`AutopilotRuleVersion`] | `autopilot_rule_version` | `186` | — | 7 |
//!
//! 配额面另有 `352` 建的两张表，类型在 [`crate::autopilot_quota`]；wakeup 面在
//! [`crate::wakeup`]。**本波 0 新迁移**：上面 12 张表（含 wakeup 的 2 张）全部已在上游。
//!
//! # 旧 stub 错在哪（重写前的实测）
//!
//! 重写前的 `autopilot.rs`（85 行）字段名与语义大面积不对，**不要再参考**：
//!
//! | 旧 stub | 真值 |
//! | --- | --- |
//! | `name` | `title` |
//! | `enabled: bool` | `status ∈ {active,paused,archived}` + `pause_reason` |
//! | `squad_id` | `assignee_type ∈ {agent,squad}` + `assignee_id`（二态被压成一态） |
//! | `trigger` / `cron_expression` / `webhook_url` / `trigger_event_filters` | **不属于本表** ⇒ [`AutopilotTrigger`] |
//! | `rule_version` / `rule` | **不属于本表** ⇒ [`AutopilotRuleVersion`] |
//! | `AutopilotRun.status` 是自由 `String`，注释写 `queued/running/success` | `CHECK` 是 `issue_created/running/completed/failed/skipped`（`043` 定，`079` 补回 `skipped`） |
//! | `AutopilotRun.started_at` / `finished_at` | `triggered_at` / `completed_at` |
//! | `AutopilotRun` 缺 `source` / `trigger_id` / `issue_id` / `squad_id` / `squad_id` 等 9 列 | 见 [`AutopilotRun`] |
//!
//! ## ⚠️ 与 `docs/44-M5-PLAN.md` §5.1 对照表的**两处偏差**（以本文件为准）
//!
//! §5.1 写着「`priority` 缺」「`concurrency_policy` 缺」——那是只读了建表迁移 `042`、
//! 没读后续 alter 的结论。实测真值：
//!
//! - `priority`：`042:10` 建了，**`058_drop_autopilot_priority_and_project_id` 已 DROP**
//!   （理由：与 agent 自己任务队列的优先级重复）。
//! - `concurrency_policy ∈ {skip,queue,replace}`：`042:17` 建了，
//!   **`043_fix_orphaned_autopilot_runs` 已 DROP**（理由：`skip` 有孤儿 run bug，整个特性被拆掉）。
//! - `project_id`：`058` 与 `priority` 一起 DROP，**`097` 又加回来了**（所以它在，且可空）。
//!
//! ⇒ 结论：`autopilot` 表的列**就是**上面表里那 16 个，**没有** `priority`、**没有**
//! `concurrency_policy`。复算命令见 `docs/44` §10；判据是 `contracts/upstream-schema.json`
//! 里 `autopilot.*` 的 column 对象恰好 16 个。
//!
//! 「跳过派发」（旧 `shouldSkipDispatch` 吃 `concurrency_policy`）在 `043` 之后改由
//! **in-flight `autopilot_run` 检查**承担 ⇒ 这是 M5-2/M5-5 的行为面，不是类型面。
//!
//! # 约定
//!
//! - 每个封闭枚举的 [`as_str`](AutopilotStatus::as_str) 取值 = 对应 DB `CHECK` 的取值集合；
//!   改这里必须同步改迁移（本波不改迁移）。
//! - `Timestamp` 一律 UTC 语义（库列是 `timestamptz`）；`timezone` 只是**调度用的 IANA 名**，
//!   不改变存储语义。
//! - `assignee_id` 在 `096` 之后**没有外键**（可以是 agent 也可以是 squad）；
//!   `assignee_type` 决定它指向哪张表。`autopilot_run.squad_id` 是运行时的旁路记录。
//! - `webhook_token` / `signing_secret` 是凭据：**只经 `mc-telemetry::redact` 出日志与响应**。
//! - `*_type` 列的取值是 `member | agent`，与 `wakeup` / `issue` 的 `creator_type` 词汇表一致
//!   （注意：`mc-repos` 的 M2 迁移 `0004` 对本仓自建表用的是 `user | agent`，那是**另一张表**的事）。
//! - **哪些列是「无 CHECK 的约定词汇表」**（绑定/解码时不要当封闭枚举断言）：
//!   `autopilot_trigger.created_by_type` / `published_by_type`（`449`/`189`：无 CHECK、无 FK，
//!   完整性在应用层）、`autopilot_rule_version.published_by_type`（`186`：无 CHECK），
//!   以及自由文本的 `autopilot_run.reason_code` / `webhook_delivery.reason_code`。
//!   本文件仍用 [`AutopilotActorType`] 表达它们（上游就写这两个值），但从库里读到别的值
//!   **不能当不可能事件**——解码要么回退要么报错，由 repos 层决定（M5-1/M5-2）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// `autopilot_trigger.kind` / `autopilot_run.source` 的**共同上游词汇表**
/// （`schedule | webhook | api`，`run.source` 另有 `manual`）。
///
/// 本仓把它拆成两个枚举（[`TriggerKind`] / [`RunSource`]），因为它们各自有独立 `CHECK`；
/// 合并成一个会丢掉 `manual` 这个只属于 run 的取值。
pub const ACTOR_TYPES: [&str; 2] = ["member", "agent"];

// --------------------------------------------------------------------------- //
// 枚举（取值逐字来自 DB CHECK 约束）
// --------------------------------------------------------------------------- //

/// `autopilot.status` ∈ `{active,paused,archived}`（`042` 建 + `251` 补 `pause_reason`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotStatus {
    /// 正常参与调度。
    Active,
    /// 暂停（`pause_reason` 说明原因；自动暂停监视器也写这里）。
    Paused,
    /// 归档：不再调度，但历史 run / delivery 保留。
    Archived,
}

impl AutopilotStatus {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Archived => "archived",
        }
    }
}

/// `autopilot.assignee_type` ∈ `{agent,squad}`（`096`；同迁移去掉 `assignee_id` 的外键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotAssigneeType {
    /// `assignee_id` 指向 `agent`。
    Agent,
    /// `assignee_id` 指向 `squad`。派单仍解析成**单个 agent**（`squad.leader_id`，
    /// 「squad-as-leader」），所以 run 的 `squad_id` 只是归因旁路，执行与成本算在 leader 上。
    Squad,
}

impl AutopilotAssigneeType {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Squad => "squad",
        }
    }
}

/// `member | agent` 主体类型（`autopilot.created_by_type`、`autopilot_trigger.created_by_type` /
/// `published_by_type`、`autopilot_rule_version.published_by_type`、
/// `issue_wakeup.filter_actor_type` 共用同一词汇表）。
///
/// 注意 `autopilot_collaborator` / `autopilot_subscriber` 的 `user_type` **不属于**本词汇表，
/// 见 [`AutopilotUserType`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotActorType {
    /// 工作区成员。
    Member,
    /// 工作区内的 agent。
    Agent,
}

impl AutopilotActorType {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Agent => "agent",
        }
    }

    /// 只有 `member` 能当 run 主体（`449` 列注释：`Only \'member\' yields a run principal`）。
    #[must_use]
    pub const fn yields_run_principal(self) -> bool {
        matches!(self, Self::Member)
    }
}

/// `autopilot.execution_mode` ∈ `{create_issue,run_only}`（决定派单分支：建 issue 还是只跑）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotExecutionMode {
    /// 先建 issue，再把 issue 派给 assignee。
    CreateIssue,
    /// 不建 issue，直接按 assignee 跑一次。
    RunOnly,
}

impl AutopilotExecutionMode {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateIssue => "create_issue",
            Self::RunOnly => "run_only",
        }
    }
}

/// `autopilot_collaborator.user_type` / `autopilot_subscriber.user_type`（member-only）。
///
/// `CHECK (user_type IN ('member'))`（`120`:10 / `128`:13，原文「Members-only for now
/// (broaden the CHECK to expand)」）——**不是** [`AutopilotActorType`]。
/// 单变体枚举是**故意**的：让「这里不可能出现 agent」在类型层成立，不靠注释约束。
///
/// 两张表都是**无外键、无级联**（应用层保证）：autopilot 存在性与成员资格
/// 都在 API 边界校验，删 autopilot 时由 handler 在同一事务里清行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotUserType {
    /// 工作区成员。
    Member,
}

impl AutopilotUserType {
    /// DB `CHECK` 的唯一取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
        }
    }
}

/// `autopilot_trigger.kind` ∈ `{schedule,webhook,api}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// 按 `cron_expression` + `timezone` 定时（`next_run_at` 由调度器推进）。
    Schedule,
    /// 入站 webhook（`webhook_token` 做路径凭据，写 `webhook_delivery` 行）。
    Webhook,
    /// 显式 API 调用（`POST /api/autopilots/{id}/trigger`）。
    Api,
}

impl TriggerKind {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Webhook => "webhook",
            Self::Api => "api",
        }
    }
}

/// `autopilot_trigger.provider` / `webhook_delivery.provider` ∈ `{generic,github}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookProvider {
    /// 通用载体：签名走 `generic` 约定。
    Generic,
    /// `github` 载体：签名走 `X-Hub-Signature-256`。
    Github,
}

impl WebhookProvider {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Github => "github",
        }
    }
}

/// `autopilot_run.status` ∈ `{issue_created,running,completed,failed,skipped}`。
///
/// 演进：`042` 建表时是 6 态 `{pending,issue_created,running,skipped,completed,failed}`
/// ⇒ `043` 去掉 `pending` 与 `skipped`（存量行迁成 `failed`）⇒ `079` 又把 `skipped`
/// 加回来。理由（`079` 原文）：离线 runtime 的准入闸需要一个**非失败**的终态来记录
/// 「有意不派发」，复用 `failed` 会污染驱动自动暂停的失败率信号。`skipped` 是终态，
/// 所以 `043` 建的 in-flight 偏索引（只覆盖 `issue_created`/`running`）不用改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotRunStatus {
    /// issue 已建，任务尚未入队/开跑。
    IssueCreated,
    /// 任务已交给 runtime。
    Running,
    /// 正常结束。
    Completed,
    /// 失败（`failure_reason` / `reason_code` 记录原因）。
    Failed,
    /// 主动不派发（例如 assignee 的 runtime 离线）——**终态，不算失败**。
    Skipped,
}

impl AutopilotRunStatus {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IssueCreated => "issue_created",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// 是否终态（`issue_created` / `running` 是 in-flight；`043` 建的偏索引就只覆盖这两个）。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }
}

/// `autopilot_run.source` ∈ `{schedule,manual,webhook,api}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSource {
    /// 调度器按 `next_run_at` 触发。
    Schedule,
    /// 用户显式触发（`POST /api/autopilots/{id}/trigger`）。
    Manual,
    /// 入站 webhook 触发（`trigger_id` / `webhook_delivery_id` 会一起写）。
    Webhook,
    /// 其它 API 路径触发。
    Api,
}

impl RunSource {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Manual => "manual",
            Self::Webhook => "webhook",
            Self::Api => "api",
        }
    }
}

/// `webhook_delivery.status` ∈ `{queued,dispatched,rejected,ignored,failed}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// 已落库，等待 worker（`available_at` + `lease_token` 是出队条件）。
    Queued,
    /// 已派发（`autopilot_run_id` 填上）。
    Dispatched,
    /// 拒绝（签名/配额/形状不合规）。
    Rejected,
    /// 有意忽略（例如事件不在 `event_filters` 里）。
    Ignored,
    /// 派发尝试失败（`attempt_count` / `error` 记录）。
    Failed,
}

impl DeliveryStatus {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::Rejected => "rejected",
            Self::Ignored => "ignored",
            Self::Failed => "failed",
        }
    }
}

/// `webhook_delivery.signature_status` ∈ `{not_required,valid,invalid,missing}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    /// 该 trigger 没配 `signing_secret`。
    NotRequired,
    /// 签名校验通过。
    Valid,
    /// 签名存在但不匹配。
    Invalid,
    /// 配了 secret 但请求没带签名头。
    Missing,
}

impl SignatureStatus {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Missing => "missing",
        }
    }
}

// --------------------------------------------------------------------------- //
// 实体
// --------------------------------------------------------------------------- //

/// `autopilot` 行（16 列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Autopilot {
    /// 主键。
    pub id: Id,
    /// 所属工作区（级联删除）。
    pub workspace_id: Id,
    /// 可选项目（`058` DROP、`097` 加回；`ON DELETE SET NULL`）。
    pub project_id: Option<Id>,
    /// 名称（旧 stub 的 `name` 是错的）。
    pub title: String,
    /// 描述。
    pub description: Option<String>,
    /// 指派对象类型：`agent` 或 `squad`。
    pub assignee_type: AutopilotAssigneeType,
    /// 指派对象 id（`096` 之后无外键，指向由 `assignee_type` 决定）。
    pub assignee_id: Id,
    /// 三态状态（`042` 的两态 `enabled: bool` 是错的）。
    pub status: AutopilotStatus,
    /// 暂停原因（`251` 加；`status = paused` 时有值）。
    pub pause_reason: Option<String>,
    /// 执行模式：建 issue 还是只跑（决定派单分支）。
    pub execution_mode: AutopilotExecutionMode,
    /// issue 标题模板（`create_issue` 模式用；支持占位符，见上游 `service/autopilot.go`）。
    pub issue_title_template: Option<String>,
    /// 创建者类型（越权判定要用）。
    pub created_by_type: AutopilotActorType,
    /// 创建者 id。
    pub created_by_id: Id,
    /// 最近一次 run 的时间（列表页展示 + 节流判定）。
    pub last_run_at: Option<Timestamp>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 更新时间。
    pub updated_at: Timestamp,
}

/// `autopilot_trigger` 行（19 列）。
///
/// `created_by_*`（`449`）与 `published_by_*`（`189`）是**两套**主体，不要混用：
///
/// - `published_by_*` = 「当前对这份**生效配置**负责的人」，会随实质性编辑**转移**（`189`/`MUL-4302`），
///   自 `MUL-6951` 起**只是 CONFIG 审计**，不再决定 run 的授权。
/// - `created_by_*` = **不可变**创建者（创建时写一次，编辑路径永不改写）；自 `MUL-6951` 起
///   schedule/webhook run 就是**以这个人的身份**发起（准入、任务 originator、后续委派都解析到它）。
///   注意：只有 `member` 才产生 run 主体（`agent` 不产生）。
///
/// `449` 的回填是**明知不精确**的 best-effort：`published_by` 在时照抄，因此只被编辑过的触发器
/// 会冻结「最后一个可恢复的编辑者」；`189` 之前的老行两者皆为 `NULL` ⇒ **没有可证明的主体**，
/// 派发必须 **fail-closed**（记录 `failure_reason`），而且**故意没有恢复路径**（重新保存只会再戳
/// `published_by`）。这是 M5-3 的行为面，类型面只需知道两边都可空。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutopilotTrigger {
    /// 主键。
    pub id: Id,
    /// 所属 autopilot（级联删除）。
    pub autopilot_id: Id,
    /// 触发类型。
    pub kind: TriggerKind,
    /// webhook 载体。
    pub provider: WebhookProvider,
    /// 展示名。
    pub label: Option<String>,
    /// 是否启用（触发器级别；与 `autopilot.status` 正交）。
    pub enabled: bool,
    /// 5 字段 cron（`Minute|Hour|Dom|Month|Dow`，**无秒**）；`kind = schedule` 时有值。
    pub cron_expression: Option<String>,
    /// 调度时区（IANA 名，如 `Asia/Shanghai`）；可空 ⇒ 按 `UTC` 解释。
    pub timezone: Option<String>,
    /// 事件过滤（`110` 加的 `event_filters`，`jsonb`）。
    pub event_filters: Option<serde_json::Value>,
    /// 入站 webhook 的路径凭据（**凭据**：只经 `mc-telemetry` 出日志）。
    pub webhook_token: Option<String>,
    /// 签名密钥（**凭据**，同上）。
    pub signing_secret: Option<String>,
    /// 下次触发时间（调度器读写）。
    pub next_run_at: Option<Timestamp>,
    /// 最近一次触发时间。
    pub last_fired_at: Option<Timestamp>,
    /// 不可变创建者类型（`449`；可空，`NULL` ⇒ fail-closed；**无 CHECK**）。
    pub created_by_type: Option<AutopilotActorType>,
    /// 不可变创建者 id（`449`；可空）——run 以它为主体。
    pub created_by_id: Option<Id>,
    /// 配置责任人类型（`189`；可空；**无 CHECK**；CONFIG 审计用，**不决定授权**）。
    pub published_by_type: Option<AutopilotActorType>,
    /// 配置责任人 id（`189`；同上）。
    pub published_by_id: Option<Id>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 更新时间。
    pub updated_at: Timestamp,
}

/// `autopilot_run` 行（18 列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutopilotRun {
    /// 主键。
    pub id: Id,
    /// 所属 autopilot（级联删除）。
    pub autopilot_id: Id,
    /// 触发它的触发器（`ON DELETE SET NULL`；手动触发也可能为空）。
    pub trigger_id: Option<Id>,
    /// 状态（五态，见 [`AutopilotRunStatus`]；`042` 的默认值 `pending` 已随 `043` 消失）。
    pub status: AutopilotRunStatus,
    /// 触发来源（见 [`RunSource`]）。
    pub source: RunSource,
    /// `create_issue` 模式建出来的 issue。
    pub issue_id: Option<Id>,
    /// squad 指派的旁路记录（`ON DELETE SET NULL`）。
    pub squad_id: Option<Id>,
    /// 派出去的任务（`agent_task_queue`，`ON DELETE SET NULL`）。
    pub task_id: Option<Id>,
    /// 配额预留（`352`；**无外键**，与 `webhook_delivery_id` 同属应用层完整性）。
    pub quota_reservation_id: Option<Id>,
    /// 触发它的入站投递（**无外键**：`176` 有意不加，让部署/回滚不耦合 `webhook_delivery` 的级联）。
    pub webhook_delivery_id: Option<Id>,
    /// 触发载荷快照。
    pub trigger_payload: Option<serde_json::Value>,
    /// 计划触发时间（`124` 加；调度取整语义见 M5-7）。
    pub planned_at: Option<Timestamp>,
    /// 实际触发时间。
    pub triggered_at: Timestamp,
    /// 结束时间（终态才有）。
    pub completed_at: Option<Timestamp>,
    /// 结果载荷。
    pub result: Option<serde_json::Value>,
    /// 失败原因（人可读）。
    pub failure_reason: Option<String>,
    /// 机器可读原因码（**自由文本，无 CHECK**；`352` 加，配额拒绝也写这里）。
    pub reason_code: Option<String>,
    /// 创建时间。
    pub created_at: Timestamp,
}

/// `webhook_delivery` 行（28 列）——入站 webhook 的**耐久投递队列**（`093` + `176` 的 worker）。
///
/// 出队语义：`status = queued` 且 `available_at <= now()`，用 `lease_token` +
/// `lease_expires_at` 做租约（`176`）；`dispatch_attempts` / `attempt_count` 分开计
/// 「派发尝试」与「HTTP 重试」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookDelivery {
    /// 主键。
    pub id: Id,
    /// 所属工作区（级联删除）。
    pub workspace_id: Id,
    /// 命中的 autopilot（级联删除）。
    pub autopilot_id: Id,
    /// 命中的触发器（级联删除）。
    pub trigger_id: Id,
    /// webhook 载体。
    pub provider: WebhookProvider,
    /// 投递状态（见 [`DeliveryStatus`]）。
    pub status: DeliveryStatus,
    /// 签名校验结果（见 [`SignatureStatus`]）。
    pub signature_status: SignatureStatus,
    /// 事件名（载体给出的 `event`）。
    pub event: String,
    /// 去重键（同一 `dedupe_key` 只派发一次）。
    pub dedupe_key: Option<String>,
    /// 去重来源（哪一段正文算出的 key）。
    pub dedupe_source: Option<String>,
    /// 重放幂等键（`352` 加）。
    pub replay_idempotency_key: Option<String>,
    /// 从哪条投递重放而来（自引用，`ON DELETE SET NULL`）。
    pub replayed_from_delivery_id: Option<Id>,
    /// 派发后对应的 run（`ON DELETE SET NULL`）。
    pub autopilot_run_id: Option<Id>,
    /// 总尝试次数（`176` 的 `dispatch_attempts` 只算 worker 派发；本列自 `093` 起
    /// 用于**入站去重命中**：重复请求不另起行，只把命中目标的本列 +1）。
    pub attempt_count: i32,
    /// worker 派发尝试次数（`176`；退避与放弃判定用它）。
    pub dispatch_attempts: i32,
    /// 可出队时间（退避到期）。
    pub available_at: Timestamp,
    /// 租约令牌（`176`；worker 认领时轮换）。
    pub lease_token: Option<Id>,
    /// 租约到期时间（过期即可被别的 worker 偷）。
    pub lease_expires_at: Option<Timestamp>,
    /// 最近一次尝试时间。
    pub last_attempt_at: Timestamp,
    /// 白名单保留的请求头（`jsonb`）。
    pub selected_headers: serde_json::Value,
    /// 请求 `Content-Type`。
    pub content_type: Option<String>,
    /// 原始正文（`bytea`；用于重放与签名复算）。
    pub raw_body: Option<Vec<u8>>,
    /// 出站响应状态码（重放时用）。
    pub response_status: Option<i32>,
    /// 出站响应正文（截断后）。
    pub response_body: Option<String>,
    /// 失败原因（人可读）。
    pub error: Option<String>,
    /// 机器可读原因码（自由文本）。
    pub reason_code: Option<String>,
    /// 接收时间（载体报文自带的时间不算）。
    pub received_at: Timestamp,
    /// 创建时间。
    pub created_at: Timestamp,
}

/// `autopilot_collaborator` 行（5 列，`128`）——显式写权授予。
///
/// 授权集合（`128` 原文，`MUL-3807`）= **创建者 ∪ workspace owner/admin ∪ 本表成员**；
/// 被列入者可编辑/删除/触发/重放投递/管理触发器与 webhook 密钥。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutopilotCollaborator {
    /// 所属 autopilot（PK 的一部分）。
    pub autopilot_id: Id,
    /// 协作者 id（PK 的一部分）。
    pub user_id: Id,
    /// 协作者类型（member-only）。
    pub user_type: AutopilotUserType,
    /// 谁授予的。
    pub granted_by: Id,
    /// 创建时间。
    pub created_at: Timestamp,
}

/// `autopilot_subscriber` 行（4 列，`120`）——自动订阅该 autopilot 建的 issue 的成员模板。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutopilotSubscriber {
    /// 所属 autopilot（PK 的一部分）。
    pub autopilot_id: Id,
    /// 订阅者 id（PK 的一部分）。
    pub user_id: Id,
    /// 订阅者类型（member-only）。
    pub user_type: AutopilotUserType,
    /// 创建时间。
    pub created_at: Timestamp,
}

/// `autopilot_rule_version` 行（7 列，`186`）——配置快照（**append-only** 审计 + 回滚依据）。
///
/// 语义（`186` 原文，`MUL-4302`）：每次**实质性发布**（create / enable / resume / 触发条件 /
/// 执行目标 / 任务指令变更）**追加**一行，记录发布者 + 当时的生效配置摘要；
/// 装饰性编辑（改名、改描述）**不**追加。派单时读**最新**一行并盖章：
/// run 的 `originator_source = rule_owner`、`accountable_user_id = published_by_id`、
/// `agent_task_queue.rule_version_id = <本行>`。**不 UPDATE 旧行**，所以老 run 的
/// `rule_version_id` 永远指向当时真正生效的那份配置。
///
/// `published_by_type` 是 `TEXT NOT NULL` 但**无 CHECK**（上游写 `member`/`agent`）；
/// `published_by_id` 可空 ⇒ 系统发布的规则没有人类，run 降级为 `unattributed`，
/// **不允许编造**一个人。无 FK / 无级联（`186` 主动 DROP 过可能的 fkey）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutopilotRuleVersion {
    /// 主键。
    pub id: Id,
    /// 所属 autopilot。
    pub autopilot_id: Id,
    /// 所属工作区。
    pub workspace_id: Id,
    /// 配置摘要（`jsonb`；完整配置的快照形状由 M5-2 定）。
    pub config_summary: serde_json::Value,
    /// 发布者 id（可空；`NULL` ⇒ run 降级为 `unattributed`，不编造人类）。
    pub published_by_id: Option<Id>,
    /// 发布者类型（`NOT NULL` 但**无 CHECK**，约定 `member`/`agent`）。
    pub published_by_type: AutopilotActorType,
    /// 创建时间。
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 枚举 `as_str()` = DB `CHECK` 取值集合；写错就是绑定/解码对不上。
    /// 复算：`contracts/upstream-schema.json` 里对应表的 CHECK 定义。
    #[test]
    fn enum_strings_match_db_checks() {
        assert_eq!(AutopilotStatus::Active.as_str(), "active");
        assert_eq!(AutopilotStatus::Archived.as_str(), "archived");
        assert_eq!(AutopilotAssigneeType::Squad.as_str(), "squad");
        assert_eq!(AutopilotActorType::Member.as_str(), "member");
        assert_eq!(AutopilotExecutionMode::RunOnly.as_str(), "run_only");
        assert_eq!(TriggerKind::Schedule.as_str(), "schedule");
        assert_eq!(WebhookProvider::Github.as_str(), "github");
        assert_eq!(AutopilotRunStatus::IssueCreated.as_str(), "issue_created");
        assert_eq!(RunSource::Manual.as_str(), "manual");
        assert_eq!(DeliveryStatus::Queued.as_str(), "queued");
        assert_eq!(SignatureStatus::NotRequired.as_str(), "not_required");
        assert_eq!(AutopilotUserType::Member.as_str(), "member");
        assert_eq!(ACTOR_TYPES, ["member", "agent"]);
        assert!(AutopilotActorType::Member.yields_run_principal());
        assert!(!AutopilotActorType::Agent.yields_run_principal());
    }

    #[test]
    fn run_status_terminal_set() {
        assert!(!AutopilotRunStatus::IssueCreated.is_terminal());
        assert!(!AutopilotRunStatus::Running.is_terminal());
        assert!(AutopilotRunStatus::Skipped.is_terminal());
        assert!(AutopilotRunStatus::Failed.is_terminal());
    }
}
