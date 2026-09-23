//! 删除路径的三个「扁平体」409 拒绝（上游 `runtime.go` / `runtime_blocking_agents.go`
//! / `runtime_profile.go`）。
//!
//! ## 为什么这几个 409 不用本仓的嵌套错误体
//!
//! 本仓标准错误体是 `{"error":{"code","message"}}`（上游是扁平 `{"error":"msg"}`），
//! 但 `runtime_has_active_agents` / `runtime_delete_plan_changed` /
//! `runtime_profile_has_active_agents` 这三个是**前端按 `code` 分支**的：
//! 收到 `runtime_delete_plan_changed` 要重开确认弹窗、收到
//! `runtime_profile_instance_delete_unsupported` 要渲染 profile 级说明。
//! 因此这里逐字复刻上游的扁平体（`error` + `code` + 结构化伴随字段）。
//!
//! ## 阻断 agent 的分类（上游 `runtime_blocking_agents.go`）
//!
//! 「重新指派或归档它们」只对普通 user agent 成立：`archive` 端点直接拒绝任何带
//! `system_key` 的 agent，而 Agent Builder 的隐形载体根本不在 agent 列表里。所以拒绝
//! 文案按 `blocker_class` 分类（**不是** `kind`：Mika 刻意是 `kind='user'` 但
//! product-owned、不可归档）。分类键由 SQL 的 `CASE` 给出（`mc-repos` 的
//! `blocker_class` 列），未知键一律落到 `other_system` —— 宁可说「无法给出办法」，
//! 也不要端出一条做不到的指示。
//!
//! 句子里的 `%q` 用 Rust 的 `{:?}` 等价物（两者都是双引号 + 反斜杠转义）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_repos::runtime::{AgentRuntimeRow, BlockingAgentRow, RuntimeProfileRow};
use serde_json::{Map, Value};

use super::access::timestamp_opt;

/// upstream `service.OfflineRuntimeTTLDays()`（`7 * 24 * 3600` 秒 → 7 天）。
pub(crate) const OFFLINE_RUNTIME_TTL_DAYS: i64 = 7;

/// 拒绝体里最多点名几个阻断 agent（上游 `maxNamedBlockingAgents`）。
const MAX_NAMED_BLOCKING_AGENTS: usize = 5;

/// 删除的两种作用域：实例级（删一个 runtime）与 profile 级（删一个 profile）。
///
/// 差别只在**一条**办法上：把 Mika 挪走能解掉实例级拒绝，但 profile 级拒绝只有当
/// 目的地不是同一 profile 提供的另一个 runtime 时才算解除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteScope {
    Runtime,
    Profile,
}

/// 阻断 agent 的四类（上游 `blockingAgentClass`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockingAgentClass {
    /// 普通 workspace agent：重新指派或归档。
    User,
    /// 内建 Mika：**不可归档，但可以搬走**（`UpdateAgent` 照收 `runtime_id`）。
    Mika,
    /// 未完成的 Agent Builder 流程背后的隐形载体，只有创建者能释放。
    BuilderCarrier,
    /// 其它 product-owned agent：不可归档，本层说不出具体办法。
    OtherSystem,
}

impl BlockingAgentClass {
    /// 上游 `blockingAgentClassFromKey`：只认四个键，其余 → `OtherSystem`。
    fn from_key(key: &str) -> Self {
        match key {
            "user" => Self::User,
            "mika" => Self::Mika,
            "agent_builder" => Self::BuilderCarrier,
            _ => Self::OtherSystem,
        }
    }
}

/// 出现过的分类（固定顺序，保证句子稳定）。
fn classes_of(agents: &[BlockingAgentRow]) -> Vec<BlockingAgentClass> {
    let mut out = Vec::new();
    for class in [
        BlockingAgentClass::User,
        BlockingAgentClass::Mika,
        BlockingAgentClass::BuilderCarrier,
        BlockingAgentClass::OtherSystem,
    ] {
        if agents
            .iter()
            .any(|a| BlockingAgentClass::from_key(&a.blocker_class) == class)
        {
            out.push(class);
        }
    }
    out
}

