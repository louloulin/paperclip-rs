//! chat 面真库测试脚手架（M4-4 / LUM-1601，补 `docs/45` §3 G9 的缺口）。
//!
//! 覆盖对象是 chat 面**手写 SQL 最多**的那批模块：`chat_task/{send,queue,onboarding}`、
//! `chat_history`、`chat_quick_action`，外加 `chat_session` / `chat_message` /
//! `chat_pinned_agent` / `chat_draft_restore`。它们的语义（`FOR UPDATE` 锁顺序、CAS、可见头不变式、
//! 1µs 时间戳推导、附件绑定条件、`ON CONFLICT` 幂等）在内存里看着也对，只有真 PostgreSQL 能给
//! 结论 —— 这正是本片存在的理由。
//!
//! 运行（`scripts/gates.sh --with-db` 的第 ⑥ 道门跑的就是这条命令）：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1601:…@127.0.0.1:5432/multica_lum1601 \
//!   cargo test -p mc-repos --lib -- --ignored
//! ```
//!
//! 没有 `MULTICA_TEST_DATABASE_URL` ⇒ 静默跳过（不接库时 `cargo test` 必须绿）；**设了却连不上
//! ⇒ panic**（库坏了必须红：静默跳过会让空跑伪装成绿，与 `crate::autopilot::tests` 同款）。
//!
//! 为什么只铺一份现场：各模块共用同一个「workspace + user + runtime + agent + 会话」最小
//! 现场，各铺一份等于把同一批 INSERT 抄 N 遍。断言分布在 `send.rs` / `queue.rs` /
//! `onboarding.rs` / `session.rs` / `message.rs` / `pin.rs` / `draft.rs` 七个子模块，以及
//! `chat_history.rs` / `chat_quick_action.rs` 的内联 `mod tests` 里（后者直接
//! `use crate::chat_task::tests::…`；同为 `#[cfg(test)]` ⇒ 不产生非测试构建的依赖边）。
//!
//! `chat_session` / `chat_message` / `chat_pinned_agent` / `chat_draft_restore` 的用例落在
//! 这里而不是那四个文件内联，是因为**真库现场只有一份**，且 `chat_session.rs`（797 行）已经贴着
//! 门 ⑩ 的单文件 800 行上限 —— 内联测试会把那个热点文件顶穿。
//!
//! ⚠️ 本片**不**做迁移、不建表：第 ⑥ 道门前置的 `mc-migrate run` 负责 schema。

use std::env;

use chrono::{DateTime, Utc};
use mc_db::Db;
use uuid::Uuid;

mod draft;
mod message;
mod onboarding;
mod pin;
mod queue;
mod send;
mod session;

use crate::chat_task::{ChatTaskRepo, PRIORITY_CHAT};

/// 最小现场：一个 workspace + 一个 user + 一台 runtime + 一个 agent + 一个会话。
///
/// 每个用例 `setup()` 一次、`teardown()` 一次；id 全部随机 ⇒ 与同库并发跑的其它用例
/// （`cargo test -- --ignored` 会在同一个 binary 里并发）互不干扰。
pub(crate) struct Fixture {
    pub(crate) db: Db,
    pub(crate) workspace_id: Uuid,
    pub(crate) user_id: Uuid,
    pub(crate) runtime_id: Uuid,
    pub(crate) agent_id: Uuid,
    pub(crate) session_id: Uuid,
}

impl Fixture {
    pub(crate) fn repo(&self) -> ChatTaskRepo {
        ChatTaskRepo::new(self.db.clone())
    }

    pub(crate) fn pool(&self) -> &sqlx::PgPool {
        self.db.pool()
    }
}

