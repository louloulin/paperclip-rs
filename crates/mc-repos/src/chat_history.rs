//! M4-4（LUM-1475）：`chat_history` 仓储 —— `/api/chat/history` 与 `/api/chat/thread`
//! 的读取面（上游 `router.go` #24–#25，handler `chat_history.go` 418 行）。
//!
//! | 本模块方法 | 上游 query |
//! | --- | --- |
//! | [`ChatHistoryRepo::task_context`] | `GetAgentTask`（`agent_task_queue` 单行） |
//! | [`ChatHistoryRepo::session_workspace`] | `GetChatSession` 的最小投影 |
//! | [`ChatHistoryRepo::context_generation`] | `GetChannelChatContextGeneration`（`channel.sql:875`） |
//! | [`ChatHistoryRepo::channel_type_for_session`] | `GetChannelChatSessionBindingBySessionAny`（`channel.sql:702`） |
//! | [`ChatHistoryRepo::transcript_page`] | `ListChatMessagesPage`（`chat.sql:1085`）/ `…ForChannelContext`（`chat.sql:917`） |
//!
//! **两条分支都真做**：`channel_context_revision` 有效时走**上下文代际过滤**的分页
//! （`ListChatMessagesPageForChannelContext`），否则走 M4-3 的可见头分页
//! （[`crate::chat_message::ChatMessageRepo::list_page`]）。前者不是预留位 —— 渠道任务的
//! `channel_context_revision` 只要被填上（M7 的 Feishu/Slack ingest 会填），这条路径就会
//! 被真正走到；不实现它会让渠道任务读到整个 room 的历史（跨代际泄漏）。
//!
//! ⚠️ 范围硬边界（`docs/42` §4.3 第 3 条）：上游这两个端点在有渠道绑定时把读取交给
//! `h.SlackHistory`（`channel.HistoryReader` 的 slack/lark 实现）。**本波只落「无渠道
//! reader」的两条路径**：history = 已存转录（上面的分页），thread =
//! `writeNoChannelIntegration`（200 + note）。渠道 reader 随 M7 补齐，已在 `docs/45`
//! §`known_gap` 显式登记。⇒ 本文件**不引入渠道 API 客户端**，只读 `channel_*` 两张**已有**
//! 表（无新迁移）。
//!
//! 约定与 M1/M2/M3 各 Repo 一致（见 `crate::task` / `crate::chat_session`）：
//! - `Row` 用原始 `Uuid`/`String` 字段（`mc_core::Id` 没有 sqlx impl ⇒ 手写 `FromRow`）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - **真库测试**在本文件末尾的 `mod tests`（LUM-1601 补的 `docs/45` §3 G9 缺口），现场借自
//!   `crate::chat_task::tests`（同一个 workspace + user + agent + runtime 最小现场，不为读面
//!   再铺一份）；`#[ignore]` + `MULTICA_TEST_DATABASE_URL`，由 `scripts/gates.sh --with-db`
//!   的第 ⑥ 道门拉起。
//!
//! 本文件**只读**：不出现 INSERT / UPDATE / DELETE。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::chat_message::{ChatMessageRepo, ChatMessageRow};
use crate::workspace::map_sqlx_err;
use crate::Result;

/// `chatHistorySession` 认领任务所需的最小投影（上游 `GetAgentTask` 只被读这三列）。
#[derive(Debug, Clone, FromRow)]
pub struct ChatHistoryTaskRow {
    /// 任务 id。
    pub id: Uuid,
    /// 所属会话；`NULL` ⇒ 400 `"this task is not a chat task"`。
    pub chat_session_id: Option<Uuid>,
    /// 渠道上下文版本；有效时读到的历史必须先按代际过滤。
    pub channel_context_revision: Option<i64>,
}

/// `channel_chat_context_generation` 的读取投影（`history_*` 两列 + boundary 标记）。
#[derive(Debug, Clone, FromRow)]
pub struct ChatContextGenerationRow {
    /// 本代际可读的**最早** provider 消息 id（`after` 边界）。
    pub history_start_message_id: Option<String>,
    /// 本代际可读的**最晚** provider 消息 id（`until` 边界）。
    pub history_end_message_id: Option<String>,
    /// 边界尚未落定（渠道 reader 要据此放宽窗口）。
    pub history_boundary_pending: bool,
}