fn has(classes: &[BlockingAgentClass], wanted: BlockingAgentClass) -> bool {
    classes.contains(&wanted)
}

/// 上游 `blockingAgentLabel`：product-owned 的会额外标注，读者一眼能看出为什么朴素
/// 办法在它身上不成立。
fn label(agent: &BlockingAgentRow, class: BlockingAgentClass) -> String {
    let runtime_name = agent
        .runtime_custom_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&agent.runtime_name);
    let status = &agent.runtime_status;
    match class {
        BlockingAgentClass::BuilderCarrier => {
            format!("an unfinished Agent Builder session on {runtime_name:?} ({status})")
        }
        BlockingAgentClass::Mika | BlockingAgentClass::OtherSystem => {
            format!(
                "{:?} on {runtime_name:?} ({status}, built into Multica)",
                agent.name
            )
        }
        BlockingAgentClass::User => format!("{:?} on {runtime_name:?} ({status})", agent.name),
    }
}

/// 上游 `blockingAgentRemedies`：一类办法一条从句，顺序固定；空结果表示全部是
/// product-owned，调用方应明说「这里没有能做的动作」。
fn remedies(classes: &[BlockingAgentClass], scope: DeleteScope) -> Vec<String> {
    let mixed = has(classes, BlockingAgentClass::Mika)
        || has(classes, BlockingAgentClass::BuilderCarrier)
        || has(classes, BlockingAgentClass::OtherSystem);

    let mut out = Vec::new();
    if has(classes, BlockingAgentClass::User) && mixed {
        out.push(
            "The agents above that are not marked as built into Multica can be reassigned or archived."
                .to_string(),
        );
    } else if has(classes, BlockingAgentClass::User) {
        out.push("Reassign or archive them first.".to_string());
    }
    if has(classes, BlockingAgentClass::BuilderCarrier) {
        out.push(
            "The unfinished Agent Builder session(s) here are hidden from the agent list, and only \
             their creator can open them — ask the member who started the session to switch its \
             runtime or discard it; another admin cannot do that for them."
                .to_string(),
        );
    }
    if has(classes, BlockingAgentClass::Mika) {
        let target = match scope {
            DeleteScope::Profile => "a runtime that this profile does not provide",
            DeleteScope::Runtime => "another runtime",
        };
        out.push(format!(
            "Mika is built into Multica, so it cannot be archived — but it can be moved: open \
             Mika's agent page and bind it to {target}."
        ));
    }
    if has(classes, BlockingAgentClass::OtherSystem) {
        out.push("Some blockers are agents built into Multica and cannot be archived.".to_string());
    }
    out
}

/// 阻断 agent 的线上投影。
///
/// upstream 在 runtime 路径上回的是完整 `AgentResponse`，profile 路径上回的是这个
/// 子集。本切片两条路都用这个子集：确认弹窗只需要「哪个 agent、在哪台机器上」，
/// 而完整 agent 行属于 agent 切片的 DTO（跨切片耦合，见 `docs/39-M3-4-RUNTIME-PROFILES.md` §4）。
fn blocking_agents_value(agents: &[BlockingAgentRow]) -> Value {
    Value::Array(
        agents
            .iter()
            .map(|a| {
                let mut map = Map::new();
                map.insert("id".into(), Value::String(a.id.as_string()));
                map.insert("name".into(), Value::String(a.name.clone()));
                map.insert("kind".into(), Value::String(a.kind.clone()));
                map.insert(
                    "system_key".into(),
                    a.system_key.clone().map_or(Value::Null, Value::String),
                );
                map.insert("runtime_id".into(), Value::String(a.runtime_id.as_string()));
                map.insert("runtime_name".into(), Value::String(a.runtime_name.clone()));
                map.insert(
                    "runtime_custom_name".into(),
                    a.runtime_custom_name
                        .clone()
                        .map_or(Value::Null, Value::String),
                );
                map.insert(
                    "runtime_status".into(),
                    Value::String(a.runtime_status.clone()),
                );
                Value::Object(map)
            })
            .collect(),
    )
}

