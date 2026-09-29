//! 回放器的**通用种子装配**：把 fixture 按字面量引用、却从未被种下的实体行建出来。
//!
//! # 这份装配是为了把一句话变成真的
//!
//! §205.1 定下的判别式是「凡主语是 harness 的前提都是待办装配，不是能力缺口」。
//! 本文件是那条判别式在 **database 层**的落地。
//!
//! # 🔴 但先说清楚：这些 id 原本**不是**「少种了一行」
//!
//! 门 ⑨ 里有 100 条 `→404`，看上去像是「引用的实体行从未种下」。**实测不是。**
//! 抽取器的 `package_literals`（现搬在 `scripts/extract_borrowed_ids.py`）是
//! **全仓扫描 + `setdefault`**，所以某个测试文件里函数内的
//! ``const agentID = "<uuid>"`` 会成为**全仓**名字 `agentID` 的取值。于是
//! `"/api/agents/" + agentID` 这条 URL 带上了从**另一个测试**里抄来的 UUID，
//! 而上游真正的意图是访问它刚用 `dbfx.Agent(...)` 建出来的那一行。
//!
//! 这就是为什么单纯「把那一行种下去」是错的：种出来的行和 URL 里的 id 仍然是两件事。
//! 所以真正的修复在抽取侧（`extract_borrowed_ids.seeded_symbol_for`）：识别出这种
//! **借来的**行 id，按**路由的集合段**（不是 Go 变量名 —— 变量名正是撞号的东西）
//! 判成 `$test<Kind>ID`，由本文件把那一类行**用真实路由**建出来，回放时绑定到它。
//!
//! # 纪律：种子行必须由真实路由建
//!
//! 与 `harness::database_router` 建 workspace 时同一条纪律（那里是因为仓库层没有
//! workspace create API）。种子行的形状因此永远和 handler 期望的一致 —— 我们不会
//! 手写 `INSERT` 去猜一张表的列。**唯一**的例外是 runtime，见 [`seed_runtime`]。
//! # 每类**每个测试**一行（§213）
//!
//! §209 的第一版是「每类**一行**，全回放共享」。那一刻的权衡是真的：上游的 CRUD
//! 链（`TestIssuesCRUDThroughRouter`：create → get → put → put → list → delete →
//! get 404）正是靠「同一个 id 贯穿全链」才成立的，给每条 fixture 各建一行会把
//! delete-then-get 变成 get-200。
//!
//! 但「共享」的粒度选错了：跨测试共享让**独立测试之间互相摧毁**。replay 序 57 的
//! chat 用例删掉了唯一的 session、序 206 的 issues 用例删掉了唯一的 issue、
//! 序 335 的 workspace 用例删掉了唯一的 workspace —— 其后引用同一符号的 fixture
//! 一起 404，23 条，登记在 docs/37 §209.4 的 C 桶。
//!
//! 修法是把共享的粒度从**回放**降到**测试**：分组键 = `Fixture.source.test`
//! （`lib.rs` 的 `Source::test`；300 个上游测试名全仓没有一个重名）。每个分组独占
//!
//! * 一个 workspace：`$testWorkspaceID` 就是它（workspace 被 `DELETE` 摧毁同样是
//!   分组内的破坏，见 C 桶里那 5 条 `workspaces/…`）；
//! * 一枚 `mdt_` 令牌：daemon 身份被限定在某个 workspace 内，令牌因此也得按组分
//!   （见 [`crate::daemon_token::register`]）；
//! * 四行实体：agent / issue / chat session / task。
//!
//! 于是「同一个 id 贯穿全链」在**分组之内**仍然成立，而分组之间的 `DELETE` 只摧毁
//! 自己那一份。`""` 是兜底分组：`Bindings` 拿它的 workspace 与令牌当默认值。
//!
//! 要种几个分组由 [`groups_for`] 从 fixture 面算出 —— **只有**真的引用了那五类符号
//! 的测试才被种，不是给 300 个测试各建一套。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use crate::Fixture;

/// 兜底分组的键。它不是任何测试的名字，只用来承载 `Bindings` 的默认 workspace /
/// 默认令牌 —— 不引用任何种子符号的 fixture 从不落到别的分支上。
pub const DEFAULT_GROUP: &str = "";

