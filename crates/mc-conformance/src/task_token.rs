//! **任务令牌装置**（`actor.kind = agent` 那一档身份的凭据面，`§297`）。
//!
//! # 这一族红在哪
//!
//! 门 ⑨ 实测：`PRECONDITION` 25 条里最大的一块是 **11 条 `agent` actor**，
//! 报告里逐字是同一句 `actor kind Agent needs a real credential; this runner does not
//! fabricate one` —— 它们**根本没有进判定**（`outcome == unevaluable`）。
//! 分族判据写在 `scripts/t1_6_taxonomy.py`：第一刀是 `outcome` 而不是 `requires`，
//! 所以这 11 条属于装置面，**不属于任何 handler**。
//!
//! # 上游那一档身份的凭据是什么
//!
//! 这 11 条里有 10 条打 `/api/chat/history` 与 `/api/chat/thread`。上游把它们交给
//! agent 侧 CLI，凭据是**任务作用域令牌**：`X-Actor-Source: task_token` + `X-Task-ID`
//! （上游 `chat_history_test.go:104-110` 的 `taskActorReq`，注释逐字写着
//! "builds a request as the Auth middleware would leave it for a mat_ task token"）。
//! 本仓把同一道门逐字复刻在 `routes/chat/task/history.rs::chat_history_scope`：
//! 403（不是任务令牌）→ 400（缺 / 坏 task id）→ 404（任务不存在）→ 400（不是 chat 任务）
//! → 404（会话不存在）→ 403（workspace 不匹配）→ 404（代际不存在）。
//!
//! 🔴 **所以这一档在本仓是有解析面的**：它不是「一个只能伪造的 header」，而是
//! 「一个必须指向库里真有一行 `agent_task_queue` 的 id」。缺的是那一行 ——
//! 而 fixture 里那个 id 是**抽取器借来的字面量**（`seed.rs` 模块头记的同一个病：
//! `package_literals` 是全仓扫描 + `setdefault`，于是别处测试的 UUID 落进了这条 header）。
//!
//! # 本模块干的两件事
//!
//! 1. **声明**（[`TASK_TOKEN_TASKS`]）：哪个分组需要哪一行任务、主键逐字是哪个 UUID、
//!    它当时**是不是**一个 chat 任务。声明挂在**上游 provenance** 上并带证据，
//!    判据是「上游那条测试当时处于什么世界」，**一个字节都不读 `expect`** ——
//!    与 [`crate::device_shape`] 同款纪律。
//! 2. **兑现**（[`seed_for_group`]）：用 `TaskRepo::create_task` 建那一行（主键由装置
//!    铸造，因为 12 个分组要 12 行而字面量只有一个），并在回放时把 fixture 里那枚
//!    字面量绑到它 —— 见 [`crate::Bindings::task_token_task_for`]。
//!
//! # 为什么是仓储调用而不是真实路由（唯一一处例外，且理由写在这里）
//!
//! 本仓唯一会往 `agent_task_queue` 插行的**路由**是 chat 发送面
//! （`mc-repos/src/chat_task/send.rs`），而它**恒**写 `chat_session_id`
//! ⇒ 建不出「不是 chat 任务」那一行（`chat_history_test.go:642` 那条要的正是它）。
//! 唯一给得起「`chat_session_id` 可空」的是 `mc_repos::task::NewTask`
//! （`create_task` 是 `mc-http` 里零调用点的仓储面，与 `seed_runtime` 同一类例外）。
//! 与 [`crate::seed::seed_runtime`] 同一句话：**缺的不是仓储能力，是把这一面暴露成
//! 一条 HTTP 路由**；而 ⑦ `known_gap = 0` ⇒ 本仓不许新增注册路由。
//!
//! # 本模块**给不出**的东西（写在这里是为了不让下一个读代码的人重新推一遍）
//!
//! * 🔴 **agent 身份的校验面**：上游 `resolveActor` 会校验 (agent, task) 这一对之后才
//!   信任 `X-Task-ID`；本仓**没有**这一面（`/api/issues` 只认 session 成员身份）。
//!   所以 `issues/TestCreateIssue_AgentCreate_StampsActingTaskOrigin` 被放行之后，
//!   它断言的仍然只是状态码 —— **它测不出「少了一道 agent 身份校验」**。
//!   这不是本模块能补的（补它要动 `mc-http`，本片写集之外），登记在此。
//! * `TestGetChatHistory_NonChatTask` 之外的形态（代际 / 渠道绑定）本仓没有渠道阅读器
//!   （M7），因此那些 fixture 只能断言到状态码 200 这一层。

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use mc_core::Id;
use mc_repos::task::{NewTask, TaskRepo};