/// 扁平 409 体（`error` + `code` + 可选伴随字段）。
fn conflict(code: &str, error: String, extra: Vec<(&str, Value)>) -> Response {
    let mut map = Map::new();
    map.insert("error".into(), Value::String(error));
    map.insert("code".into(), Value::String(code.to_string()));
    for (key, value) in extra {
        map.insert(key.to_string(), value);
    }
    (StatusCode::CONFLICT, Json(Value::Object(map))).into_response()
}

// ---------------------------------------------------------------------------
// 实例级拒绝（DELETE /api/runtimes/:id/）
// ---------------------------------------------------------------------------

/// upstream `runtimeHasActiveAgentsResponse`：严格删除遇到活跃 agent。
pub(crate) fn runtime_has_active_agents(agents: &[BlockingAgentRow]) -> Response {
    conflict(
        "runtime_has_active_agents",
        "cannot delete runtime: it has active agents bound to it. Reassign them or confirm \
         unbinding them first."
            .to_string(),
        vec![("active_agents", blocking_agents_value(agents))],
    )
}

/// 确认删除时活跃集合已漂移：换 `code` 让前端知道「你打开弹窗的页面已经过期」，
/// 并把最新快照带上，用户不必再单独拉一次列表。
pub(crate) fn runtime_delete_plan_changed(agents: &[BlockingAgentRow]) -> Response {
    conflict(
        "runtime_delete_plan_changed",
        "the active agent set changed; please review and confirm again.".to_string(),
        vec![("active_agents", blocking_agents_value(agents))],
    )
}

/// 取消之后仍有未完成 task（`service.ErrRuntimeNotDrained`）。
pub(crate) fn runtime_delete_not_drained(scope: DeleteScope) -> Response {
    let error = match scope {
        DeleteScope::Runtime => "the runtime still has tasks in flight; retry in a moment.",
        DeleteScope::Profile => {
            "a runtime of this profile still has tasks in flight; retry in a moment."
        }
    };
    conflict("runtime_delete_not_drained", error.to_string(), Vec::new())
}

/// agent 与 runtime 跨 workspace 绑定（`service.ErrRuntimeWorkspaceMismatch`）。
pub(crate) fn runtime_delete_workspace_mismatch(scope: DeleteScope) -> Response {
    let error = match scope {
        DeleteScope::Runtime => "the runtime has an invalid cross-workspace agent binding.",
        DeleteScope::Profile => {
            "a runtime of this profile has an invalid cross-workspace agent binding."
        }
    };
    conflict(
        "runtime_delete_workspace_mismatch",
        error.to_string(),
        Vec::new(),
    )
}

// ---------------------------------------------------------------------------
// profile 级拒绝（DELETE /api/workspaces/:id/runtime-profiles/:profileId）
// ---------------------------------------------------------------------------

/// upstream `profileDeleteBlockedByAgents`：profile 是 workspace 级的，它的阻断 agent
/// 常常在**另一台**机器上，所以文案要按机器点名 —— 否则用户唯一的出路会变成
/// 「解绑一台本来好好的机器上的 agent」，正是要避免的伤害（GH #8456）。
///
/// `total` 是**全集**计数（有界读取之外的行也在内），办法从句只由全集派生。
pub(crate) fn profile_has_active_agents(
    profile_name: &str,
    agents: &[BlockingAgentRow],
    total: i64,
) -> Response {
    if agents.is_empty() {
        // 调用方只在确有阻断者时才构造拒绝体。
        return conflict(
            "runtime_profile_has_active_agents",
            "cannot delete this custom runtime profile: active agents are still bound to its \
             runtimes."
                .to_string(),
            Vec::new(),
        );
    }

    let classes = classes_of(agents);

    let mut named = Vec::new();
    for agent in agents {
        if named.len() >= MAX_NAMED_BLOCKING_AGENTS {
            continue;
        }
        named.push(label(
            agent,
            BlockingAgentClass::from_key(&agent.blocker_class),
        ));
    }
    let mut listed = named.join(", ");
    let remaining = total - i64::try_from(named.len()).unwrap_or(i64::MAX);
    if remaining > 0 {
        listed = format!("{listed}, and {remaining} more");
    }

    let subject = if profile_name.trim().is_empty() {
        "this custom runtime profile".to_string()
    } else {
        format!("the custom runtime profile {profile_name:?}")
    };

    let mut sentences = vec![format!(
        "cannot delete {subject}: {total} active agent(s) are still bound to its runtimes — \
         {listed}."
    )];
    let mut remedied = remedies(&classes, DeleteScope::Profile);
    if remedied.is_empty() {
        remedied = vec!["None of them can be released from here.".to_string()];
    }
    sentences.extend(remedied);
    sentences.push(
        "Deleting this profile removes its runtime on every machine that registered it, so agents \
         on a machine you did not intend to touch will be affected too."
            .to_string(),
    );

    let shown = i64::try_from(named.len()).unwrap_or(i64::MAX);
    conflict(
        "runtime_profile_has_active_agents",
        sentences.join(" "),
        vec![
            ("active_agents", blocking_agents_value(agents)),
            ("active_agent_count", Value::from(total)),
            ("active_agents_truncated", Value::from(total > shown)),
        ],
    )
}