/// 会让一个分组需要**自己那套行**的符号：四类实体，加上 workspace。
///
/// `$testWorkspaceID` 与那四类的解析面不同（它由 `Bindings` 直接持有，不是本文件种
/// 出来的行），但**它同样是「被 `DELETE` 摧毁的共享行」** —— C 桶里 5 条
/// `workspaces/…` 就是它，所以它也必须按组分。
pub const GROUP_SYMBOLS: [&str; 5] = [
    "$testAgentID",
    "$testIssueID",
    "$testChatSessionID",
    "$testTaskID",
    "$testWorkspaceID",
];

/// 一个[分组](groups_for)独占的那几行。
///
/// `Debug` 手写而非 derive：`daemon_token` 是明文凭据（只活在内存里），derive 会把它
/// 逐字打进任何一条 `{:?}`；而「哪几类种到了」正是排查时要先看的东西，所以逐字段列。
#[derive(Clone)]
pub struct GroupSeed {
    pub workspace_id: Uuid,
    pub daemon_token: String,
    pub agent: Uuid,
    pub issue: Uuid,
    pub chat_session: Uuid,
    pub task: Uuid,
}

impl std::fmt::Debug for GroupSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupSeed")
            .field("workspace_id", &self.workspace_id)
            .field("daemon_token", &"mdt_<redacted>")
            .field("agent", &self.agent)
            .field("issue", &self.issue)
            .field("chat_session", &self.chat_session)
            .field("task", &self.task)
            .finish()
    }
}

/// 一个**不拥有任何被种资源**的 workspace ＋ 它里面那枚 `mdt_` 令牌。
///
/// 上游跨空间 daemon 探针注入的身份就是这种形状（见 [`crate::upstream_facts`]）。
/// `Debug` 手写脱敏的理由与 [`GroupSeed`] 同：`daemon_token` 是明文凭据。
#[derive(Clone)]
pub struct Outsider {
    /// outsider workspace 的 id（报告里可以出现 —— 它是 id 级信息）。
    pub workspace_id: Uuid,
    /// 登记在 outsider workspace 里的 `mdt_…` 明文。**不写进 `report.json`**。
    pub daemon_token: String,
}

impl std::fmt::Debug for Outsider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outsider")
            .field("workspace_id", &self.workspace_id)
            .field("daemon_token", &"mdt_<redacted>")
            .finish()
    }
}

/// 全部种子：**分组键 → 那一组的行**。键是 `Fixture.source.test`，`""` 是兜底分组。
///
/// 字段名与 `Bindings` 里的符号名一一对应（`$testAgentID` ↔ [`GroupSeed::agent`]）。
#[derive(Clone, Default)]
pub struct Seed {
    groups: BTreeMap<String, GroupSeed>,
    /// 全回放共享的**跨空间**身份；只在 [`crate::upstream_facts::needs_outsider`]
    /// 为真时才建（见 [`seed`]）。
    outsider: Option<Outsider>,
}

impl std::fmt::Debug for Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seed")
            .field("groups", &self.groups.len())
            .field("outsider", &self.outsider)
            .finish()
    }
}

impl Seed {
    /// 抽取器只发这四个「实体行」符号。`$testWorkspaceID` 在这里**不是**一行，而是
    /// 「每个分组一个 workspace」（见 [`GROUP_SYMBOLS`]）。
    pub const SYMBOLS: [&str; 4] = [
        "$testAgentID",
        "$testIssueID",
        "$testChatSessionID",
        "$testTaskID",
    ];

    /// 一个符号在**该分组**里的那一行。
    ///
    /// 未种出的（含分组不存在）一律 `None`，由 [`crate::Bindings::resolve`] 变成
    /// 「unbound symbol」错误：猜一个 UUID 会让请求落在一个不存在的行上，症状是
    /// `404`，而 `404` 与「这条 fixture 本来就不该过」在报告里**长得一样**。
    #[must_use]
    pub fn get(&self, group: &str, symbol: &str) -> Option<Uuid> {
        let rows = self.groups.get(group)?;
        match symbol {
            "$testAgentID" => Some(rows.agent),
            "$testIssueID" => Some(rows.issue),
            "$testChatSessionID" => Some(rows.chat_session),
            "$testTaskID" => Some(rows.task),
            _ => None,
        }
    }