/// 那一行任务当时**是不是**一个 chat 任务（`agent_task_queue.chat_session_id`）。
///
/// 两条形态来自同一个上游事实：上游 `newChatHistoryTask(t, chatSession bool)`
/// （`chat_history_test.go:63-84`）用同一个 helper 建两种任务，
/// `chatSession == false` 时 `chat_session_id` 为 `NULL`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// 绑着一个 chat 会话 ⇒ history / thread 读得到转录（上游 10 条 200）。
    Chat,
    /// `chat_session_id IS NULL` ⇒ 上游那句
    /// `"this task is not a chat task"` ⇒ 400（`chat_history_test.go:642`）。
    NotAChatTask,
}
/// 一条任务令牌装置声明：挂在**上游 provenance** 上，带上游证据。
#[derive(Debug, Clone, Copy)]
pub struct TaskTokenTask {
    /// 上游测试名（= 分组键 `Fixture.source.test`）。
    pub test: &'static str,
    /// fixture 的 `actor.upstream_identity["X-Task-ID"]` 里**逐字**出现的那个值
    /// （借来的行 id，**不是**行主键 —— 见 [`TASK_TOKEN_TASKS`]）。
    pub task_id: &'static str,
    /// 这一行当时是不是 chat 任务。
    pub kind: TaskKind,
    /// 上游证据：`文件:行` + 那条测试建了什么。
    pub evidence: &'static str,
}

/// 全部任务令牌声明（`§297` 实测 11 条 `agent` fixture 所属的 12 个分组）。
///
/// 那 11 条 fixture 的 `X-Task-ID` **逐字相同**（`5c57b65b-…`）—— 抽取器的
/// `package_literals` 把同一个 UUID 借给了全部分组（`seed.rs` 模块头记的同一个病）。
///
/// 🔴 正因为它逐字相同，**种出来的那一行不能拿这个字面量当主键**：12 个分组要 12 行，
/// 而主键全局唯一。种子的粒度必须回到「每分组一行」（否则第二条就撞主键），
/// 于是装置在回放时把那枚字面量**绑到该分组自己那一行**
/// （`crate::Bindings::task_token_task_for`）。这与抽取器侧
/// `extract_borrowed_ids.seeded_symbol_for` 把借来的行 id 判成 `$test<Kind>ID`
/// 是同一件事的两端：**字面量不是行 id，装置得把它绑到它真的种出来的那一行**。
pub const TASK_TOKEN_TASKS: &[TaskTokenTask] = &[
    TaskTokenTask {
        test: "TestGetChatChannelHistory_Success",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:165 `newChatHistoryTask(t, true)` 建的是**带会话**的 \
                   任务，`taskActorReq` 带上 actor 头后断言 200",
    },
    TaskTokenTask {
        test: "TestGetChatThread_CurrentThread",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:163 `newChatHistoryTask(t, true)` + `GetChatThread` ⇒ 200",
    },
    TaskTokenTask {
        test: "TestGetChatThread_ByID",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:186 同上，多带一个 `?id=70.0`",
    },
    TaskTokenTask {
        test: "TestGetChatHistory_NoSlackBindingFallsBackToTranscript",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:213 同上（Slack 读替身返回 `ErrNoSlackSession` ⇒ \
                   回落转录）",
    },
    TaskTokenTask {
        test: "TestGetChatHistory_NoSlackBindingReadsStoredTranscript",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:246 `newChatHistoryTaskForSession` + 四条 \
                   `chat_message` ⇒ 200 且读得到三条可见消息",
    },
    TaskTokenTask {
        test: "TestGetChatHistory_NilReaderServesTranscript",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:289 `newChatHistoryTask(t, true)`，Slack 读替身是 \
                   `nil` ⇒ 仍回落转录",
    },
    TaskTokenTask {
        test: "TestGetChatHistory_NilReaderServesStoredTranscript",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:320 `newChatHistoryTaskForSession` + 两条 \
                   `chat_message`",
    },
    TaskTokenTask {
        test: "TestGetChatHistory_TranscriptNamesItsChannel",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:380 一个会话两处断言（读得到转录 + `channel_type` \
                   报出平台）",
    },
    TaskTokenTask {
        test: "TestGetChatHistory_ChannelTaskCannotReadEarlierContextGeneration",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:486 渠道任务 + `context_revision` 代际窗口 ⇒ 200。\
                   本仓没有渠道阅读器（M7），因此只能判定到状态码这一层",
    },
    TaskTokenTask {
        test: "TestGetChatHistory_RejectsForgedTaskID",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "chat_history_test.go:610 这条**不带** `X-Actor-Source` ⇒ 上游第一道 actor \
                   闸就拒（403）。本仓那道闸逐字相同，所以这一条**不依赖**种出来的任务行 —— \
                   种它只是为了让 `X-Task-ID` 有个可解析的取值",
    },
    TaskTokenTask {
        test: "TestGetChatHistory_NonChatTask",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::NotAChatTask,
        evidence: "chat_history_test.go:642 `newChatHistoryTask(t, false)` ⇒ \
                   `chat_session_id IS NULL` ⇒ 上游回 `\"this task is not a chat task\"` 400",
    },
    TaskTokenTask {
        test: "TestCreateIssue_AgentCreate_StampsActingTaskOrigin",
        task_id: TASK_ID_LITERAL,
        kind: TaskKind::Chat,
        evidence: "issue_agent_create_origin_test.go:36-46 插一行带 `originator_user_id` 的 \
                   `running` 任务，再带 `X-Agent-ID` + `X-Task-ID` 建 issue，断言 201。\
                   🔴 本仓没有 `resolveActor` 那一面，所以放行之后这条只断言到状态码",
    },
];