// ---------------------------------------------------------------------------
// profile 实例拒绝（删不掉的那单个 runtime 行）
// ---------------------------------------------------------------------------

/// 阻止 retention GC 回收该 runtime 的东西（上游 `profileInstanceBlockers`）。
///
/// 比「有没有活跃 agent」多一项：GC 还要**跨全部 user agent（含归档）**复查 drain，
/// 只报活跃集合会许下一个 sweeper 会跳过的承诺。
#[derive(Debug, Default)]
pub(crate) struct InstanceBlockers {
    pub(crate) agents: Vec<BlockingAgentRow>,
    pub(crate) undrained_tasks: i64,
    /// 读取失败 → `false`，此时只承诺与阻断集无关的那部分。
    pub(crate) known: bool,
}

/// upstream `profileInstanceDeleteRefusal`：解释为什么这一行不能单独删，以及**该改做什么**。
///
/// 旧文案只说「去删它的 runtime profile 吧」，对这个错误最常见的场景（某台退役机器
/// 留下的行，而 profile 还挂在其它健康机器上）是**有害建议**：照做会一并删掉那些机器
/// 的 runtime，而且只要还有 agent 绑着就会被拒。所以新版先讲用户真正想要的结果 ——
/// 离线的行会被自动回收 —— 再说明 profile 删除的波及范围，而不是推荐它。
pub(crate) fn profile_instance_delete_unsupported(
    rt: &AgentRuntimeRow,
    profile: &RuntimeProfileRow,
    blockers: &InstanceBlockers,
) -> Response {
    let ttl_days = OFFLINE_RUNTIME_TTL_DAYS;
    let name = rt
        .custom_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&rt.name);

    let mut parts = vec![format!(
        "cannot delete {name:?} on its own: it is registered from the custom runtime profile \
         {:?}.",
        profile.display_name
    )];

    if rt.status == "online" {
        parts.push(format!(
            "It is still online, so its daemon would register it again. Stop that daemon first; \
             Multica then removes the runtime automatically after {ttl_days} days offline, once no \
             agent is bound to it and nothing is still running on it."
        ));
    } else if !blockers.known {
        parts.push(format!(
            "It is offline, and Multica removes offline runtimes automatically after {ttl_days} \
             days, once no agent is bound to them and nothing is still running on them."
        ));
    } else if !blockers.agents.is_empty() || blockers.undrained_tasks > 0 {
        // GC 需要两者都清掉：只点 agent 会让清了 agent 的用户一周后原地打转，
        // 还在等一个没人告诉过他的 task。
        let mut holds = Vec::new();
        if !blockers.agents.is_empty() {
            holds.push(format!(
                "{} agent(s) are still bound to it",
                blockers.agents.len()
            ));
        }
        if blockers.undrained_tasks > 0 {
            holds.push(format!(
                "{} unfinished task(s) belong to it or to agents bound to it",
                blockers.undrained_tasks
            ));
        }
        parts.push(format!(
            "It is offline, but {} holds it in place; Multica removes the runtime automatically \
             after {ttl_days} days offline once that is cleared.",
            holds.join(" and ")
        ));
        parts.extend(remedies(
            &classes_of(&blockers.agents),
            DeleteScope::Runtime,
        ));
        if blockers.undrained_tasks > 0 {
            parts.push(
                "Let those tasks finish, or cancel them — one can be running on a different \
                 machine if its agent was moved there."
                    .to_string(),
            );
        }
    } else {
        parts.push(format!(
            "It is offline with no agents bound and nothing still running on it, so Multica \
             removes it automatically after {ttl_days} days offline — this row will be reclaimed \
             without any action from you."
        ));
    }
    parts.push(
        "Deleting the profile instead would remove this runtime on every machine that registered \
         it, not just this one."
            .to_string(),
    );

    let mut extra = vec![
        ("profile_id", Value::String(profile.id.as_string())),
        ("profile_name", Value::String(profile.display_name.clone())),
        ("runtime_status", Value::String(rt.status.clone())),
        (
            "last_seen_at",
            timestamp_opt(rt.last_seen_at).map_or(Value::Null, Value::String),
        ),
        ("auto_cleanup_after_days", Value::from(ttl_days)),
    ];
    if blockers.known {
        extra.push((
            "active_agent_count",
            Value::from(i64::try_from(blockers.agents.len()).unwrap_or(i64::MAX)),
        ));
        extra.push((
            "undrained_task_count",
            Value::from(blockers.undrained_tasks),
        ));
    }

    conflict(
        "runtime_profile_instance_delete_unsupported",
        parts.join(" "),
        extra,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_core::Id;

    fn agent(name: &str, class: &str, runtime_name: &str) -> BlockingAgentRow {
        BlockingAgentRow {
            id: Id::new(),
            name: name.to_string(),
            kind: "user".to_string(),
            system_key: None,
            runtime_id: Id::new(),
            runtime_name: runtime_name.to_string(),
            runtime_custom_name: None,
            runtime_status: "offline".to_string(),
            blocker_class: class.to_string(),
            total_count: 1,
            user_count: i64::from(class == "user"),
            mika_count: i64::from(class == "mika"),
            agent_builder_count: i64::from(class == "agent_builder"),
            other_system_count: i64::from(class == "other_system"),
        }
    }

    /// 未知键必须落到 `other_system`：只有 `user` 才能派生那条「可以重新指派或归档」的
    /// 办法（上游注释里点名的失败模式）。
    #[test]
    fn unknown_blocker_class_falls_back_to_other_system() {
        assert_eq!(
            BlockingAgentClass::from_key("something-new"),
            BlockingAgentClass::OtherSystem
        );
        assert_eq!(
            BlockingAgentClass::from_key("user"),
            BlockingAgentClass::User
        );
    }

    #[test]
    fn remedies_distinguish_user_from_product_owned() {
        let only_user = remedies(&[BlockingAgentClass::User], DeleteScope::Runtime);
        assert_eq!(
            only_user,
            vec!["Reassign or archive them first.".to_string()]
        );

        let mixed = remedies(
            &[BlockingAgentClass::User, BlockingAgentClass::Mika],
            DeleteScope::Profile,
        );
        assert!(mixed[0].starts_with("The agents above that are not marked"));
        assert!(mixed[1].contains("this profile does not provide"));

        assert!(
            remedies(&[BlockingAgentClass::OtherSystem], DeleteScope::Runtime)
                .iter()
                .all(|line| !line.contains("Reassign"))
        );
    }

    #[test]
    fn profile_labels_use_the_custom_name_when_present() {
        let mut row = agent("Mika", "mika", "daemon-name");
        row.runtime_custom_name = Some("  ".to_string() + "my laptop");
        assert_eq!(
            label(&row, BlockingAgentClass::Mika),
            "\"Mika\" on \"my laptop\" (offline, built into Multica)"
        );
    }
}