    /// 该分组的 workspace —— `$testWorkspaceID` 就是它。
    #[must_use]
    pub fn workspace(&self, group: &str) -> Option<Uuid> {
        self.groups.get(group).map(|rows| rows.workspace_id)
    }

    /// 该分组的 `mdt_` 明文令牌。
    #[must_use]
    pub fn daemon_token(&self, group: &str) -> Option<&str> {
        self.groups
            .get(group)
            .map(|rows| rows.daemon_token.as_str())
    }

    /// 种了几个分组（含兜底的那一个）。
    #[must_use]
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// 本次回放里**跨空间**身份所在 workspace 的 id（没建 outsider 时为 `None`）。
    #[must_use]
    pub fn outsider_workspace(&self) -> Option<Uuid> {
        self.outsider.as_ref().map(|o| o.workspace_id)
    }

    /// outsider workspace 里那枚令牌的 `mdt_…` 明文。
    #[must_use]
    pub fn outsider_daemon_token(&self) -> Option<&str> {
        self.outsider.as_ref().map(|o| o.daemon_token.as_str())
    }

    /// 挂上 outsider 身份（[`crate::seed_catalog::seed_outsider`] 的产物）。
    #[must_use]
    pub fn with_outsider(mut self, outsider: Outsider) -> Self {
        self.outsider = Some(outsider);
        self
    }

    /// 手工装一个分组。
    ///
    /// 生产的唯一入口是 [`seed`]；这个只开在 `#[cfg(test)]` 下，好让 `bindings` 的
    /// 单测能造出一份种子 —— 那些断言要验的是**解析规则**，不该为此起一个真库。
    #[cfg(test)]
    #[must_use]
    pub fn with_group(mut self, group: &str, rows: GroupSeed) -> Self {
        self.groups.insert(group.to_string(), rows);
        self
    }
}

/// 分组键 = `Fixture.source.test`。
///
/// 用它而不是 `id`：`id` 是 `<domain>/<test>@<file>:<line>#<n>`，**同一条上游测试的
/// 多次请求**（CRUD 链）会落在不同 `id` 上，按 `id` 分组等于没分组。`source.test`
/// 才是「同一个上游测试」这件事的键。
#[must_use]
pub fn group_of(fx: &Fixture) -> &str {
    &fx.source.test
}

/// 需要**自己一套行**的分组键，按字典序。
///
/// 判据是「这条 fixture 在 [`crate::plan`] 会解析到的位置里引用了 [`GROUP_SYMBOLS`]
/// 之一」。刻意扫得比 `plan()` 宽（连 `body` 与 `path` 一起扫）：多算一个分组只会
/// 多建一套行，少算一个分组则会让那批 fixture 静默变成 `unbound symbol` →
/// `unevaluable`（§205.5 纪律：不可判定与判定为过在总数里长得一样）。
#[must_use]
pub fn groups_for(fixtures: &[Fixture]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for fx in fixtures {
        let used = referenced_symbols(fx);
        if GROUP_SYMBOLS.iter().any(|sym| used.contains(*sym)) {
            out.insert(fx.source.test.clone());
        }
    }
    out.into_iter().collect()
}

/// `plan()` 会送去 [`crate::Bindings::resolve`] 的全部取值位置。
fn referenced_symbols(fx: &Fixture) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_symbols(&fx.path, &mut out);
    for raw in fx.path_params.values() {
        collect_symbols(raw, &mut out);
    }
    for raw in fx.query.values() {
        collect_symbols(raw, &mut out);
    }
    for raw in fx.headers.values() {
        collect_symbols(raw, &mut out);
    }
    for raw in fx.actor.upstream_identity.values() {
        collect_symbols(raw, &mut out);
    }
    collect_json_symbols(fx.body.as_ref(), &mut out);
    out
}

