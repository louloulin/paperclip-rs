//! wakeup 服务层（上半）：`Validate` / `save`（含 `Create`、`Enable`）/ `EditInstruction` /
//! `Disable` / `CheckClaim` / `StopClosedIssueWakeups`。
//!
//! - **写者**：M5-6。上游 `internal/service/issue_wakeup.go` 的 1–512 与 743–832 行。
//! - **鉴权全在 HTTP 层（C3）**：本层只做**上游同款的业务级**判定（成员资格、所有者/管理员、
//!   `CanMemberInvokeAgent`），不重复 workspace 解析。
//! - **所有写都在一个事务里**：`save`/`disable`/`edit_instruction` 的顺序、锁的重数与上游逐字对应
//!   （`LockWakeupIssue` 可能被调两次，两次都是 `FOR UPDATE`，无害；顺序错才会死锁）。
//! - **时间基准一律取 `SELECT now()`**（[`wl::transaction_now`]），不用应用侧时钟：上游在同一事务里
//!   既读 `now()` 又写 `next_fire_at`，混用两个时钟会让 `at` 校验与调度推进出现竞态。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::schedule::next_occurrence_after_utc;
use super::WakeupError;
use mc_repos::wakeup::lookup::WakeupAgentRow;
use mc_repos::wakeup::{issue as wi, lookup as wl, receipt as wr, WakeupRow};

/// 指令字节上限（上游 `len(in.Instruction) > 12000`，Go 的 `len` 是字节）。
pub const INSTRUCTION_MAX_BYTES: usize = 12_000;
/// `after_seconds` / `interval_seconds` 的上限（一年）。
pub const SCHEDULE_SECONDS_MAX: i64 = 31_536_000;
/// `interval_seconds` 的下限（1 分钟）。
pub const INTERVAL_SECONDS_MIN: i64 = 60;

/// `eventcontract.WakeupTypes`（25 项）的逐字移植。
///
/// **为什么不从别处引**：本仓没有 `eventcontract` 的等价物（`mc-realtime` 只有传输层帧），
/// 这 25 个名字就是 wakeup 的对外契约，落在本模块并由单测钉住。
pub const WAKEUP_EVENT_TYPES: [&str; 25] = [
    "task.queued",
    "task.dispatched",
    "task.deferred",
    "task.waiting_local_directory",
    "task.started",
    "task.completed",
    "task.failed",
    "task.cancelled",
    "issue.updated",
    "issue.status_changed",
    "issue.assignee_changed",
    "issue.parent_changed",
    "issue.project_changed",
    "issue.labels_changed",
    "issue.properties_changed",
    "issue.metadata_changed",
    "comment.created",
    "comment.updated",
    "comment.deleted",
    "comment.resolved",
    "comment.unresolved",
    "reaction.added",
    "reaction.removed",
    "attachment.attached",
    "attachment.detached",
];

/// `eventcontract.LifecycleTypes`：这两类**不能**注册（它们的事件本身就是 issue 的生死）。
pub const LIFECYCLE_EVENT_TYPES: [&str; 2] = ["issue.created", "issue.deleted"];

/// `eventcontract.TaskEvent(status)`：`running` 是唯一的改名，其余 `task.<status>`。
#[must_use]
pub fn task_event(status: &str) -> String {
    if status == "running" {
        "task.started".to_string()
    } else {
        format!("task.{status}")
    }
}

/// 上游 `WakeupInput`（JSON 键名逐字保留；`at` 用 RFC3339）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WakeupInput {
    /// 被唤醒的 agent。
    pub agent_id: String,
    /// 注入 prompt 的指令。
    pub instruction: String,
    /// `event | at | every | cron`。
    pub kind: String,
    /// `once | continuous`。
    pub mode: String,
    /// 订阅的事件名。
    pub event_types: Vec<String>,
    /// 「只有这个 agent 的动作算数」。
    pub filter_agent_id: String,
    /// `member | agent`。
    pub filter_actor_type: String,
    /// 主体过滤 id。
    pub filter_actor_id: String,
    /// 「只有这个 run 的动作算数」。
    pub filter_task_id: String,
    /// 注册它的评论。
    pub parent_comment_id: String,
    /// `kind=at`：相对当前时间多少秒后。
    pub after_seconds: i64,
    /// `kind=at`：绝对时间。
    pub at: Option<DateTime<Utc>>,
    /// `kind=every`：间隔秒。
    pub interval_seconds: i64,
    /// `kind=cron`：5 字段表达式。
    pub cron_expression: String,
    /// 调度时区（IANA；空 ⇒ `UTC`）。
    pub timezone: String,
}