/// 铺现场。返回 `None` 只在「**没有配**库」时发生。
///
/// 没设 `MULTICA_TEST_DATABASE_URL` ⇒ 打印跳过信息并返回 `None`（本仓所有 `#[ignore]` 真库套件的统一约定：
/// 门 ⑤ 是不带库跑 `cargo test` 的，不能让整套测试变红）；**一旦设了就绝不静默跳过** ——
/// 哪怕只是一个空串，也照走连接并在此 `expect` 失败（红），不给「设了却没连上」留退路。
pub(crate) async fn setup() -> Option<Fixture> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1)
        .await
        .expect("MULTICA_TEST_DATABASE_URL is set but connect failed");
    let pool = db.pool();

    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-lum1601-chat', $1) RETURNING id",
    )
    .bind(format!("itest-lum1601-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    // `timezone` / `onboarding_questionnaire` 是 `user_onboarding_profile` 的两列，给非默认值
    // 才能证明它读的是行而不是常量。
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email, timezone, onboarding_questionnaire)
           VALUES ('itest-lum1601', $1, 'Asia/Shanghai', $2::jsonb) RETURNING id"#,
    )
    .bind(format!("itest-lum1601-{}@example.com", Uuid::new_v4()))
    .bind(r#"{"role":"architect"}"#)
    .fetch_one(pool)
    .await
    .expect("insert user");
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(workspace_id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert member");

    // `status = 'online'`：发消息不看 runtime 在线与否（`AgentReadiness` 是本片的 known_gap），
    // 但 `send` 的归属栅栏要求 runtime 行存在。
    let runtime_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, owner_id) \
         VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-lum1601-rt-{}", Uuid::new_v4()))
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime");

    let agent_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
         VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-lum1601-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert agent");

    // 会话 `runtime_id` 与 agent 一致（真实形态）；「锁内重读」的用例会把它改成陈旧值。
    let session_id: Uuid = sqlx::query_scalar(
        "INSERT INTO chat_session(workspace_id, agent_id, creator_id, title, runtime_id, \
                                  explicitly_created_at) \
         VALUES ($1, $2, $3, '', $4, now()) RETURNING id",
    )
    .bind(workspace_id)
    .bind(agent_id)
    .bind(user_id)
    .bind(runtime_id)
    .fetch_one(pool)
    .await
    .expect("insert chat_session");

    Some(Fixture {
        db,
        workspace_id,
        user_id,
        runtime_id,
        agent_id,
        session_id,
    })
}

/// 拆现场：删 workspace（`chat_session` / `chat_message` / `attachment` / `agent` /
/// `agent_runtime` / `agent_task_queue` 都从它级联）再删 user。
pub(crate) async fn teardown(fixture: &Fixture) {
    let pool = fixture.pool();
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(fixture.user_id)
        .execute(pool)
        .await;
}

// ---------------------------------------------------------------------------
// 现场零件
// ---------------------------------------------------------------------------

/// 再插一个会话（用例要两个会话 / 归档会话时用）。
///
/// `title` 恒为空串：标题 CAS 的用例必须从空开始；要非空标题的用例自己 `UPDATE`。
pub(crate) async fn new_session(fixture: &Fixture, agent_id: Uuid, status: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO chat_session(workspace_id, agent_id, creator_id, title, status, \
                                  runtime_id, explicitly_created_at) \
         VALUES ($1, $2, $3, '', $4, $5, now()) RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(agent_id)
    .bind(fixture.user_id)
    .bind(status)
    .bind(fixture.runtime_id)
    .fetch_one(fixture.pool())
    .await
    .expect("insert chat_session")
}

/// 再插一个 agent（`chat_pinned_agent` 的用例需要两个对端才能验 `position` 排序）。
pub(crate) async fn new_agent(fixture: &Fixture) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
         VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(format!("itest-lum1601-agent2-{}", Uuid::new_v4()))
    .bind(fixture.runtime_id)
    .bind(fixture.user_id)
    .fetch_one(fixture.pool())
    .await
    .expect("insert agent")
}