/// chat 历史 / 线索的只读仓储。
#[derive(Debug, Clone)]
pub struct ChatHistoryRepo {
    db: mc_db::Db,
}

impl ChatHistoryRepo {
    /// 用连接池构造。
    pub fn new(db: mc_db::Db) -> Self {
        Self { db }
    }

    /// 上游 `GetAgentTask`（`chatHistorySession` 只读 `chat_session_id` 与
    /// `channel_context_revision`）。`None` = 任务不存在。
    pub async fn task_context(&self, task_id: Uuid) -> Result<Option<ChatHistoryTaskRow>> {
        sqlx::query_as::<_, ChatHistoryTaskRow>(
            "SELECT id, chat_session_id, channel_context_revision FROM agent_task_queue \
             WHERE id = $1",
        )
        .bind(task_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 会话所属 workspace（`chatHistorySession` 的纵深防御：token 盖章的 workspace 必须
    /// 与会话一致）。`None` = 会话已不存在。
    pub async fn session_workspace(&self, session_id: Uuid) -> Result<Option<Uuid>> {
        sqlx::query_scalar("SELECT workspace_id FROM chat_session WHERE id = $1")
            .bind(session_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetChannelChatContextGeneration`（`channel.sql:875`，逐字单行读）。
    ///
    /// `None` ⇒ handler 404 `"chat context generation not found"`。
    pub async fn context_generation(
        &self,
        session_id: Uuid,
        revision: i64,
    ) -> Result<Option<ChatContextGenerationRow>> {
        sqlx::query_as::<_, ChatContextGenerationRow>(
            "SELECT history_start_message_id, history_end_message_id, \
                    history_boundary_pending \
             FROM channel_chat_context_generation \
             WHERE chat_session_id = $1 AND revision = $2",
        )
        .bind(session_id)
        .bind(revision)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `sessionChannelType`：**只有「没有这一行」才等于「没有渠道」**。
    ///
    /// 任何其它失败都是「读不出来」，handler 必须报错而不是猜 `""` —— 后者会把一个
    /// Lark/WeCom 会话在 200 响应里说成纯 web 会话。
    pub async fn channel_type_for_session(&self, session_id: Uuid) -> Result<Option<String>> {
        sqlx::query_scalar(
            "SELECT channel_type FROM channel_chat_session_binding WHERE chat_session_id = $1",
        )
        .bind(session_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// `chatMessageHistory` 的取页：代际有效时走渠道上下文过滤，否则走可见头分页。
    ///
    /// 返回**时间倒序**（新 → 旧）的一页；转成渠道契约的「旧 → 新」由
    /// `mc_chat::history::transcript_page` 负责。
    pub async fn transcript_page(
        &self,
        session_id: Uuid,
        context_revision: Option<i64>,
        fetch_limit: i64,
        before: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<Vec<ChatMessageRow>> {
        match context_revision {
            Some(revision) => {
                self.transcript_page_for_channel_context(session_id, revision, fetch_limit, before)
                    .await
            }
            None => {
                ChatMessageRepo::new(self.db.clone())
                    .list_page(session_id, fetch_limit, before)
                    .await
            }
        }
    }

    /// 上游 `ListChatMessagesPageForChannelContext`（`chat.sql:917`）。
    ///
    /// 三处与可见头分页的**实质差异**（照上游逐字）：
    /// 1. **没有** visible-head EXCEPT 子句 —— 渠道任务的输入批次天然可见；
    /// 2. assistant 行的代际**继承自它所属任务**（`LEFT JOIN agent_task_queue owner`），
    ///    所以重试与迟到完成都留在自己那一代；
    /// 3. `revision = 1` 时把 `NULL` 也算作同一代（回填前的老数据）。
    pub async fn transcript_page_for_channel_context(
        &self,
        session_id: Uuid,
        revision: i64,
        fetch_limit: i64,
        before: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<Vec<ChatMessageRow>> {
        let (before_created_at, before_id) = match before {
            Some((created_at, id)) => (Some(created_at), Some(id)),
            None => (None, None),
        };
        sqlx::query_as::<_, ChatMessageRow>(
            "SELECT message.* FROM chat_message AS message \
             LEFT JOIN agent_task_queue AS owner ON owner.id = message.task_id \
             WHERE message.chat_session_id = $1 \
               AND message.message_kind != 'channel_command' \
               AND ( \
                   (message.role = 'user' \
                    AND (message.channel_context_revision = $2 \
                         OR ($2 = 1 AND message.channel_context_revision IS NULL))) \
                   OR \
                   (message.role != 'user' \
                    AND (owner.channel_context_revision = $2 \
                         OR ($2 = 1 AND owner.channel_context_revision IS NULL))) \
               ) \
               AND ($3::timestamptz IS NULL \
                    OR (message.created_at, message.id) < ($3::timestamptz, $4::uuid)) \
             ORDER BY message.created_at DESC, message.id DESC \
             LIMIT $5",
        )
        .bind(session_id)
        .bind(revision)
        .bind(before_created_at)
        .bind(before_id)
        .bind(fetch_limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }
}

impl crate::RepoWithDb for ChatHistoryRepo {
    fn db(&self) -> &mc_db::Db {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    //! 真库语义（`MULTICA_TEST_DATABASE_URL`；见 `crate::chat_task::tests` 的说明）。
    //!
    //! 两条分页路径的分界就在这里：`context_revision` 有效 ⇒ 代际过滤（渠道），否则 ⇒
    //! 可见头分页（web）。两者的行集在同一个会话上**必然不同**，所以两侧都要真库断言。
    #![allow(clippy::too_many_lines)]

    use chrono::{DateTime, Timelike, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::chat_task::tests::{
        insert_message, insert_task, new_session, setup, teardown, Fixture, MessageSeed, TaskSeed,
    };

    // 同一批时间戳里刻意放了两条**同一秒内**的消息（`04.100000` / `04.200000`）：
    // 游标丢精度就丢行，这一对就是探针。
    const M0: &str = "2026-03-01 00:00:01.100000+00";
    const M1: &str = "2026-03-01 00:00:02.200000+00";
    const M2: &str = "2026-03-01 00:00:02.500000+00";
    const M3: &str = "2026-03-01 00:00:03.000000+00";
    const M4: &str = "2026-03-01 00:00:04.100000+00";
    const M5: &str = "2026-03-01 00:00:04.200000+00";
    const M6: &str = "2026-03-01 00:00:05.100000+00";
    const M7: &str = "2026-03-01 00:00:05.200000+00";
    const M8: &str = "2026-03-01 00:00:05.300000+00";

    fn history(fixture: &Fixture) -> ChatHistoryRepo {
        ChatHistoryRepo::new(fixture.db.clone())
    }

    fn pages(fixture: &Fixture) -> ChatMessageRepo {
        ChatMessageRepo::new(fixture.db.clone())
    }

    fn cursor_of(row: &ChatMessageRow) -> (DateTime<Utc>, Uuid) {
        (row.created_at, row.id)
    }

    async fn plain_session(fixture: &Fixture) -> Uuid {
        new_session(fixture, fixture.agent_id, "active").await
    }

    /// 翻三页拼回全集：无重复、无缺口，且每页都是时间倒序。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn transcript_page_walks_the_whole_transcript_without_gaps_or_duplicates() {
        let Some(fixture) = setup().await else {
            println!("skip transcript_page_walks_the_whole_transcript_without_gaps_or_duplicates: no env");
            return;
        };
        let session = plain_session(&fixture).await;
        let mut inserted = Vec::new();
        for at in [M0, M1, M2, M3, M4, M5] {
            inserted.push(insert_message(&fixture, session, MessageSeed::user(at).at(at)).await);
        }

        let page1 = pages(&fixture)
            .list_page(session, 2, None)
            .await
            .expect("page 1");
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].id, inserted[5], "新 → 旧");
        assert_eq!(page1[1].id, inserted[4]);
        assert_eq!(
            page1[1].created_at.nanosecond(),
            100_000_000,
            "驱动必须把亚秒精度带回来（游标就是它）"
        );

        let page2 = pages(&fixture)
            .list_page(session, 2, Some(cursor_of(&page1[1])))
            .await
            .expect("page 2");
        let page3 = pages(&fixture)
            .list_page(session, 2, Some(cursor_of(&page2[1])))
            .await
            .expect("page 3");
        let page4 = pages(&fixture)
            .list_page(session, 2, Some(cursor_of(&page3[1])))
            .await
            .expect("page 4");

        assert_eq!(page2[0].id, inserted[3]);
        assert_eq!(page2[1].id, inserted[2]);
        assert_eq!(page3[0].id, inserted[1]);
        assert_eq!(page3[1].id, inserted[0]);
        assert!(page4.is_empty(), "翻到底不给空行");

        let walked: Vec<Uuid> = page1
            .iter()
            .chain(&page2)
            .chain(&page3)
            .map(|row| row.id)
            .collect();
        let mut expected: Vec<Uuid> = inserted.clone();
        expected.reverse();
        assert_eq!(walked, expected, "拼回来的顺序与全集逐字一致");

        teardown(&fixture).await;
    }

    /// 游标必须是**库里的**时间戳：同一秒内的两行，用截断到整秒的游标翻页会丢行。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn transcript_page_cursor_must_keep_sub_second_precision() {
        let Some(fixture) = setup().await else {
            println!("skip transcript_page_cursor_must_keep_sub_second_precision: no env");
            return;
        };
        let session = plain_session(&fixture).await;
        let older = insert_message(&fixture, session, MessageSeed::user("older").at(M4)).await;
        let newer = insert_message(&fixture, session, MessageSeed::user("newer").at(M5)).await;

        let first = pages(&fixture)
            .list_page(session, 1, None)
            .await
            .expect("first page");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, newer);

        // 真游标（库值）：同秒的下一行仍然翻得到。
        let second = pages(&fixture)
            .list_page(session, 1, Some(cursor_of(&first[0])))
            .await
            .expect("second page");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, older);

        // 截断到整秒的游标（客户端若把时间戳格式化成秒就会这样）：同秒那一行被跳过。
        let truncated = first[0]
            .created_at
            .with_nanosecond(0)
            .expect("truncate to the second");
        let lost = pages(&fixture)
            .list_page(session, 1, Some((truncated, first[0].id)))
            .await
            .expect("truncated cursor page");
        assert!(
            lost.is_empty(),
            "截断到整秒的游标会跳过同一秒内的更旧行 ⇒ 契约要求 next_cursor 用库里的完整时间戳"
        );

        teardown(&fixture).await;
    }

    /// 可见头不变式：只有当前可见轮（及其之前的非排队轮）的 user 消息在消息流里；
    /// `channel_command` 任何情况下都不可见。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn transcript_page_hides_queued_follow_ups_and_channel_commands() {
        let Some(fixture) = setup().await else {
            println!("skip transcript_page_hides_queued_follow_ups_and_channel_commands: no env");
            return;
        };
        let session = plain_session(&fixture).await;
        // 两轮都**还在排队**：可见头按 (priority DESC, created_at ASC) 取 A。
        let head = insert_task(
            &fixture,
            session,
            TaskSeed::queued().at("2026-03-01 00:00:01+00"),
        )
        .await;
        let follow_up = insert_task(
            &fixture,
            session,
            TaskSeed::queued().at("2026-03-01 00:00:02+00"),
        )
        .await;
        // 时间戳全部显式给：同一毫秒内生成的 v7 id 之间没有顺序保证，靠 `id DESC` 断序会飘。
        let head_input = insert_message(
            &fixture,
            session,
            MessageSeed::user("head turn").on(head).at(M4),
        )
        .await;
        let follow_input = insert_message(
            &fixture,
            session,
            MessageSeed::user("follow up").on(follow_up).at(M5),
        )
        .await;
        let control = insert_message(
            &fixture,
            session,
            MessageSeed::user("[control]")
                .kind("channel_command")
                .at(M7),
        )
        .await;
        let answer = insert_message(
            &fixture,
            session,
            MessageSeed::assistant("answer").on(head).at(M6),
        )
        .await;

        let visible = pages(&fixture)
            .list_page(session, 50, None)
            .await
            .expect("page");
        let ids: Vec<Uuid> = visible.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![answer, head_input],
            "排队中的追问在成为可见头之前先藏起来（控制面记录永不可见）"
        );
        assert!(!ids.contains(&follow_input));
        assert!(!ids.contains(&control));

        // 追问被 daemon 认领（`dispatched` 压过 `queued` 的排序档）⇒ 可见头换人：
        // 新头的输入转为可见，旧头的输入退出。
        sqlx::query("UPDATE agent_task_queue SET status = 'dispatched' WHERE id = $1")
            .bind(follow_up)
            .execute(fixture.pool())
            .await
            .expect("claim the follow up");
        let visible = pages(&fixture)
            .list_page(session, 50, None)
            .await
            .expect("page after claim");
        let ids: Vec<Uuid> = visible.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![answer, follow_input],
            "新的可见头换人，旧头的输入退出"
        );
        assert!(!ids.contains(&head_input));
        assert!(!ids.contains(&control));

        teardown(&fixture).await;
    }

    /// 渠道代际过滤：user 行按自己的 `channel_context_revision`，assistant 行**继承所属任务**
    /// 的那一列；`revision = 1` 时 `NULL` 视为同一代；没有任何可见头 EXCEPT。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn transcript_page_for_channel_context_filters_by_generation() {
        let Some(fixture) = setup().await else {
            println!("skip transcript_page_for_channel_context_filters_by_generation: no env");
            return;
        };
        let session = plain_session(&fixture).await;
        // 两个代际的任务：7（当前）与 5（旧）。
        let current = insert_task(
            &fixture,
            session,
            TaskSeed::queued().status("running").revision(7),
        )
        .await;
        let legacy = insert_task(
            &fixture,
            session,
            TaskSeed::queued().status("completed").revision(5).at(M1),
        )
        .await;
        // 排队中的追问（非可见头）：渠道路径**不**做可见头过滤。
        let pending = insert_task(&fixture, session, TaskSeed::queued().at(M3).revision(7)).await;

        let in_generation = insert_message(
            &fixture,
            session,
            MessageSeed::user("gen 7").at(M1).revision(7),
        )
        .await;
        let unversioned =
            insert_message(&fixture, session, MessageSeed::user("gen null").at(M2)).await;
        let old_generation = insert_message(
            &fixture,
            session,
            MessageSeed::user("gen 5").at(M3).revision(5),
        )
        .await;
        let pending_input = insert_message(
            &fixture,
            session,
            MessageSeed::user("pending gen 7")
                .on(pending)
                .at(M4)
                .revision(7),
        )
        .await;
        let current_answer = insert_message(
            &fixture,
            session,
            MessageSeed::assistant("answer gen 7").on(current).at(M5),
        )
        .await;
        let legacy_answer = insert_message(
            &fixture,
            session,
            MessageSeed::assistant("answer gen 5").on(legacy).at(M6),
        )
        .await;
        let product_reply =
            insert_message(&fixture, session, MessageSeed::assistant("opening").at(M7)).await;
        let control = insert_message(
            &fixture,
            session,
            MessageSeed::user("[control]")
                .kind("channel_command")
                .at(M8)
                .revision(7),
        )
        .await;

        let page = history(&fixture)
            .transcript_page_for_channel_context(session, 7, 50, None)
            .await
            .expect("channel page");
        let ids: Vec<Uuid> = page.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![current_answer, pending_input, in_generation],
            "user 按自己的列，assistant 继承任务；无头过滤；代际外与控制面都不可见"
        );
        for hidden in [
            unversioned,
            old_generation,
            legacy_answer,
            product_reply,
            control,
        ] {
            assert!(
                !ids.contains(&hidden),
                "代际外 / 控制面 / 无 task 的 assistant 行"
            );
        }

        // `transcript_page` 按 `context_revision` 分发到同一条路径。
        let dispatched = history(&fixture)
            .transcript_page(session, Some(7), 50, None)
            .await
            .expect("dispatched page");
        assert_eq!(dispatched.iter().map(|row| row.id).collect::<Vec<_>>(), ids);

        // `revision = 1`：它只覆盖回填前的老数据（`NULL`）—— 每代只收自己的行，第 7 代的行
        // **不**属于第 1 代。user 行看自己的列，assistant 行看所属任务的列（无 task 时也算 NULL）。
        let first_generation = history(&fixture)
            .transcript_page_for_channel_context(session, 1, 50, None)
            .await
            .expect("first generation page");
        let ids: Vec<Uuid> = first_generation.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![product_reply, unversioned],
            "第一代 = 全部没有代际的行"
        );
        for other_generation in [
            in_generation,
            old_generation,
            pending_input,
            current_answer,
            legacy_answer,
        ] {
            assert!(!ids.contains(&other_generation), "别的代际的行不属于第一代");
        }

        // 同一会话走可见头路径（`context_revision = None`）：行集与渠道路径**不同**。
        let web = history(&fixture)
            .transcript_page(session, None, 50, None)
            .await
            .expect("web page");
        let web_ids: Vec<Uuid> = web.iter().map(|row| row.id).collect();
        assert!(
            !web_ids.contains(&pending_input),
            "可见头路径会把非头的排队输入藏起来（渠道路径不会）"
        );
        assert!(web_ids.contains(&in_generation));
        assert!(!web_ids.contains(&control));

        teardown(&fixture).await;
    }

    /// 四条单行读：任务上下文 / 会话所属 workspace / 绑定渠道类型 / 代际边界。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn single_row_reads_return_none_for_missing_rows_and_the_row_for_present_ones() {
        let Some(fixture) = setup().await else {
            println!("skip single_row_reads_return_none_for_missing_rows_and_the_row_for_present_ones: no env");
            return;
        };
        let session = plain_session(&fixture).await;
        let task = insert_task(
            &fixture,
            session,
            TaskSeed::queued().status("running").revision(7),
        )
        .await;
        let repo = history(&fixture);

        let context = repo
            .task_context(task)
            .await
            .expect("task context")
            .expect("row present");
        assert_eq!(context.id, task);
        assert_eq!(context.chat_session_id, Some(session));
        assert_eq!(context.channel_context_revision, Some(7));
        assert!(repo
            .task_context(Uuid::new_v4())
            .await
            .expect("missing task")
            .is_none());

        assert_eq!(
            repo.session_workspace(session)
                .await
                .expect("session workspace"),
            Some(fixture.workspace_id)
        );
        assert!(repo
            .session_workspace(Uuid::new_v4())
            .await
            .expect("missing session workspace")
            .is_none());

        // 没有绑定行 = 没有渠道；有绑定行 = 有渠道。
        assert!(repo
            .channel_type_for_session(session)
            .await
            .expect("unbound channel")
            .is_none());
        sqlx::query(
            "INSERT INTO channel_chat_session_binding(chat_session_id, installation_id, \
                 channel_type, channel_chat_id, chat_type, context_revision) \
             VALUES ($1, $2, 'lark', 'oc_itest', 'group', 7)",
        )
        .bind(session)
        .bind(Uuid::new_v4())
        .execute(fixture.pool())
        .await
        .expect("insert channel binding");
        assert_eq!(
            repo.channel_type_for_session(session)
                .await
                .expect("bound channel")
                .as_deref(),
            Some("lark")
        );

        assert!(repo
            .context_generation(session, 7)
            .await
            .expect("missing generation")
            .is_none());
        sqlx::query(
            "INSERT INTO channel_chat_context_generation(chat_session_id, revision, \
                 history_start_message_id, history_end_message_id, history_boundary_pending) \
             VALUES ($1, 7, 'om_start', 'om_end', TRUE)",
        )
        .bind(session)
        .execute(fixture.pool())
        .await
        .expect("insert context generation");
        let generation = repo
            .context_generation(session, 7)
            .await
            .expect("generation")
            .expect("row present");
        assert_eq!(
            generation.history_start_message_id.as_deref(),
            Some("om_start")
        );
        assert_eq!(generation.history_end_message_id.as_deref(), Some("om_end"));
        assert!(generation.history_boundary_pending);
        assert!(repo
            .context_generation(session, 8)
            .await
            .expect("other revision")
            .is_none());

        teardown(&fixture).await;
    }
}