/// 扫出一个字符串里所有 `$name` 形态的符号（`$` + `[A-Za-z0-9_]*`）。
fn collect_symbols(raw: &str, out: &mut BTreeSet<String>) {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        out.insert(raw[start..i].to_string());
    }
}

/// `body` 是任意 JSON：逐层走到字符串上再扫符号。
fn collect_json_symbols(value: Option<&serde_json::Value>, out: &mut BTreeSet<String>) {
    match value {
        Some(serde_json::Value::String(s)) => collect_symbols(s, out),
        Some(serde_json::Value::Array(items)) => {
            for item in items {
                collect_json_symbols(Some(item), out);
            }
        }
        Some(serde_json::Value::Object(map)) => {
            for item in map.values() {
                collect_json_symbols(Some(item), out);
            }
        }
        _ => {}
    }
}

/// 给 `groups` 里每个分组各建一套行（workspace + `mdt_` 令牌 + 四行实体）。
///
/// 任一行种不出来就整体失败 —— 半套种子会让一部分 fixture 变成「看起来判过了」的
/// 假象，那比明确报错糟得多。
pub async fn seed(
    router: &Router,
    db: &mc_db::pool::Db,
    user_id: Uuid,
    groups: &[String],
) -> Result<Seed> {
    let user = user_id.to_string();
    // 本次回放的 nonce：workspace 的 name/slug 必须**跨并发回放**唯一 —— 同一个测试库里
    // 可能同时跑着两条 `database_router`（`cargo test` 默认并行），slug 撞车会让后一条
    // 的 `POST /api/workspaces` 直接失败。
    let run = Uuid::new_v4().to_string().replace('-', "");
    let run = run[..8].to_string();
    let mut seed = Seed::default();
    // 兜底分组先建：`Bindings` 的默认 workspace / 默认令牌取它的那一份。
    let fallback = seed_group(router, db, &user, user_id, DEFAULT_GROUP, 0, &run).await?;
    seed.groups.insert(DEFAULT_GROUP.to_string(), fallback);
    for (i, group) in groups.iter().enumerate() {
        if seed.groups.contains_key(group) {
            continue;
        }
        let rows = seed_group(router, db, &user, user_id, group, i + 1, &run).await?;
        seed.groups.insert(group.clone(), rows);
    }
    // 跨空间 daemon 身份：只在语料真的点了名（[`crate::upstream_facts`]）时才建 ——
    // 没这一条需求时连建都不建，免得每次回放白加一个 workspace。
    if crate::upstream_facts::needs_outsider(groups) {
        let outsider = crate::seed_catalog::seed_outsider(router, db, &user, &run).await?;
        seed.outsider = Some(outsider);
    }
    Ok(seed)
}

/// 一个分组的一整套行。顺序在这里是**语义**而不是风格：workspace 是四行的容器，
/// 而 runtime ⇒ agent ⇒ chat session ⇒ task 每一环都依赖前一环。
///
/// `index` 只用来让同一次回放里的 workspace 名字/slug 互不相同 —— 分组键本身可能很长、
/// 带大小写与斜杠，不是合法的 slug。
async fn seed_group(
    router: &Router,
    db: &mc_db::pool::Db,
    user: &str,
    user_id: Uuid,
    group: &str,
    index: usize,
    run: &str,
) -> Result<GroupSeed> {
    let workspace_id = seed_workspace(router, user, group, index, run).await?;
    // 上游 `createTestCustomStatus` 直接 INSERT `issue_status`，抽取器只抽 HTTP 调用
    // ⇒ 目录项得由装置补（且**只补**上游真建过的那些，见 `upstream_facts.rs`）。
    crate::seed_catalog::seed_custom_statuses(router, user, workspace_id, group).await?;
    let runtime_id = seed_runtime(db, user_id, workspace_id).await?;
    let agent = seed_agent(router, user, workspace_id, runtime_id).await?;
    let issue = seed_issue(router, user, workspace_id).await?;
    let chat_session = seed_chat_session(router, user, workspace_id, agent).await?;
    let task = seed_task(router, user, workspace_id, agent, chat_session).await?;
    // 🔴 令牌在 workspace 建好**之后**才签发：`daemon_token.workspace_id` 有指向
    // `workspace(id)` 的外键，而这条外键正是「daemon 身份被限定在某个 workspace 内」
    // 的地基（§209.2 的同一段纪律，现在按分组各签一枚）。
    let daemon = crate::daemon_token::register(
        &mc_repos::daemon::DaemonRepo::new(db),
        mc_core::Id::from(workspace_id),
    )
    .await
    .with_context(|| format!("register daemon token for seed group {group:?}"))?;
    Ok(GroupSeed {
        workspace_id,
        daemon_token: daemon.raw,
        agent,
        issue,
        chat_session,
        task,
    })
}