/// 再插一个会话，**不盖**「显式创建」戳 —— `explicitly_created_at IS NOT NULL` 是
/// `is_public_in_workspace` 与两个列表查询的可见性判据，必须有「没盖戳」的现场。
pub(crate) async fn new_unmarked_session(fixture: &Fixture, agent_id: Uuid, status: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO chat_session(workspace_id, agent_id, creator_id, title, status, \
                                  runtime_id, explicitly_created_at) \
         VALUES ($1, $2, $3, '', $4, $5, NULL) RETURNING id",
    )
    .bind(fixture.workspace_id)
    .bind(agent_id)
    .bind(fixture.user_id)
    .bind(status)
    .bind(fixture.runtime_id)
    .fetch_one(fixture.pool())
    .await
    .expect("insert chat_session")
}

/// 插一个 project（`create_explicit` / `update_project_locked` 的 `FOR KEY SHARE` 目标）。
pub(crate) async fn new_project(fixture: &Fixture, title: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO project(workspace_id, title) VALUES ($1, $2) RETURNING id")
        .bind(fixture.workspace_id)
        .bind(title)
        .fetch_one(fixture.pool())
        .await
        .expect("insert project")
}

/// 插一条 `chat_draft_restore`（**没有** `chat_session` 外键 ⇒ 删会话不会级联带走它）。
pub(crate) async fn new_draft_restore(
    fixture: &Fixture,
    session_id: Uuid,
    task_id: Uuid,
    content: &str,
    created_at: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO chat_draft_restore (id, chat_session_id, task_id, content, \
                                         attachment_ids, created_at) \
         VALUES ($1, $2, $3, $4, $5, COALESCE($6::timestamptz, now())) RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(task_id)
    .bind(content)
    .bind(Vec::<Uuid>::new())
    .bind(created_at)
    .fetch_one(fixture.pool())
    .await
    .expect("insert chat_draft_restore")
}

/// `agent_task_queue` 的一行（只列本片断言的列）。
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct RawTask {
    pub(crate) status: String,
    pub(crate) priority: i32,
    pub(crate) runtime_id: Option<Uuid>,
    pub(crate) chat_input_task_id: Option<Uuid>,
    /// `revision: Some(_)` ⇒ 渠道上下文代际（`chat_history` 的分支开关）。
    pub(crate) channel_context_revision: Option<i64>,
    /// `Some(_)` ⇒ 背景 quick-actions 重生成轮（对 pending 面不可见）。
    pub(crate) regenerate_quick_actions_for: Option<Uuid>,
    /// 归属三元组（send 事务的第 5 步写死 `direct_human` / `chat` / 会话 id）。
    pub(crate) originator_source: Option<String>,
    pub(crate) trigger_evidence_kind: Option<String>,
    pub(crate) trigger_evidence_ref_id: Option<Uuid>,
    /// `buildRuntimeMCPOverlay` 不可达 ⇒ 恒 NULL（`docs/45` `known_gap` 第 1 条）。
    pub(crate) runtime_mcp_overlay: Option<serde_json::Value>,
    pub(crate) fire_at: Option<DateTime<Utc>>,
    pub(crate) completed_at: Option<DateTime<Utc>>,
    /// 取消路径要清掉它（上游 `CancelQueuedAgentTasksForSession` 的 `prepare_lease_expires_at
    /// = NULL`）。
    pub(crate) prepare_lease_expires_at: Option<DateTime<Utc>>,
}

/// 读一行 `agent_task_queue`（断言语义用的原始投影，不走仓储 —— 仓储的读面本身就可能是被测对象）。
pub(crate) async fn raw_task(fixture: &Fixture, task_id: Uuid) -> RawTask {
    sqlx::query_as::<_, RawTask>(
        "SELECT status, priority, runtime_id, chat_input_task_id, channel_context_revision, \
                regenerate_quick_actions_for, originator_source, trigger_evidence_kind, \
                trigger_evidence_ref_id, runtime_mcp_overlay, fire_at, completed_at, \
                prepare_lease_expires_at \
         FROM agent_task_queue WHERE id = $1",
    )
    .bind(task_id)
    .fetch_one(fixture.pool())
    .await
    .expect("load agent_task_queue row")
}