/// 上游 `WakeupEnableInput`（重新启用：客户端**不必**回传整份配置）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WakeupEnableInput {
    /// 乐观并发版本号。
    pub revision: i64,
    /// `kind=at` 可以顺手改一次时间。
    pub at: Option<DateTime<Utc>>,
    /// 已消费的 `once` 必须显式 rearm。
    pub rearm: bool,
}

/// 上游 `WakeupInstructionInput`（只改指令，**不动版本号**）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WakeupInstructionInput {
    /// 新指令。
    pub instruction: String,
    /// 客户端读到的旧指令（并发比对的一半）。
    pub expected_instruction: String,
    /// 客户端读到的旧版本号（并发比对的一半）。
    pub revision: i64,
}

/// 上游 `Validate`：**就地**规范化 `in` 并算出首次触发时间（`None` = 不入库）。
///
/// 返回 `Ok(None)` 表示「事件驱动，没有 `next_fire_at`」；错误一律是
/// [`WakeupError::Input`]（→ 400），文案与上游逐字一致。
#[allow(clippy::too_many_lines)] // 上游 `Validate` 是一整段顺序规则；拆开会让「哪条规则先判」不可见
pub fn validate(
    input: &mut WakeupInput,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, WakeupError> {
    input.instruction = input.instruction.trim().to_string();
    if input.instruction.is_empty() || input.instruction.len() > INSTRUCTION_MAX_BYTES {
        return Err(WakeupError::input("instruction must contain 1–12000 bytes"));
    }
    if input.timezone.is_empty() {
        input.timezone = "UTC".to_string();
    }
    if input.timezone.parse::<chrono_tz::Tz>().is_err() {
        return Err(WakeupError::input("invalid timezone"));
    }
    if input.mode.is_empty() {
        input.mode = if input.kind == "every" || input.kind == "cron" {
            "continuous".to_string()
        } else {
            "once".to_string()
        };
    }
    if input.mode != "once" && input.mode != "continuous" {
        return Err(WakeupError::input("mode must be once or continuous"));
    }
    if !input.filter_actor_type.is_empty() || !input.filter_actor_id.is_empty() {
        if input.kind != "event"
            || (input.filter_actor_type != "member" && input.filter_actor_type != "agent")
            || input.filter_actor_id.is_empty()
        {
            return Err(WakeupError::input(
                "actor filter requires an event, member or agent type, and actor ID",
            ));
        }
        if !input.filter_agent_id.is_empty() || !input.filter_task_id.is_empty() {
            return Err(WakeupError::input(
                "choose an actor filter or agent/run filters",
            ));
        }
        for event in &input.event_types {
            if event.starts_with("task.") {
                return Err(WakeupError::input(
                    "actor filters apply to issue, comment, reaction and attachment changes; use agent/run filters for task events",
                ));
            }
        }
    }
    match input.kind.as_str() {
        "event" => {
            if input.event_types.is_empty() || input.event_types.len() > WAKEUP_EVENT_TYPES.len() {
                return Err(WakeupError::input("select at least one supported event"));
            }
            for event in &input.event_types {
                if LIFECYCLE_EVENT_TYPES.contains(&event.as_str()) {
                    return Err(WakeupError::input(format!(
                        "{event} cannot wake its own issue; subscriptions require an existing, open issue"
                    )));
                }
                if !WAKEUP_EVENT_TYPES.contains(&event.as_str()) {
                    return Err(WakeupError::input(format!("unsupported event: {event}")));
                }
            }
            if !input.filter_task_id.is_empty()
                && !input.event_types.iter().all(|e| e.starts_with("task."))
            {
                return Err(WakeupError::input("task filter requires task events"));
            }
            if input.after_seconds != 0
                || input.at.is_some()
                || input.interval_seconds != 0
                || !input.cron_expression.is_empty()
            {
                return Err(WakeupError::input(
                    "event wakeups cannot contain a schedule",
                ));
            }
            // 旧的「只有变更事件 + agent 过滤」是 actor=agent 的别名；混了 `task.*` 的订阅保持原样
            // （已有客户端依赖它）。
            if !input.filter_agent_id.is_empty() && input.filter_task_id.is_empty() {
                let mutation_only = !input.event_types.iter().any(|e| e.starts_with("task."));
                if mutation_only {
                    input.filter_actor_type = "agent".to_string();
                    input.filter_actor_id = input.filter_agent_id.clone();
                    input.filter_agent_id = String::new();
                }
            }
            Ok(None)
        }
        "at" => {
            if input.mode != "once" {
                return Err(WakeupError::input("single time requires once mode"));
            }
            // 上游：`(in.At == nil) == (in.AfterSeconds == 0)` —— 两者必须**恰好一个**成立。
            if input.at.is_none() == (input.after_seconds == 0)
                || input.after_seconds < 0
                || input.after_seconds > SCHEDULE_SECONDS_MAX
            {
                return Err(WakeupError::input(
                    "provide at or after_seconds (1–31536000)",
                ));
            }
            if input.interval_seconds != 0 || !input.cron_expression.is_empty() {
                return Err(WakeupError::input("incompatible schedule fields"));
            }
            match input.at {
                Some(at) => Ok(Some(at)),
                None => Ok(Some(now + chrono::Duration::seconds(input.after_seconds))),
            }
        }
        "every" => {
            if input.mode != "continuous"
                || input.interval_seconds < INTERVAL_SECONDS_MIN
                || input.interval_seconds > SCHEDULE_SECONDS_MAX
                || input.at.is_some()
                || input.after_seconds != 0
                || !input.cron_expression.is_empty()
            {
                return Err(WakeupError::input(
                    "every requires continuous mode and interval_seconds between 60 and 31536000",
                ));
            }
            Ok(Some(
                now + chrono::Duration::seconds(input.interval_seconds),
            ))
        }
        "cron" => {
            if input.mode != "continuous"
                || input.at.is_some()
                || input.after_seconds != 0
                || input.interval_seconds != 0
            {
                return Err(WakeupError::input(
                    "cron requires continuous mode without other schedule fields",
                ));
            }
            match next_occurrence_after_utc(&input.cron_expression, &input.timezone, now) {
                Ok(Some(next)) => Ok(Some(next)),
                _ => Err(WakeupError::input("cron must have a future occurrence")),
            }
        }
        _ => Err(WakeupError::input("kind must be event, at, every or cron")),
    }
}

/// `wakeupUUID`：空串 ⇒ `None`；非法 ⇒ `invalid wakeup: invalid UUID`。
fn parse_uuid_field(raw: &str) -> Result<Option<Uuid>, WakeupError> {
    if raw.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(raw)
        .map(Some)
        .map_err(|_| WakeupError::input("invalid UUID"))
}

/// 上游 `authorize`：`member` 必须是成员，且能调用该 agent；agent 必须未归档且绑了 runtime。
pub async fn authorize(
    conn: &mut PgConnection,
    workspace_id: Uuid,
    member: Uuid,
    agent: &WakeupAgentRow,
) -> Result<(), WakeupError> {
    if agent.archived_at.is_some() || agent.runtime_id.is_none() {
        return Err(WakeupError::Forbidden);
    }
    if wl::member_role(&mut *conn, workspace_id, member)
        .await?
        .is_none()
    {
        return Err(WakeupError::Forbidden);
    }
    if !can_member_invoke_agent(&mut *conn, agent, member, workspace_id).await? {
        return Err(WakeupError::Forbidden);
    }
    Ok(())
}

/// 上游 `CanMemberInvokeAgent`：**逐字**实现（**不要**换成 `AgentScope::member_allowed_to_view`：
/// 那个还会给管理员/`can_manage` 放行，比这里宽）。
pub async fn can_member_invoke_agent(
    conn: &mut PgConnection,
    agent: &WakeupAgentRow,
    member: Uuid,
    workspace_id: Uuid,
) -> Result<bool, WakeupError> {
    if agent.owner_id == Some(member) {
        return Ok(true);
    }
    if agent.permission_mode != "public_to" {
        return Ok(false);
    }
    let is_member = wl::member_role(&mut *conn, workspace_id, member)
        .await?
        .is_some();
    for (target_type, target_id) in wl::invocation_targets(&mut *conn, agent.id).await? {
        match target_type.as_str() {
            "workspace" if is_member => return Ok(true),
            "member" if target_id == member => return Ok(true),
            _ => {}
        }
    }
    Ok(false)
}

/// 上游 `Create` = `Save(零 id)`。
pub async fn create(
    pool: &PgPool,
    issue_id: Uuid,
    member: Uuid,
    source: Option<Uuid>,
    input: WakeupInput,
) -> Result<WakeupRow, WakeupError> {
    save(pool, issue_id, member, source, None, input, None).await
}

/// 上游 `Enable` = `save(空 input, enable)`（配置全程从库里读，客户端不回传）。
pub async fn enable(
    pool: &PgPool,
    issue_id: Uuid,
    member: Uuid,
    source: Option<Uuid>,
    id: Uuid,
    enable: &WakeupEnableInput,
) -> Result<WakeupRow, WakeupError> {
    save(
        pool,
        issue_id,
        member,
        source,
        Some(id),
        WakeupInput::default(),
        Some(enable),
    )
    .await
}

/// 上游 `Save` / `save`：**新建或整份替换**一条订阅（`revision` 自增，旧收据与未启动的 run 作废）。
#[allow(clippy::too_many_lines)] // 上游 `save` 221 行的事务脚本：锁序/分支顺序就是语义，不平铺会更难对照
pub async fn save(
    pool: &PgPool,
    issue_id: Uuid,
    member: Uuid,
    source: Option<Uuid>,
    existing_id: Option<Uuid>,
    mut input: WakeupInput,
    enable: Option<&WakeupEnableInput>,
) -> Result<WakeupRow, WakeupError> {
    let mut tx = pool.begin().await?;
    // ① 先锁 issue 背后的 workspace（`FOR KEY SHARE OF w`）再锁 issue —— 顺序与上游一致。
    wi::lock_workspace_for_issue(&mut tx, issue_id).await?;
    let issue = wi::lock_issue(&mut tx, issue_id).await?;
    if !wi::issue_is_active(&mut *tx, issue.workspace_id, &issue.status).await? {
        return Err(WakeupError::input("issue is closed"));
    }
    let now = wl::transaction_now(&mut tx).await?;

    if let Some(enable) = enable {
        let target_id = existing_id.ok_or(WakeupError::NotFound)?;
        let old = wi::lock(&mut tx, target_id).await?;
        if old.issue_id != issue_id || old.workspace_id != issue.workspace_id {
            return Err(WakeupError::NotFound);
        }
        if enable.revision < 1 {
            return Err(WakeupError::input("revision is required"));
        }
        if old.revision != enable.revision {
            return Err(WakeupError::Conflict);
        }
        input = WakeupInput {
            agent_id: uuid_string(old.agent_id),
            instruction: old.instruction.clone(),
            kind: old.kind.clone(),
            mode: old.mode.clone(),
            event_types: old.event_types.clone(),
            filter_agent_id: opt_uuid_string(old.filter_agent_id),
            filter_actor_type: old.filter_actor_type.clone().unwrap_or_default(),
            filter_actor_id: opt_uuid_string(old.filter_actor_id),
            filter_task_id: opt_uuid_string(old.filter_task_id),
            parent_comment_id: opt_uuid_string(old.parent_comment_id),
            after_seconds: 0,
            at: None,
            interval_seconds: old.interval_seconds.unwrap_or(0),
            cron_expression: old.cron_expression.clone().unwrap_or_default(),
            timezone: old.timezone.clone(),
        };
        if enable.at.is_some() && old.kind != "at" {
            return Err(WakeupError::input(
                "only single-time wakeups accept a new time",
            ));
        }
        if !old.enabled
            && old.mode == "once"
            && (old.disabled_at.is_none() || old.last_task_id.is_some())
        {
            if !enable.rearm {
                return Err(WakeupError::input(
                    "consumed one-shot requires explicit rearm",
                ));
            }
            if wi::active_run_exists(&mut *tx, issue_id, old.id).await? {
                return Err(WakeupError::Conflict);
            }
        }
        if old.kind == "at" {
            if let Some(at) = old.next_fire_at {
                input.at = Some(at);
            }
            if let Some(at) = enable.at {
                input.at = Some(at);
            }
            let future = input.at.is_some_and(|at| at > now);
            if !old.enabled && !future {
                return Err(WakeupError::input("choose a future time"));
            }
        }
    }

    let next = validate(&mut input, now)?;
    let Some(agent_id) = parse_uuid_field(&input.agent_id)? else {
        return Err(WakeupError::input("agent_id is required"));
    };
    let agent = wl::agent_for_wakeup(&mut *tx, issue.workspace_id, agent_id)
        .await?
        .ok_or(WakeupError::Forbidden)?;
    authorize(&mut tx, issue.workspace_id, member, &agent).await?;

    let filter_agent = parse_uuid_field(&input.filter_agent_id)?;
    let filter_task = parse_uuid_field(&input.filter_task_id)?;
    let filter_actor = parse_uuid_field(&input.filter_actor_id)?;
    if let Some(actor) = filter_actor {
        let found = if input.filter_actor_type == "member" {
            wl::member_role(&mut *tx, issue.workspace_id, actor)
                .await?
                .is_some()
        } else {
            wl::agent_for_wakeup(&mut *tx, issue.workspace_id, actor)
                .await?
                .is_some()
        };
        if !found {
            return Err(WakeupError::Forbidden);
        }
    }
    let parent = parse_uuid_field(&input.parent_comment_id)?;
    if let Some(agent_id) = filter_agent {
        if wl::agent_for_wakeup(&mut *tx, issue.workspace_id, agent_id)
            .await?
            .is_none()
        {
            return Err(WakeupError::Forbidden);
        }
    }
    if input.kind != "event"
        && (!input.event_types.is_empty() || filter_agent.is_some() || filter_task.is_some())
    {
        return Err(WakeupError::input(
            "time wakeup cannot contain event filters",
        ));
    }
    if let Some(parent) = parent {
        if !wl::comment_exists(&mut *tx, parent, issue_id).await? {
            return Err(WakeupError::input("comment not found"));
        }
    }

    if let Some(existing_id) = existing_id {
        let old = wi::lock(&mut tx, existing_id).await?;
        if old.issue_id != issue_id || old.workspace_id != issue.workspace_id {
            return Err(WakeupError::NotFound);
        }
        let role = wl::member_role(&mut *tx, issue.workspace_id, member)
            .await?
            .ok_or(WakeupError::Forbidden)?;
        if old.created_by != member && role != "owner" && role != "admin" {
            return Err(WakeupError::Forbidden);
        }
        if let Some(enable) = enable {
            if old.enabled {
                if enable.rearm || enable.at.is_some() {
                    return Err(WakeupError::Conflict);
                }
                // 幂等「重新启用」：不改任何字段、**不 bump revision**，原样返回。
                tx.commit().await?;
                return Ok(old);
            }
        }
        wr::discard_for_wakeup(&mut tx, old.id).await?;
        wi::cancel_unstarted_wakeup_tasks(&mut tx, old.id).await?;
    }

    let mut target = None;
    if let Some(task_id) = filter_task {
        match wi::lock_source_task(&mut tx, task_id, issue_id).await? {
            Some(row) => {
                if let Some(agent_id) = filter_agent {
                    if agent_id != row.agent_id {
                        return Err(WakeupError::input("run and agent filters disagree"));
                    }
                }
                target = Some(row);
            }
            None => return Err(WakeupError::input("run does not belong to this issue")),
        }
    }

    let new = wi::NewWakeup {
        workspace_id: issue.workspace_id,
        issue_id: issue.id,
        agent_id: agent.id,
        created_by: member,
        source_task_id: source,
        parent_comment_id: parent,
        instruction: input.instruction.clone(),
        kind: input.kind.clone(),
        mode: input.mode.clone(),
        event_types: input.event_types.clone(),
        filter_agent_id: filter_agent,
        filter_task_id: filter_task,
        filter_actor_type: {
            let value = input.filter_actor_type.clone();
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        },
        filter_actor_id: filter_actor,
        interval_seconds: if input.interval_seconds > 0 {
            Some(input.interval_seconds)
        } else {
            None
        },
        cron_expression: if input.cron_expression.is_empty() {
            None
        } else {
            Some(input.cron_expression.clone())
        },
        timezone: input.timezone.clone(),
        next_fire_at: next,
    };
    let mut out = match existing_id {
        Some(id) => wi::replace(&mut tx, id, &new).await?,
        None => wi::create(&mut tx, &new).await?,
    };

    // 注册即已终结的源 run：同一把行锁下补一条「注册快照」收据，关掉「刚注册就错过」的竞态。
    if let Some(target) = target {
        if matches!(target.status.as_str(), "completed" | "failed" | "cancelled")
            && input.event_types.contains(&task_event(&target.status))
        {
            let event_type = task_event(&target.status);
            let payload = serde_json::json!({
                "event_id": format!("{}:{}", target.id, target.status),
                "event_type": event_type,
                "version": 1,
                "occurred_at": target.completed_at,
                "observed_at": now,
                "registration_snapshot": true,
                "workspace_id": issue.workspace_id,
                "issue_id": issue.id,
                "task_id": target.id,
                "source_task_id": target.id,
                "status": target.status,
                "agent_id": target.agent_id,
                "actor_type": "agent",
                "actor_id": target.agent_id,
                "retry_of_task_id": target.retry_of_task_id,
                "rerun_of_task_id": target.rerun_of_task_id,
            });
            wr::record(
                &mut tx,
                out.id,
                out.revision,
                &format!("{}:{}", target.id, target.status),
                &event_type,
                &payload,
            )
            .await?;
            if out.mode == "once" {
                wi::advance(&mut tx, out.id, false, None, None, None).await?;
                out.enabled = false;
            }
        }
    }
    tx.commit().await?;
    Ok(out)
}

fn uuid_string(id: Uuid) -> String {
    if id.is_nil() {
        String::new()
    } else {
        id.to_string()
    }
}

fn opt_uuid_string(id: Option<Uuid>) -> String {
    id.map(|id| id.to_string()).unwrap_or_default()
}

/// 上游 `EditInstruction`：改指令但**保留 revision 与已排队的工作**（并发比对 revision + 旧文案）。
pub async fn edit_instruction(
    pool: &PgPool,
    issue_id: Uuid,
    id: Uuid,
    member: Uuid,
    input: &WakeupInstructionInput,
) -> Result<(), WakeupError> {
    let instruction = input.instruction.trim().to_string();
    if instruction.is_empty() || instruction.len() > INSTRUCTION_MAX_BYTES || input.revision < 1 {
        return Err(WakeupError::input(
            "instruction must be 1–12000 bytes and revision is required",
        ));
    }
    let mut tx = pool.begin().await?;
    let workspace_id = wi::lock_workspace_for_issue(&mut tx, issue_id).await?;
    let issue = wi::lock_issue(&mut tx, issue_id).await?;
    let role = wl::member_role(&mut *tx, issue.workspace_id, member)
        .await?
        .ok_or(WakeupError::Forbidden)?;
    let w = wi::lock(&mut tx, id).await?;
    if w.issue_id != issue.id || w.workspace_id != issue.workspace_id {
        return Err(WakeupError::NotFound);
    }
    if w.created_by != member && role != "owner" && role != "admin" {
        return Err(WakeupError::Forbidden);
    }
    let agent = wl::agent_for_wakeup(&mut *tx, issue.workspace_id, w.agent_id)
        .await?
        .ok_or(WakeupError::Forbidden)?;
    authorize(&mut tx, issue.workspace_id, member, &agent).await?;
    if w.revision != input.revision || w.instruction != input.expected_instruction {
        return Err(WakeupError::Conflict);
    }
    wi::edit_instruction(&mut tx, id, workspace_id, &instruction).await?;
    tx.commit().await?;
    Ok(())
}

/// 上游 `Disable`：只撤掉**尚未被认领**的工作；已在跑的 run 走普通 Stop 控制。
///
/// 返回被取消的未启动 run 行（M5-8 负责在提交后广播 `task.cancelled`；本片没有该广播原语）。
pub async fn disable(
    pool: &PgPool,
    issue_id: Uuid,
    id: Uuid,
    member: Uuid,
) -> Result<(WakeupRow, Vec<wi::WakeupTaskRow>), WakeupError> {
    let mut tx = pool.begin().await?;
    wi::lock_workspace_for_issue(&mut tx, issue_id).await?;
    let issue = wi::lock_issue(&mut tx, issue_id).await?;
    let role = wl::member_role(&mut *tx, issue.workspace_id, member)
        .await?
        .ok_or(WakeupError::Forbidden)?;
    let w = wi::lock(&mut tx, id).await?;
    if w.issue_id != issue.id || w.workspace_id != issue.workspace_id {
        return Err(WakeupError::NotFound);
    }
    if w.created_by != member && role != "owner" && role != "admin" {
        return Err(WakeupError::Forbidden);
    }
    wi::disable(&mut tx, id).await?;
    wr::discard_for_wakeup(&mut tx, id).await?;
    let cancelled = wi::cancel_unstarted_wakeup_tasks(&mut tx, id).await?;
    let out = wi::lock(&mut tx, id).await?;
    tx.commit().await?;
    Ok((out, cancelled))
}

/// 上游 `StopClosedIssueWakeups`：issue 关掉后停掉全部 wakeup 并取消未启动的 run。
///
/// 调用方必须**在写状态的那个事务里**（且已 `LockWakeupIssue`）调它，让关停与状态写同生共死；
/// 返回的行由调用方在提交后广播 `task.cancelled`。
pub async fn stop_closed_issue_wakeups(
    conn: &mut PgConnection,
    issue: &wi::WakeupIssueRow,
) -> Result<Vec<wi::WakeupTaskRow>, WakeupError> {
    if wi::issue_is_active(&mut *conn, issue.workspace_id, &issue.status).await? {
        return Ok(Vec::new());
    }
    wi::disable_issue_wakeups(&mut *conn, issue.id).await?;
    Ok(wi::cancel_unstarted_issue_wakeup_tasks(&mut *conn, issue.id).await?)
}

/// 上游 `CheckClaim`：离线等待之后**重新**验证人的权限（入队时的 MCP overlay 不授予永久执行权）。
pub async fn check_claim(
    pool: &PgPool,
    context: Option<&serde_json::Value>,
    issue_id: Uuid,
    agent_id: Uuid,
    originator_user_id: Option<Uuid>,
) -> Result<(), WakeupError> {
    let Some(context) = context else {
        return Ok(());
    };
    let id = context
        .get("wakeup_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if id.is_empty() {
        return Ok(());
    }
    let id = Uuid::parse_str(id).map_err(|_| WakeupError::Forbidden)?;
    let revision = context
        .get("wakeup_revision")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    let w = wi::lockless(pool, id)
        .await?
        .ok_or(WakeupError::Forbidden)?;
    if w.disabled_at.is_some()
        || w.revision != revision
        || w.issue_id != issue_id
        || w.agent_id != agent_id
        || Some(w.created_by) != originator_user_id
    {
        return Err(WakeupError::Forbidden);
    }
    let agent = wl::agent_for_wakeup(pool, w.workspace_id, w.agent_id)
        .await?
        .ok_or(WakeupError::Forbidden)?;
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| WakeupError::Db(e.to_string()))?;
    authorize(&mut conn, w.workspace_id, w.created_by, &agent).await
}