/// 用真实路由建一个 workspace（`POST /api/workspaces` 会自动把创建者加成 owner，
/// 所以种子身份一定是 owner 成员 —— 与 `harness::database_router` 同一条纪律）。
pub(crate) async fn seed_workspace(
    router: &Router,
    user: &str,
    group: &str,
    index: usize,
    run: &str,
) -> Result<Uuid> {
    let suffix = if group.is_empty() {
        "default".to_string()
    } else {
        format!("g{index:04}")
    };
    // `run` 是跨并发回放唯一的 nonce（见 [`seed`]）；`suffix` 只区分**同一次**回放里的分组。
    let payload = json!({
        "name": format!("Conformance {run}-{suffix}"),
        "slug": format!("conformance-{run}-{suffix}"),
    });
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workspaces")
                .header("content-type", "application/json")
                .header(SEED_SESSION_HEADER, user)
                .header(SEED_DEV_USER_HEADER, user)
                .body(Body::from(payload.to_string()))?,
        )
        .await
        .context("dispatch POST /api/workspaces")?;
    let status = resp.status();
    let body = to_bytes(resp.into_body(), 1 << 20).await?;
    if status != StatusCode::CREATED {
        bail!(
            "seeding a workspace for seed group {group:?} failed: {status} {}",
            String::from_utf8_lossy(&body)
        );
    }
    let created: serde_json::Value = serde_json::from_slice(&body)?;
    let id = created["id"]
        .as_str()
        .with_context(|| format!("workspace id missing from create response: {created}"))?;
    Uuid::parse_str(id).context("workspace id is not a uuid")
}

/// 🔴 **本文件唯一的非路由种子**，且这是一个被记录在案的缺口，不是抄近路。
///
/// `POST /api/agents` 要求 `runtime_id` 指向一个**属于该 workspace 且调用者可用**的
/// runtime（`routes/agents/crud.rs::create_agent` 的 `scope.runtime_binding` +
/// `can_use_runtime` 两道门）。而 runtime 在上游是**daemon 注册**的实体：本仓
/// `routes/runtimes.rs` 只注册了 list / patch / delete / usage×3 / unbind / archive，
/// **没有** `POST /api/runtimes` —— 换句话说，缺的不是仓储能力
/// （`AgentRuntimeRepo::create` 就在 `mc-repos/src/runtime/ledger.rs:252`），
/// 缺的是把 daemon 注册面暴露成一条 HTTP 路由。
///
/// 那条路由属于「路由面打满之后才能加」的活（⑦ `known_gap = 0` ⇒ 本仓**不许**新增
/// 注册路由），所以本片不能靠加路由解决。写下这一段是为了让下一个读代码的人知道
/// **为什么这里是仓储调用而上面每一个都是路由调用**。
async fn seed_runtime(db: &mc_db::pool::Db, user_id: Uuid, workspace_id: Uuid) -> Result<Uuid> {
    let repo = mc_repos::runtime::AgentRuntimeRepo::new(db.clone());
    let row = repo
        .create(mc_repos::runtime::NewAgentRuntime {
            workspace_id: mc_core::Id::from(workspace_id),
            daemon_id: None,
            name: "conformance-runtime".into(),
            runtime_mode: "local".into(),
            provider: "conformance".into(),
            owner_id: Some(mc_core::Id::from(user_id)),
            profile_id: None,
            custom_name: None,
        })
        .await
        .context("create agent_runtime for the seed agent")?;
    Ok(row.id.0)
}