/// 一行 `agent_task_queue` 的插法（只列用例需要区分的那几维）。
#[derive(Debug, Clone)]
pub(crate) struct TaskSeed {
    pub(crate) status: &'static str,
    pub(crate) priority: i32,
    /// `Some(_)` ⇒ 显式 `created_at`（默认 `now()`）；FIFO 顺序要钉死时用。
    pub(crate) created_at: Option<&'static str>,
    /// `true` ⇒ `chat_input_task_id = 自己`（send 事务第 6 步的结果形态）。
    pub(crate) owns_input: bool,
    /// `Some(_)` ⇒ `chat_input_task_id = 那个任务`（auto-retry 克隆继承父任务的输入批次）。
    pub(crate) input_of: Option<Uuid>,
    /// `Some(_)` ⇒ 背景重生成轮（`pending` / `clear` / `prioritize` 都把它排除在外）。
    pub(crate) regenerate_for: Option<Uuid>,
    /// 渠道上下文代际。
    pub(crate) revision: Option<i64>,
}

impl TaskSeed {
    /// 直聊排队轮：`queued` + `PRIORITY_CHAT` + 自己认领输入批次。
    pub(crate) fn queued() -> Self {
        Self {
            status: "queued",
            priority: PRIORITY_CHAT,
            created_at: None,
            owns_input: true,
            input_of: None,
            regenerate_for: None,
            revision: None,
        }
    }

    pub(crate) fn status(mut self, status: &'static str) -> Self {
        self.status = status;
        self
    }

    pub(crate) fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub(crate) fn at(mut self, created_at: &'static str) -> Self {
        self.created_at = Some(created_at);
        self
    }

    pub(crate) fn no_input(mut self) -> Self {
        self.owns_input = false;
        self
    }

    pub(crate) fn input_owned_by(mut self, owner: Uuid) -> Self {
        self.owns_input = false;
        self.input_of = Some(owner);
        self
    }

    pub(crate) fn regenerating(mut self, message_id: Uuid) -> Self {
        self.regenerate_for = Some(message_id);
        self
    }

    pub(crate) fn revision(mut self, revision: i64) -> Self {
        self.revision = Some(revision);
        self
    }
}

/// 插一条 chat 任务。
///
/// `originator_source` / `trigger_evidence_kind` 照 send 事务写死（`direct_human` / `chat`），
/// 让 `prioritize` / `clear` 的用例与真链路同形；`issue_id` 恒 NULL（chat 面没有 issue）。
/// `completed_at` 在终态上必须非空 —— `agent_task_queue_active_requires_runtime` 这条
/// `NOT VALID` CHECK 仍然管 INSERT。
pub(crate) async fn insert_task(fixture: &Fixture, session_id: Uuid, seed: TaskSeed) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agent_task_queue ( \
             id, agent_id, runtime_id, issue_id, status, priority, chat_session_id, \
             initiator_user_id, originator_user_id, accountable_user_id, force_fresh_session, \
             originator_source, trigger_evidence_kind, trigger_evidence_ref_id, \
             chat_input_task_id, regenerate_quick_actions_for, channel_context_revision, \
             completed_at, created_at) \
         VALUES ($1, $2, $3, NULL, $4, $5, $6, $7, $7, $7, FALSE, 'direct_human', 'chat', $6, \
                 CASE WHEN $8 THEN $1 ELSE $12 END, $9, $10, \
                 CASE WHEN $4 IN ('completed', 'failed', 'cancelled') THEN now() ELSE NULL END, \
                 COALESCE($11::timestamptz, now()))",
    )
    .bind(id)
    .bind(fixture.agent_id)
    .bind(fixture.runtime_id)
    .bind(seed.status)
    .bind(seed.priority)
    .bind(session_id)
    .bind(fixture.user_id)
    .bind(seed.owns_input)
    .bind(seed.regenerate_for)
    .bind(seed.revision)
    .bind(seed.created_at)
    .bind(seed.input_of)
    .execute(fixture.pool())
    .await
    .expect("insert agent_task_queue");
    id
}