/// 那 11 条 fixture 里逐字出现的那个任务 id（**不是**行主键）。
///
/// 单独命名是为了让「同一个字面量出现在 11 条 fixture 上」这件事在代码里是**一次**断言
/// 而不是 11 条注释（见本模块的
/// `every_declared_task_id_is_the_literal_the_fixture_names`）。
const TASK_ID_LITERAL: &str = "5c57b65b-ee7a-4603-a72d-b659c34a1dc3";

/// 某个分组需要的全部任务令牌声明（0..n 条）。
pub fn tasks_for(test: &str) -> impl Iterator<Item = &'static TaskTokenTask> + '_ {
    TASK_TOKEN_TASKS.iter().filter(move |d| d.test == test)
}

/// 兑现某个分组的任务令牌声明，返回**该分组那一行**的主键（没声明 = `None`）。
///
/// 主键由**本次装置铸造**（不是那个字面量）—— 理由见 [`TASK_TOKEN_TASKS`] 的
/// 承重段：12 个分组各自要一行，而字面量只有一个。
///
/// `agent_id` / `runtime_id` 取该分组**自己那套**种子行（`seed.rs::seed_group` 的
/// `agent` 与 `seed_runtime` 的返回值）—— 跨分组共享会让一条 `DELETE` 摧毁别的分组的
/// 任务行，与 `seed.rs` 模块头那条纪律同源。
pub async fn seed_for_group(
    db: &mc_db::pool::Db,
    test: &str,
    agent_id: Uuid,
    runtime_id: Uuid,
    chat_session: Uuid,
) -> Result<Option<Uuid>> {
    let repo = TaskRepo::new(db);
    let mut seeded: Option<Uuid> = None;
    for decl in tasks_for(test) {
        // 一个分组最多一条声明：`two rows for one acting task` 不是本仓能表达的世界状态，
        // 而 fixture 上只有一枚 `X-Task-ID` —— 两条声明里后一条会静默覆盖前一条。
        if seeded.is_some() {
            bail!(
                "group {test:?} declares more than one acting task; a fixture names exactly \
                 one X-Task-ID, so a second declaration would silently shadow the first"
            );
        }
        let session = decl.kind.session(chat_session);
        let minted = Uuid::new_v4();
        let new = NewTask {
            id: Id::from(minted),
            agent_id: Id::from(agent_id),
            issue_id: None,
            runtime_id: Some(Id::from(runtime_id)),
            priority: 0,
            context: None,
            trigger_comment_id: None,
            chat_session_id: session.map(Id::from),
            autopilot_run_id: None,
            parent_task_id: None,
            retry_of_task_id: None,
            rerun_of_task_id: None,
            delegated_from_task_id: None,
            escalation_for_task_id: None,
            trigger_summary: None,
            handoff_note: None,
            is_leader_task: false,
            force_fresh_session: false,
            work_dir: None,
            budget: mc_task::retry::RetryBudget::FIRST_RUN,
            fire_at: None,
        };
        repo.create_task(&new)
            .await
            .with_context(|| format!("seed the acting task row for group {test:?}"))?;
        seeded = Some(minted);
    }
    Ok(seeded)
}