/// `POST /api/agents` → 一行 agent。
///
/// 刻意**不**传 `visibility`：上游 `visibility == "" ⇒ private`，而种子身份正是
/// 它的 owner，所以 owner 面（多数 fixture）与「非本人 ⇒ 403」面同时成立 —— 与
/// 上游的私有 agent 语义一致。
async fn seed_agent(
    router: &Router,
    session: &str,
    workspace_id: Uuid,
    runtime_id: Uuid,
) -> Result<Uuid> {
    let body = json!({
        "name": "Conformance Agent",
        "description": "seeded by the conformance replayer",
        "runtime_id": runtime_id.to_string(),
    });
    post_json(
        router,
        session,
        &format!("/api/agents?workspace_id={workspace_id}"),
        &body,
    )
    .await
    .context("seed agent: POST /api/agents")
}

/// `POST /api/issues` → 一行 issue。
async fn seed_issue(router: &Router, user: &str, workspace_id: Uuid) -> Result<Uuid> {
    let body = json!({ "title": "Conformance Issue", "status": "todo" });
    post_json(
        router,
        user,
        &format!("/api/issues?workspace_id={workspace_id}"),
        &body,
    )
    .await
    .context("seed issue: POST /api/issues")
}

/// `POST /api/chat/sessions` → 一行 chat session（上游要求 `agent_id` 非空）。
async fn seed_chat_session(
    router: &Router,
    session: &str,
    workspace_id: Uuid,
    agent: Uuid,
) -> Result<Uuid> {
    let body = json!({ "agent_id": agent.to_string() });
    post_json(
        router,
        session,
        &format!("/api/chat/sessions?workspace_id={workspace_id}"),
        &body,
    )
    .await
    .context("seed chat session: POST /api/chat/sessions")
}

/// 一行 `agent_task_queue`。
///
/// 🔴 **本仓的派单面不入队。** `TaskRepo::create_task`（`mc-repos/src/task/store.rs:212`）
/// 在 `mc-http` 里**零调用点**（实测 `grep -rn "create_task(" crates/mc-http/src` 无命中）
/// —— 把 issue 派给 agent 只写 `issue.assignee_*`，**不会**产生任务行。所以「派单产生
/// 任务」这个假设在本仓是错的，本函数一开始就是照它写的，结果种出一只空列表。
///
/// 唯一会往 `agent_task_queue` 插行的**路由**是 chat 发送面
/// （`ChatTaskRepo::send_direct_chat_message` → `mc-repos/src/chat_task/send.rs:136`），
/// 所以这里发一条聊天消息，再把任务读回来。全程仍然只用真实路由，没有 `INSERT`。
async fn seed_task(
    router: &Router,
    user: &str,
    workspace_id: Uuid,
    agent: Uuid,
    chat_session: Uuid,
) -> Result<Uuid> {
    let send = json!({ "content": "seed the conformance task queue" });
    let uri = format!("/api/chat/sessions/{chat_session}/messages?workspace_id={workspace_id}");
    // 发送面不返回 id（返回的是消息 DTO），所以只关心它有没有成功。
    send_json(router, user, &uri, &send)
        .await
        .context("seed task: POST /api/chat/sessions/:id/messages")?;

    let uri = format!("/api/agents/{agent}/tasks?workspace_id={workspace_id}");
    let resp = router
        .clone()
        .oneshot(get(&uri, user)?)
        .await
        .context("seed task: GET /api/agents/:id/tasks")?;
    let status = resp.status();
    let body = to_bytes(resp.into_body(), 1 << 20).await?;
    if status != StatusCode::OK {
        bail!("listing the seed agent's tasks failed: {status} {body:?}");
    }
    let rows: serde_json::Value = serde_json::from_slice(&body)?;
    let first = rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|t| t.get("id"))
        .and_then(serde_json::Value::as_str)
        .context("the seed agent has no task; the chat send did not enqueue one")?;
    Uuid::parse_str(first).context("task id is not a uuid")
}