/// 一行 `chat_message` 的插法。
#[derive(Debug, Clone)]
pub(crate) struct MessageSeed<'a> {
    pub(crate) role: &'a str,
    pub(crate) content: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) task_id: Option<Uuid>,
    pub(crate) created_at: Option<&'a str>,
    pub(crate) channel_ingested: bool,
    pub(crate) revision: Option<i64>,
}

impl<'a> MessageSeed<'a> {
    pub(crate) fn user(content: &'a str) -> Self {
        Self {
            role: "user",
            content,
            kind: "message",
            task_id: None,
            created_at: None,
            channel_ingested: false,
            revision: None,
        }
    }

    pub(crate) fn assistant(content: &'a str) -> Self {
        Self {
            role: "assistant",
            ..Self::user(content)
        }
    }

    pub(crate) fn kind(mut self, kind: &'a str) -> Self {
        self.kind = kind;
        self
    }

    pub(crate) fn on(mut self, task_id: Uuid) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub(crate) fn at(mut self, created_at: &'a str) -> Self {
        self.created_at = Some(created_at);
        self
    }

    pub(crate) fn ingested(mut self) -> Self {
        self.channel_ingested = true;
        self
    }

    pub(crate) fn revision(mut self, revision: i64) -> Self {
        self.revision = Some(revision);
        self
    }
}

/// 插一条 `chat_message`。
pub(crate) async fn insert_message(
    fixture: &Fixture,
    session_id: Uuid,
    seed: MessageSeed<'_>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO chat_message (id, chat_session_id, role, content, task_id, message_kind, \
                                   channel_ingested, channel_context_revision, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, COALESCE($9::timestamptz, now())) \
         RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(seed.role)
    .bind(seed.content)
    .bind(seed.task_id)
    .bind(seed.kind)
    .bind(seed.channel_ingested)
    .bind(seed.revision)
    .bind(seed.created_at)
    .fetch_one(fixture.pool())
    .await
    .expect("insert chat_message")
}

/// 从库里读回一条消息（`RETURNING` 之外的旁证：`settle` / `adopt` 都改的是别的行）。
pub(crate) async fn message_row(
    fixture: &Fixture,
    message_id: Uuid,
) -> crate::chat_message::ChatMessageRow {
    sqlx::query_as::<_, crate::chat_message::ChatMessageRow>(&format!(
        "SELECT {} FROM chat_message WHERE id = $1",
        crate::chat_message::MESSAGE_COLUMNS
    ))
    .bind(message_id)
    .fetch_one(fixture.pool())
    .await
    .expect("load chat_message row")
}

/// 会话的一行（只列本片断言的列）。
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct RawSession {
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) runtime_id: Option<Uuid>,
    pub(crate) updated_at: DateTime<Utc>,
}

pub(crate) async fn raw_session(fixture: &Fixture, session_id: Uuid) -> RawSession {
    sqlx::query_as::<_, RawSession>(
        "SELECT title, status, runtime_id, updated_at FROM chat_session WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(fixture.pool())
    .await
    .expect("load chat_session row")
}

/// `chattitle.Derive` 的替身：仓储只把它当 `fn(&str) -> String` 用（真值在 `mc-chat`，
/// 本仓储不引那条依赖边）。**截断规则必须与产品同源**（`mc_chat::task::TITLE_LIMIT = 30`，
/// 超出取 29 个 rune 再补 `…`），否则用例断言的标题不是会话列表里会看到的那一个。
/// 这里只做「第一行非空行 → 折叠空白 → 截断」三步，markdown 装饰（fence / 链接）不做。
pub(crate) fn stub_derive_title(content: &str) -> String {
    const TITLE_LIMIT: usize = 30;
    let line = content
        .split('\n')
        .find(|candidate| !candidate.trim().is_empty())
        .unwrap_or("");
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let runes: Vec<char> = line.chars().collect();
    if runes.len() <= TITLE_LIMIT {
        return line;
    }
    let head: String = runes[..TITLE_LIMIT - 1].iter().collect();
    format!("{}…", head.trim_end())
}