#[cfg(test)]
mod tests {
    use super::{task_event, validate, WakeupError, WakeupInput, WAKEUP_EVENT_TYPES};
    use crate::wakeup::dispatch::truncate_for_summary;
    use chrono::{DateTime, Utc};

    fn now() -> DateTime<Utc> {
        "2026-09-23T12:00:00Z".parse().unwrap()
    }

    fn base_event() -> WakeupInput {
        WakeupInput {
            agent_id: "01930000-0000-7000-8000-000000000001".to_string(),
            instruction: "  look at it  ".to_string(),
            kind: "event".to_string(),
            event_types: vec!["comment.created".to_string()],
            ..WakeupInput::default()
        }
    }

    fn err(input: &mut WakeupInput) -> String {
        match validate(input, now()) {
            Ok(_) => panic!("expected validation error for {input:?}"),
            Err(WakeupError::Input(msg)) => msg,
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn event_wakeup_trims_instruction_defaults_and_has_no_schedule() {
        let mut input = base_event();
        assert_eq!(validate(&mut input, now()).unwrap(), None);
        assert_eq!(input.instruction, "look at it");
        assert_eq!(input.timezone, "UTC");
        assert_eq!(input.mode, "once");
    }

    #[test]
    fn every_and_cron_default_to_continuous_mode() {
        let mut every = WakeupInput {
            kind: "every".to_string(),
            interval_seconds: 60,
            ..WakeupInput::default()
        };
        every.instruction = "x".to_string();
        assert_eq!(
            validate(&mut every, now()).unwrap(),
            Some(now() + chrono::Duration::seconds(60))
        );
        assert_eq!(every.mode, "continuous");

        let mut cron = WakeupInput {
            kind: "cron".to_string(),
            cron_expression: "0 9 * * 1-5".to_string(),
            timezone: "Asia/Shanghai".to_string(),
            ..WakeupInput::default()
        };
        cron.instruction = "x".to_string();
        let next = validate(&mut cron, now()).unwrap().unwrap();
        assert!(next > now());
        assert_eq!(cron.mode, "continuous");
    }

    #[test]
    fn at_requires_exactly_one_of_at_or_after_seconds() {
        let mut input = WakeupInput {
            kind: "at".to_string(),
            instruction: "x".to_string(),
            after_seconds: 3600,
            ..WakeupInput::default()
        };
        assert_eq!(
            validate(&mut input, now()).unwrap(),
            Some(now() + chrono::Duration::seconds(3600))
        );

        let mut both = WakeupInput {
            at: Some(now()),
            after_seconds: 5,
            ..input.clone()
        };
        assert_eq!(err(&mut both), "provide at or after_seconds (1–31536000)");

        let mut neither = WakeupInput {
            after_seconds: 0,
            ..input.clone()
        };
        assert_eq!(
            err(&mut neither),
            "provide at or after_seconds (1–31536000)"
        );

        let mut too_far = WakeupInput {
            after_seconds: 31_536_001,
            ..input
        };
        assert_eq!(
            err(&mut too_far),
            "provide at or after_seconds (1–31536000)"
        );
    }

    #[test]
    fn lifecycle_and_unsupported_events_are_rejected_verbatim() {
        let mut lifecycle = WakeupInput {
            event_types: vec!["issue.created".to_string()],
            ..base_event()
        };
        assert_eq!(
            err(&mut lifecycle),
            "issue.created cannot wake its own issue; subscriptions require an existing, open issue"
        );

        let mut bogus = WakeupInput {
            event_types: vec!["nope.happened".to_string()],
            ..base_event()
        };
        assert_eq!(err(&mut bogus), "unsupported event: nope.happened");
    }

    #[test]
    fn actor_filter_rules_and_agent_alias() {
        let mut wrong_kind = WakeupInput {
            kind: "every".to_string(),
            interval_seconds: 60,
            filter_actor_type: "member".to_string(),
            filter_actor_id: "01930000-0000-7000-8000-000000000002".to_string(),
            ..base_event()
        };
        assert_eq!(
            err(&mut wrong_kind),
            "actor filter requires an event, member or agent type, and actor ID"
        );

        let mut task_event_with_actor = WakeupInput {
            event_types: vec!["task.completed".to_string()],
            filter_actor_type: "agent".to_string(),
            filter_actor_id: "01930000-0000-7000-8000-000000000002".to_string(),
            ..base_event()
        };
        assert_eq!(
            err(&mut task_event_with_actor),
            "actor filters apply to issue, comment, reaction and attachment changes; use agent/run filters for task events"
        );

        // 只有变更事件 + `filter_agent_id` ⇒ 别名成 actor=agent（字段就地改写）。
        let mut alias = WakeupInput {
            filter_agent_id: "01930000-0000-7000-8000-000000000003".to_string(),
            ..base_event()
        };
        assert_eq!(validate(&mut alias, now()).unwrap(), None);
        assert_eq!(alias.filter_actor_type, "agent");
        assert_eq!(
            alias.filter_actor_id,
            "01930000-0000-7000-8000-000000000003"
        );
        assert!(alias.filter_agent_id.is_empty());

        // 混了 `task.*` 就不改写（已有客户端依赖这个形状）。
        let mut mixed = WakeupInput {
            event_types: vec!["task.completed".to_string(), "comment.created".to_string()],
            filter_agent_id: "01930000-0000-7000-8000-000000000003".to_string(),
            ..base_event()
        };
        assert_eq!(validate(&mut mixed, now()).unwrap(), None);
        assert_eq!(
            mixed.filter_agent_id,
            "01930000-0000-7000-8000-000000000003"
        );
        assert!(mixed.filter_actor_type.is_empty());
    }

    #[test]
    fn schedule_ban_and_kind_specific_rules() {
        let mut event_with_schedule = WakeupInput {
            after_seconds: 10,
            ..base_event()
        };
        assert_eq!(
            err(&mut event_with_schedule),
            "event wakeups cannot contain a schedule"
        );

        let mut six_field_cron = WakeupInput {
            kind: "cron".to_string(),
            cron_expression: "0 0 0 0 0 0".to_string(),
            ..base_event()
        };
        assert_eq!(
            err(&mut six_field_cron),
            "cron must have a future occurrence"
        );

        let mut bad_kind = WakeupInput {
            kind: "sometimes".to_string(),
            ..base_event()
        };
        assert_eq!(err(&mut bad_kind), "kind must be event, at, every or cron");

        let mut bad_mode = WakeupInput {
            mode: "forever".to_string(),
            ..base_event()
        };
        assert_eq!(err(&mut bad_mode), "mode must be once or continuous");

        let mut bad_tz = WakeupInput {
            timezone: "Mars/Olympus".to_string(),
            ..base_event()
        };
        assert_eq!(err(&mut bad_tz), "invalid timezone");

        let mut long = WakeupInput {
            instruction: "x".repeat(12_001),
            ..base_event()
        };
        assert_eq!(err(&mut long), "instruction must contain 1–12000 bytes");

        let mut blank = WakeupInput {
            instruction: "   ".to_string(),
            ..base_event()
        };
        assert_eq!(err(&mut blank), "instruction must contain 1–12000 bytes");
    }

    #[test]
    fn severity_and_event_tables_match_upstream() {
        assert_eq!(WAKEUP_EVENT_TYPES.len(), 25);
        assert!(WAKEUP_EVENT_TYPES.contains(&"task.waiting_local_directory"));
        assert_eq!(task_event("running"), "task.started");
        assert_eq!(task_event("completed"), "task.completed");
    }

    #[test]
    fn truncate_for_summary_flattens_whitespace_and_counts_runes() {
        assert_eq!(truncate_for_summary("  a\nb\tc  ", 100), "a b c");
        assert_eq!(truncate_for_summary("中文字", 2), "中文…");
        assert_eq!(truncate_for_summary("abc", 3), "abc");
    }
}