/// 发一次 `GET`。
///
/// 🔴 刻意**不**用 `expect`：URI 里带着刚种出来的那几个 id，它们虽然来自我们自己的
/// `Uuid`，但一旦哪一步把它们变成了非法 URI，这里 panic 会把整个回放变成一条
/// 与种子问题毫无字面关系的崩溃信息。回退成 `Err` 才能保住「种子失败 ⇒ 明确报错」。
fn get(uri: &str, user: &str) -> Result<Request<Body>> {
    Ok(Request::builder()
        .method("GET")
        .uri(uri)
        .header(SEED_SESSION_HEADER, user)
        .header(SEED_DEV_USER_HEADER, user)
        .body(Body::empty())?)
}

/// 🔴 两个身份头都要发，与 `plan()` 同一条纪律。
///
/// session 中间件把 `X-Multica-Session` 解析成用户，而 M1 dev-mode 的 `AuthUser`
/// 提取器直接读 `X-Multica-User-Id`；只发一个的后果不是 403 而是 **401**
/// （「missing X-Multica-User-Id header」），而 401 在种子里看起来像「路由没挂」。
pub(crate) const SEED_SESSION_HEADER: &str = "x-multica-session";
pub(crate) const SEED_DEV_USER_HEADER: &str = "x-multica-user-id";

/// 发一次 `POST`，返回响应体里的 `id`。
///
/// 🔴 失败时把状态码与响应体一起打出来：种子的形状错了只能在这里看见，
/// 变成后面几百条 `404` 就再也定位不到了。
async fn post_json(
    router: &Router,
    user: &str,
    uri: &str,
    body: &serde_json::Value,
) -> Result<Uuid> {
    let value = send_json(router, user, uri, body).await?;
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("POST {uri} response has no `id`: {value}"))?;
    Uuid::parse_str(id).with_context(|| format!("POST {uri} returned a non-uuid id: {id}"))
}

