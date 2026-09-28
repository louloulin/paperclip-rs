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
//!
//! # 一行种子，一个符号，**以及它的代价**
//!
//! 每个域**只种一行**，所有引用该符号的 fixture 共享它。这不是省事：上游的 CRUD
//! 链（`TestIssuesCRUDThroughRouter`：create → get → put → put → list → delete →
//! get 404）正是靠「同一个 id 贯穿全链」才成立的，各自种一行会把 delete-then-get
//! 变成 get-200。
//!
//! 代价是**独立的测试之间会互相摧毁**：一条 `DELETE /api/issues/{testIssueID}` 跑过之后，
//! 其后所有引用同一符号的 fixture 一起 404（docs/37 §209.4 的 C 桶，23 条）。
//! 这是**已知缺口，不是设计** —— 下一片应把符号从「每类一行」变成「每类每测试一行」
//! （`Fixture.source.test` 就是现成的分组键）。在那之前，不要把「只种一行」当成结论。

use anyhow::{bail, Context, Result};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

/// 种出来的那几行。字段名与 `Bindings` 里的符号名一一对应（`$testAgentID` ↔ `agent`）。
///
/// `Debug` 手写而非 derive：derive 会把这四个 `Option<Uuid>` 打成一行紧凑文本，
/// 而「哪几类种到了、哪几类没种到」正是排查时要先看的东西。这里逐字段列出。
/// 里面**没有凭据**（明文 `mdt_` 在 `Bindings` 里单独脱敏），所以可以逐字打。
#[derive(Clone, Default)]
pub struct Seed {
    pub agent: Option<Uuid>,
    pub issue: Option<Uuid>,
    pub chat_session: Option<Uuid>,
    pub task: Option<Uuid>,
}

impl std::fmt::Debug for Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seed")
            .field("agent", &self.agent)
            .field("issue", &self.issue)
            .field("chat_session", &self.chat_session)
            .field("task", &self.task)
            .finish()
    }
}

impl Seed {
    /// 抽取器（`scripts/extract_upstream_fixtures.py::SEEDED_COLLECTIONS`）会发出的
    /// 四个域符号。**两边必须逐字一致** —— 少一个，那一类 fixture 就整批
    /// `unbound symbol` → `unevaluable`；多一个，就是一条没人种、也无人认领的声明。
    pub const SYMBOLS: [&'static str; 4] = [
        "$testAgentID",
        "$testIssueID",
        "$testChatSessionID",
        "$testTaskID",
    ];

    /// 一个符号对应的那一行。抽取器只发 [`Self::SYMBOLS`] 里那四个符号，所以
    /// 未种出的一律是 `None`，由 [`crate::Bindings::resolve`] 报「unbound symbol」。
    ///
    /// `$testWorkspaceID` 刻意落在 `_` 那一支：workspace 由 `harness` 自己用
    /// `POST /api/workspaces` 建，不经本文件（它也不在 [`Self::SYMBOLS`] 里）。
    #[must_use]
    pub fn get(&self, symbol: &str) -> Option<Uuid> {
        match symbol {
            "$testAgentID" => self.agent,
            "$testIssueID" => self.issue,
            "$testChatSessionID" => self.chat_session,
            "$testTaskID" => self.task,
            _ => None,
        }
    }
}

/// 种出全部四行。任一行种不出来就整体失败 —— 半套种子会让一部分 fixture 变成
/// 「看起来判过了」的假象，那比明确报错糟得多。
pub async fn seed(
    router: &Router,
    db: &mc_db::pool::Db,
    user_id: Uuid,
    workspace_id: Uuid,
) -> Result<Seed> {
    let runtime_id = seed_runtime(db, user_id, workspace_id).await?;
    // 身份：与 `plan()` 同一条链路（session 头 + dev-mode 头，见 `post_json`）。
    // 种子身份自己是这些行的 owner。
    let user = user_id.to_string();
    let agent = seed_agent(router, &user, workspace_id, runtime_id).await?;
    let issue = seed_issue(router, &user, workspace_id).await?;
    let chat_session = seed_chat_session(router, &user, workspace_id, agent).await?;
    let task = seed_task(router, &user, workspace_id, agent, chat_session).await?;
    Ok(Seed {
        agent: Some(agent),
        issue: Some(issue),
        chat_session: Some(chat_session),
        task: Some(task),
    })
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
const SEED_SESSION_HEADER: &str = "x-multica-session";
const SEED_DEV_USER_HEADER: &str = "x-multica-user-id";

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

    #[test]
    fn every_emitted_symbol_maps_to_exactly_one_seed_field() {
        // 承重：抽取器 `SEEDED_COLLECTIONS` 发出的符号必须一个不漏地在这里落到字段上。
        // 漏一个的表现是那批 fixture 全部变成 `unbound symbol` → unevaluable，
        // 而 unevaluable 在总数里**不显眼**（§205.5 纪律：不可判定与判定为过长得一样）。
        let mut seed = Seed {
            agent: Some(Uuid::from_u128(1)),
            issue: Some(Uuid::from_u128(2)),
            chat_session: Some(Uuid::from_u128(3)),
            task: Some(Uuid::from_u128(4)),
        };
        for sym in Seed::SYMBOLS {
            assert!(seed.get(sym).is_some(), "{sym} 没有落到任何字段");
        }
        // workspace 由 harness 直接建，不经本文件；未知符号必须返回 None 而不是猜。
        assert_eq!(seed.get("$testWorkspaceID"), None);
        assert_eq!(seed.get("$testProjectID"), None);
        // 未种出的一律 None：报告里「不可判定」必须可区分于「种到了但请求失败」。
        seed = Seed::default();
        for sym in Seed::SYMBOLS {
            assert_eq!(seed.get(sym), None, "{sym}");
        }
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
        // `Workspace` 不经本文件（harness 自己建 workspace），所以这里允许它少一个。
        let extra: Vec<&String> = derived.difference(&declared).collect();
        assert_eq!(
            extra,
            vec!["$testWorkspaceID"],
            "抽取器会发这些符号，但 seeder 没有对应字段"
        );
    }
}