impl TaskKind {
    /// 这一形态下 `chat_session_id` 该是什么（`None` = 不是 chat 任务）。
    const fn session(self, chat_session: Uuid) -> Option<Uuid> {
        match self {
            Self::Chat => Some(chat_session),
            Self::NotAChatTask => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 声明的每一个主键都必须**真的是**某条 fixture 的 `X-Task-ID`。
    ///
    /// 反向也钉：语料里每一处 `agent` actor 的 `X-Task-ID` 字面量都必须被声明覆盖 ——
    /// 否则少声明一条就会静默变成「`unbound symbol` 式」的不可判定，而不可判定与判定为过
    /// 在 `totals` 里长得一样（`seed.rs` §205.5 同款纪律）。
    #[test]
    fn every_declared_task_id_is_the_literal_the_fixture_names() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/golden");
        let fixtures = crate::load_dir(&dir).expect("contracts/golden loads");
        let mut seen_tests: Vec<&str> = Vec::new();
        for decl in TASK_TOKEN_TASKS {
            assert!(
                !decl.evidence.trim().is_empty(),
                "{} 没有上游证据",
                decl.test
            );
            assert!(
                fixtures.iter().any(|fx| fx.source.test == decl.test
                    && fx
                        .actor
                        .upstream_identity
                        .get("X-Task-ID")
                        .is_some_and(|v| v == decl.task_id)),
                "{}: 语料里没有一条 fixture 的 X-Task-ID 逐字是 {:?}",
                decl.test,
                decl.task_id
            );
            assert!(!seen_tests.contains(&decl.test), "重复登记 {}", decl.test);
            seen_tests.push(decl.test);
        }
        // 反向：每条 `agent` fixture 的 `X-Task-ID` 都被某条声明点名。
        for fx in &fixtures {
            if fx.actor.kind != crate::ActorKind::Agent {
                continue;
            }
            let Some(task_id) = fx.actor.upstream_identity.get("X-Task-ID") else {
                continue;
            };
            assert!(
                seen_tests.contains(&fx.source.test.as_str()),
                "{}: agent fixture 引用了任务令牌，却没有对应的装置声明",
                fx.id
            );
            let _ = task_id;
        }
    }

    /// `NotAChatTask` 那一档必须真的不带会话 —— 它存在的**全部**意义就是
    /// `chat_session_id IS NULL`；写错成 `Chat` 会把上游那条 400 变成一条假 200。
    #[test]
    fn the_non_chat_shape_carries_no_session() {
        assert!(TaskKind::Chat.session(Uuid::nil()).is_some());
        assert!(TaskKind::NotAChatTask.session(Uuid::nil()).is_none());
        let non_chat: Vec<_> = TASK_TOKEN_TASKS
            .iter()
            .filter(|d| d.kind == TaskKind::NotAChatTask)
            .collect();
        assert_eq!(
            non_chat.len(),
            1,
            "「不是 chat 任务」只有上游那一条；多出来的形态没有证据"
        );
    }

    /// 声明的主键必须是合法 UUID —— 装载期就该报错，而不是回放中途变成一句
    /// 「种不出来」的错误信息（那种信息长得像 handler 坏了）。
    #[test]
    fn every_declared_task_id_parses() {
        for decl in TASK_TOKEN_TASKS {
            assert!(
                Uuid::parse_str(decl.task_id).is_ok(),
                "{}: {:?}",
                decl.test,
                decl.task_id
            );
        }
    }
}