/// 发一次 `POST`，只关心它成不成功。
async fn send_json(
    router: &Router,
    user: &str,
    uri: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header(SEED_SESSION_HEADER, user)
                .header(SEED_DEV_USER_HEADER, user)
                .body(Body::from(body.to_string()))?,
        )
        .await
        .with_context(|| format!("POST {uri}"))?;
    let status = resp.status();
    let raw = to_bytes(resp.into_body(), 1 << 20).await?;
    if !status.is_success() {
        bail!("POST {uri} -> {status} {}", String::from_utf8_lossy(&raw));
    }
    serde_json::from_slice(&raw).with_context(|| {
        format!(
            "POST {uri} did not return JSON: {}",
            String::from_utf8_lossy(&raw)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 造一条最小 fixture：只填 `groups_for` 会看的那些字段，以及 `Fixture::verify`
    /// 会看的那几个。用它来验分组键，不必依赖 `contracts/golden/**` 的具体内容。
    fn fx(test: &str, path_params: &[(&str, &str)]) -> Fixture {
        let mut value = json!({
            "schema_version": crate::SCHEMA_VERSION,
            "id": format!("dom/{test}@a.go:1#1"),
            "method": "GET",
            "path": "/api/things/{id}",
            "actor": { "kind": "anonymous" },
            "expect": { "status": 200 },
            "source": { "file": "a.go", "line": 1, "test": test, "site": "handler" },
        });
        if !path_params.is_empty() {
            let mut map = serde_json::Map::new();
            for (k, v) in path_params {
                map.insert((*k).to_string(), json!(v));
            }
            value["path_params"] = serde_json::Value::Object(map);
        }
        serde_json::from_value(value).expect("minimal fixture")
    }

    #[test]
    fn every_emitted_symbol_maps_to_exactly_one_seed_field() {
        // 承重：抽取器 `SEEDED_COLLECTIONS` 发出的符号必须一个不漏地在这里落到字段上。
        // 漏一个的表现是那批 fixture 全部变成 `unbound symbol` → unevaluable，
        // 而 unevaluable 在总数里**不显眼**（§205.5 纪律：不可判定与判定为过长得一样）。
        let group = "TestSeededRows";
        let mut seed = Seed::default();
        seed.groups.insert(
            group.to_string(),
            GroupSeed {
                workspace_id: Uuid::from_u128(9),
                daemon_token: "mdt_placeholder".into(),
                agent: Uuid::from_u128(1),
                issue: Uuid::from_u128(2),
                chat_session: Uuid::from_u128(3),
                task: Uuid::from_u128(4),
            },
        );
        for sym in Seed::SYMBOLS {
            assert!(seed.get(group, sym).is_some(), "{sym} 没有落到任何字段");
        }
        // workspace 不是「一行实体」：它是**分组自己的那个容器**，所以走 `workspace()`。
        assert_eq!(seed.get(group, "$testWorkspaceID"), None);
        assert_eq!(seed.workspace(group), Some(Uuid::from_u128(9)));
        assert_eq!(seed.get(group, "$testProjectID"), None);
        // 未种出的分组一律 None：报告里「不可判定」必须可区分于「种到了但请求失败」。
        assert_eq!(seed.get("TestNotSeeded", "$testIssueID"), None);
        assert_eq!(seed.workspace("TestNotSeeded"), None);
        assert_eq!(seed.group_count(), 1);
    }

    #[test]
    fn groups_for_picks_exactly_the_tests_that_reference_a_group_symbol() {
        // 承重：分组多算一个只是多建一套行，少算一个会让那批 fixture 静默变 unevaluable。
        // 所以这条钉住**两个方向**：引用了的必须进，没引用的必须不进。
        let fixtures = vec![
            fx("TestUsesIssue", &[("id", "$testIssueID")]),
            fx("TestUsesIssueAgain", &[("id", "$testIssueID")]),
            fx("TestUsesWorkspace", &[("id", "$testWorkspaceID")]),
            fx("TestUsesTaskEmbedded", &[("id", "api/$testTaskID/tail")]),
            fx("TestUsesNothing", &[("id", "not-a-uuid")]),
        ];
        assert_eq!(
            groups_for(&fixtures),
            vec![
                "TestUsesIssue".to_string(),
                "TestUsesIssueAgain".to_string(),
                "TestUsesTaskEmbedded".to_string(),
                "TestUsesWorkspace".to_string(),
            ]
        );
        // 分组键是 `source.test` 而不是 `id`：同一条上游测试的多次请求（CRUD 链）必须
        // 落到**同一组**，否则 delete-then-get 会变成 get-200。
        assert_eq!(group_of(&fixtures[0]), "TestUsesIssue");
    }

    #[test]
    fn symbol_registry_matches_the_extractors_collection_table() {
        // 承重：`SYMBOLS` 与抽取器 `SEEDED_COLLECTIONS` 是两份手抄的清单。
        // 手抄清单会漂，而漂的方向是「一边多一个符号」⇒ 那一类 fixture 静默变
        // unevaluable。这里把抽取器那份**逐字抄过来**当断言：它变了，这条就红。
        const EXTRACTOR_COLLECTIONS: [(&str, &str); 6] = [
            ("agents", "Agent"),
            ("issues", "Issue"),
            ("workspaces", "Workspace"),
            ("sessions", "ChatSession"),
            ("chat-sessions", "ChatSession"),
            ("tasks", "Task"),
        ];
        let derived: BTreeSet<String> = EXTRACTOR_COLLECTIONS
            .iter()
            .map(|(_, kind)| format!("$test{kind}ID"))
            .collect();
        let declared: BTreeSet<String> = Seed::SYMBOLS.iter().map(|s| (*s).to_string()).collect();
        // `Workspace` 不在 `SYMBOLS` 里，因为本文件不给它种「一行」—— 它每个分组一个，
        // 由 [`GROUP_SYMBOLS`] 与 [`seed_workspace`] 供，所以这里允许它少一个。
        let extra: Vec<&String> = derived.difference(&declared).collect();
        assert_eq!(
            extra,
            vec!["$testWorkspaceID"],
            "抽取器会发这些符号，但 seeder 没有对应字段"
        );
        // 反方向也要钉：`GROUP_SYMBOLS` 必须**恰好**是抽取器那一份加上 workspace。
        let group_symbols: BTreeSet<String> =
            GROUP_SYMBOLS.iter().map(|s| (*s).to_string()).collect();
        assert!(
            group_symbols.is_superset(&declared),
            "$testWorkspaceID 之外还有缺口"
        );
        assert_eq!(group_symbols.len(), declared.len() + 1);
    }
}
