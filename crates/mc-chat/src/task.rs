//! chat **派发面**的纯领域规则（M4-4 / LUM-1475）：任务队列状态集合、排队位置语义、
//! `wait_reason` 的显示门、以及发消息时要落的首条标题（`chattitle.Derive`）。
//!
//! 本模块**不做 I/O**：SQL 在 `mc_repos::chat_task`，HTTP 在
//! `mc_http::routes::chat::task`。每条规则对回上游原文：
//!
//! | 规则 | 上游出处 |
//! | --- | --- |
//! | 非终态 5 值 = pending：`queued` / `dispatched` / `running` / `waiting_local_directory` / `deferred` | `chat.sql` `HasActiveChatTaskForSession` / `ListPendingChatTasksForSession` / `ListPendingChatTasksByCreator` |
//! | `queued` = **位置**语义，不是 DB 状态：同会话已有更早的可见任务才算排队 | `task.go:2360` `HasPendingChatTurnForSession` |
//! | `regenerate_quick_actions_for IS NULL` 的后台刷新不计入 pending | `chat.sql` 两条 pending 查询（MUL-5149） |
//! | `wait_reason` 只在 `waiting_local_directory` 时下发 | `chat.go:1302` `waitReasonForStatus` |
//! | chat 任务优先级恒 2（medium） | `task.go:2377` `Priority: 2`（注释：matches `EnqueueChatTask`）|
//! | `supports_queue` 恒 `true` | `chat.go` 三处响应字面量 |
//! | 首条标题 = 第一行非空 + 去 Markdown + 30 字符 + 单省略号 | `chat_title.go:22` `Derive` |
//! | 归档会话 / 归档 agent / 无 runtime agent 的三条拒绝文案 | `chat.go` send 分支 |
//!
//! ⚠️ **本仓 `migrations/0001_init.up.sql:230` 的 `agent_task_queue.status` CHECK 是错的**
//! （缺 `waiting_local_directory` / `deferred` 等取值）。状态集合的真值是
//! `contracts/upstream-schema.sql` + 上游迁移，本模块只照抄上游查询里的字面集合。

/// chat 任务的优先级（上游 `SendDirectChatMessage` 的 `Priority: 2`，注释说「matches
/// `EnqueueChatTask`」——两条 chat 入队路径必须同优先级，否则直接发消息与渠道入队会互相插队）。
pub const PRIORITY_CHAT: i32 = 2;

/// `queued` —— 已入库、还没被 daemon 认领。**产品语义上**它只是「位置」：
/// 见 [`position_is_queued`]。
pub const STATUS_QUEUED: &str = "queued";
/// `dispatched` —— 已挂到某个 runtime 连接上。
pub const STATUS_DISPATCHED: &str = "dispatched";
/// `running` —— agent 正在跑。
pub const STATUS_RUNNING: &str = "running";
/// `waiting_local_directory` —— 被本地目录租约挡住（唯一会下发 `wait_reason` 的状态）。
pub const STATUS_WAITING_LOCAL_DIRECTORY: &str = "waiting_local_directory";
/// `deferred` —— 延迟重试（`fire_at` 未到），**也算 pending**：它是在当前 turn 之前恢复的旧 turn。
pub const STATUS_DEFERRED: &str = "deferred";

/// 上游所有 pending/非终态查询共用的 5 值集合（字面来源见模块头）。
pub const PENDING_STATUSES: [&str; 5] = [
    STATUS_QUEUED,
    STATUS_DISPATCHED,
    STATUS_RUNNING,
    STATUS_WAITING_LOCAL_DIRECTORY,
    STATUS_DEFERRED,
];

/// 「正在跑」的 3 值集合（`HasActiveChatTaskForSession` 与 `regenerate` 的忙判据同款）：
/// `queued` / `deferred` 还没开始，不算 active。
pub const ACTIVE_STATUSES: [&str; 3] = [
    STATUS_DISPATCHED,
    STATUS_RUNNING,
    STATUS_WAITING_LOCAL_DIRECTORY,
];

/// 该状态是否算「在飞」（pending）。
pub fn is_pending(status: &str) -> bool {
    PENDING_STATUSES.contains(&status)
}

/// 该状态是否算「已开始执行」（`HasActiveChatTaskForSession` 的口径）。
pub fn is_active(status: &str) -> bool {
    ACTIVE_STATUSES.contains(&status)
}

/// 上游 `waitReasonForStatus`（`chat.go:1302`）：`wait_reason` 进入 hold 时写一次、
/// 恢复时**从不**清空，所以只有 `waiting_local_directory` 才把它下发；否则十分钟前那句
/// 「held by task abc12345」会读成当前正在生效的解释。
pub fn wait_reason_for_status(status: &str, reason: Option<&str>) -> String {
    if status != STATUS_WAITING_LOCAL_DIRECTORY {
        return String::new();
    }
    reason.unwrap_or_default().trim().to_owned()
}

/// 上游 `SendDirectChatMessage` 对 `queued` 的定义（`task.go:2360` 注释）：
///
/// > 每个新建任务的 DB 状态都是 `queued`，直到 daemon 认领；**产品排队语义是位置**：
/// > 只有当同一会话里已经有一个可见任务排在它前面时，这次发送才算 follow-up。
/// > deferred 重试也算，因为它是在本 turn 之前恢复更早的 turn。
///
/// `has_pending_turn_before_insert == true` 就是「排在别人后面」。
pub fn position_is_queued(has_pending_turn_before_insert: bool) -> bool {
    has_pending_turn_before_insert
}

/// `supports_queue`：三处 chat 响应都写死 `true`（`chat.go` 的 `SendChatMessageResponse` /
/// `PendingChatTaskResponse`）。放在领域层是为了让「本仓支持排队」成为一个**被引用的常量**
/// 而不是散落的字面量。
pub const SUPPORTS_QUEUE: bool = true;

/// 上游 send 路径在事务内/事务外重查后可能返回的三条冲突（`chat.go:930` 的 `switch`）。
///
/// 注意状态码**不同**：handler 门里读到的归档会话是 **400** `chat session is archived`，
/// 而事务内重查（并发归档）落的是 **409** ——本枚举只表达后者。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendReject {
    /// `ErrChatSessionArchived` → 409 `chat session is archived`。
    #[error("chat session is archived")]
    SessionArchived,
    /// `ErrChatTaskAgentArchived` → 409 `chat agent is archived`。
    #[error("chat agent is archived")]
    AgentArchived,
    /// `ErrChatTaskAgentNoRuntime` → 409 `chat agent has no runtime`。
    #[error("chat agent has no runtime")]
    NoRuntime,
}

// ---------------------------------------------------------------------------
// 标题派生（上游 `chat_title.go` 的 `Derive`，296 行里被 send 路径用到的那一段）
// ---------------------------------------------------------------------------

/// 标题字符数上限（上游 `deterministicTitleLimit = 30`，按**rune** 计）。
pub const TITLE_LIMIT: usize = 30;

/// 上游 `chattitle.Derive`：从首条用户消息派生一个确定性的会话标题。
///
/// 逐字对齐 Go 实现（`chat_title.go:22`）的四步：
/// 1. 取**第一行非空**（Go `strings.TrimSpace` 判空，Unicode 空白）；
/// 2. 抹掉行内 ```` ```...``` ```` 代码围栏（Go 正则 `.` 不跨行、且 `.*?` 非贪婪
///    ⇒ 只有**同行的**成对围栏才被匹配，没配对的保持原样）；
/// 3. 去掉 Markdown 标记字符（Go 的 `markdownMarks`：`#`、`*`、反引号、`>`、`~`、
///    `_`），再把 `!?[文字](链接)` 还原成「文字」；
/// 4. ASCII 空白折叠成单空格 + Unicode trim；超 30 字符则截到 29 + 单省略号 `…`
///    （截断后**先**去掉右侧空白再拼省略号）。
///
/// 返回空串表示「派生不出标题」——上游调用方据此**跳过** `InitializeChatSessionTitle`
/// 的 CAS，而不是拿空串去写（`if title := chattitle.Derive(content); title != ""`）。
pub fn derive_title(body: &str) -> String {
    let mut line = "";
    for candidate in body.split('\n') {
        if !candidate.trim().is_empty() {
            line = candidate;
            break;
        }
    }
    let line = strip_fences(line);
    let line = strip_marks(&line);
    let line = unwrap_links(&line);
    let line = collapse_ascii_space(&line);
    let line = line.trim();
    if line.is_empty() {
        return String::new();
    }
    let runes: Vec<char> = line.chars().collect();
    if runes.len() <= TITLE_LIMIT {
        return line.to_owned();
    }
    let head: String = runes[..TITLE_LIMIT - 1].iter().collect();
    format!("{}…", head.trim_end())
}

/// `` ```...``` `` → `" "`（Go `markdownFence.ReplaceAllString(line, " ")`）。
///
/// 只在一行内匹配（上游正则的 `.` 不匹配 `\n`），非贪婪 ⇒ 闭合围栏取**最近的**一个；
/// 没有闭合就整行不动。每次替换成一个空格，并从句柄**之后**继续扫描（Go 的 `ReplaceAll`
/// 不重叠）。
fn strip_fences(line: &str) -> String {
    const FENCE: &str = "```";
    let mut out = String::with_capacity(line.len());
    let mut pos = 0usize;
    while let Some(rel) = line[pos..].find(FENCE) {
        let open = pos + rel;
        let search_from = open + FENCE.len();
        match line[search_from..].find(FENCE) {
            Some(rel_close) => {
                let close = search_from + rel_close + FENCE.len();
                out.push_str(&line[pos..open]);
                out.push(' ');
                pos = close;
            }
            None => break,
        }
    }
    out.push_str(&line[pos..]);
    out
}

/// 去掉 Markdown 标记字符（Go 的 `markdownMarks` 集合：`#`、`*`、反引号、`>`、`~`、`_`，
/// 逐字符删除）。
fn strip_marks(line: &str) -> String {
    line.chars()
        .filter(|c| !matches!(c, '#' | '*' | '`' | '>' | '~' | '_'))
        .collect()
}

/// `!?[文字](链接)` → `文字`（Go `markdownLink = !?\[([^\]]*)\]\([^)]*\)`）。
///
/// 与 Go 的匹配语义一致的三点：文字部分不跨 `]`（`[^\]]*`）、链接部分不跨 `)`
/// （`[^)]*`）、失败时从**下一个字符**重新尝试起始位置（所以 `[a][b](u)` 里只有
/// `[b](u)` 被替换）。可选的 `!` 前缀（图片）会一起吃掉。
fn unwrap_links(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut pos = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let open = if bytes[i] == b'[' {
            Some(i)
        } else if bytes[i] == b'!' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            Some(i + 1)
        } else {
            None
        };
        let Some(open) = open else {
            i += 1;
            continue;
        };
        // `[^\]]*` 在 `]` 处停（贪婪与否无关：字符类不含 `]`）。
        let Some(rel_close) = line[open + 1..].find(']') else {
            i += 1;
            continue;
        };
        let close = open + 1 + rel_close;
        if close + 1 >= bytes.len() || bytes[close + 1] != b'(' {
            i += 1;
            continue;
        }
        let Some(rel_end) = line[close + 2..].find(')') else {
            i += 1;
            continue;
        };
        let end = close + 2 + rel_end + 1;
        out.push_str(&line[pos..i]);
        out.push_str(&line[open + 1..close]);
        pos = end;
        i = end;
    }
    out.push_str(&line[pos..]);
    out
}

/// `[[:space:]]+` → `" "`（Go 的 POSIX 类只含 ASCII 空白：`\t\n\v\f\r `）。
fn collapse_ascii_space(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_space = false;
    for c in line.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r') {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// pending 行投影
// ---------------------------------------------------------------------------

/// 一行 pending/queued chat 任务（`ListPendingChatTasksForSession` 的可见投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTaskRow {
    /// `agent_task_queue.id`。
    pub task_id: uuid::Uuid,
    /// `agent_task_queue.status`（未归一化，`is_pending` 才是判据）。
    pub status: String,
    /// `agent_task_queue.created_at`（响应里的 `created_at` 用**秒精度**渲染）。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `wait_reason`（只在 `waiting_local_directory` 时下发，见 [`wait_reason_for_status`]）。
    pub wait_reason: Option<String>,
    /// 该任务输入批次里的用户消息 id（`queued_tasks[].message_id`；head 不下发）。
    pub message_id: Option<uuid::Uuid>,
    /// 该消息正文（`queued_tasks[].content`）。
    pub content: Option<String>,
}

/// 上游 `GetPendingChatTask` 的可见投影：`tasks` 按可见头顺序给出，`head` 是当前任务，
/// `queued` 只含 **status == queued** 的后续行（`dispatched` 的后续行不下发 ——
/// 上游 `for _, task := range tasks[1:] { if task.Status != "queued" { continue } }`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTaskProjection {
    /// 当前任务（`tasks[0]`）；无 pending 时为 `None`。
    pub head: Option<PendingTaskRow>,
    /// `head` 之后的 `queued` 行（顺序 = SQL 顺序）。
    pub queued: Vec<PendingTaskRow>,
    /// 上游 `supports_queue`，恒 [`SUPPORTS_QUEUE`]。
    pub supports_queue: bool,
}

/// 把 `ListPendingChatTasksForSession` 的行序列投影成响应形状。
pub fn project_pending_tasks(mut tasks: Vec<PendingTaskRow>) -> PendingTaskProjection {
    if tasks.is_empty() {
        return PendingTaskProjection {
            head: None,
            queued: Vec::new(),
            supports_queue: SUPPORTS_QUEUE,
        };
    }
    let rest = tasks.split_off(1);
    let head = tasks.pop();
    PendingTaskProjection {
        head,
        queued: rest
            .into_iter()
            .filter(|t| t.status == STATUS_QUEUED)
            .collect(),
        supports_queue: SUPPORTS_QUEUE,
    }
}

/// 上游 `ListPendingChatTasks` 的一行（`PendingChatTaskItem`）：只有 `task_id` / `status` /
/// `chat_session_id` 三个字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChatTaskItem {
    /// `agent_task_queue.id`。
    pub task_id: uuid::Uuid,
    /// 未归一化状态。
    pub status: String,
    /// `chat_session.id`。
    pub chat_session_id: uuid::Uuid,
}

/// 上游 `PrioritizeQueuedChatTask` 的 CAS 结果（`chat.sql` `PrioritizeQueuedChatTask`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrioritizeOutcome {
    /// 被提权的任务 id（= 请求里的 `task_id`）。
    pub task_id: uuid::Uuid,
    /// 被顶下去的当前任务 id；**没有**当前任务时为 `None`（响应里 `active_task_id` 省略）。
    pub active_task_id: Option<uuid::Uuid>,
}

/// `/api/chat/pending-tasks` 的可访问 agent 过滤：`allowed` 为空时上游**跳过整次查询**
/// 直接返回空集（`if len(allowed) == 0`），这是可观察行为（少一次往返、且不会把
/// 「看不见」误报成「没有」）。两个 handler 共用这一条判据。
pub fn skip_pending_query(allowed_agents: usize) -> bool {
    allowed_agents == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn row(status: &str, secs: i64) -> PendingTaskRow {
        PendingTaskRow {
            task_id: uuid::Uuid::from_u128(u128::from(secs.unsigned_abs())),
            status: status.to_owned(),
            created_at: chrono::Utc.timestamp_opt(secs, 0).unwrap(),
            wait_reason: None,
            message_id: None,
            content: None,
        }
    }

    #[test]
    fn pending_status_set_matches_upstream_queries() {
        for status in PENDING_STATUSES {
            assert!(is_pending(status), "{status} 是上游 pending 集合成员");
        }
        for status in [
            "finished",
            "failed",
            "cancelled",
            "waiting_local_directory?",
        ] {
            assert!(!is_pending(status), "{status} 不是 pending");
        }
        // active 是 pending 的真子集（queued/deferred 还没开始跑）。
        for status in ACTIVE_STATUSES {
            assert!(is_pending(status));
        }
        assert!(!is_active(STATUS_QUEUED));
        assert!(!is_active(STATUS_DEFERRED));
    }

    #[test]
    fn wait_reason_only_survives_on_waiting_local_directory() {
        assert_eq!(
            wait_reason_for_status(
                STATUS_WAITING_LOCAL_DIRECTORY,
                Some("  held by task a1b2  ")
            ),
            "held by task a1b2"
        );
        // 状态走后，残留的 wait_reason 绝不能下发（旧解释会被读成当前解释）。
        assert_eq!(
            wait_reason_for_status(STATUS_RUNNING, Some("held by task a1b2")),
            ""
        );
        assert_eq!(
            wait_reason_for_status(STATUS_WAITING_LOCAL_DIRECTORY, None),
            ""
        );
    }

    #[test]
    fn queued_is_a_question_about_position_not_status() {
        assert!(!position_is_queued(false));
        assert!(position_is_queued(true));
        assert_eq!(PRIORITY_CHAT, 2);
        // 借一个非 const 绑定，免得 `assert!` 在常量上被判「值恒真」（门 ③）。
        let supports_queue: bool = SUPPORTS_QUEUE;
        assert!(supports_queue);
    }

    #[test]
    fn pending_projection_keeps_head_and_only_status_queued_followups() {
        let projection = project_pending_tasks(Vec::new());
        assert!(projection.head.is_none());
        assert!(projection.queued.is_empty());
        assert!(projection.supports_queue);

        let projection = project_pending_tasks(vec![
            row(STATUS_RUNNING, 1),
            row(STATUS_QUEUED, 2),
            row(STATUS_DISPATCHED, 3),
            row(STATUS_QUEUED, 4),
        ]);
        let head = projection.head.expect("head");
        assert_eq!(head.status, STATUS_RUNNING);
        // dispatched 的后续行不下发：它已经在跑，不是「排在你后面」。
        assert_eq!(
            projection
                .queued
                .iter()
                .map(|t| t.status.as_str())
                .collect::<Vec<_>>(),
            vec![STATUS_QUEUED, STATUS_QUEUED]
        );
    }

    #[test]
    fn pending_query_is_skipped_when_no_agent_is_visible() {
        assert!(skip_pending_query(0));
        assert!(!skip_pending_query(1));
    }

    #[test]
    fn title_takes_the_first_non_empty_line() {
        assert_eq!(derive_title("\n\n  hello world  "), "hello world");
        assert_eq!(derive_title("first line\nsecond line"), "first line");
        assert_eq!(derive_title("   \n\t\n "), "");
    }

    #[test]
    fn title_strips_markdown_and_flattens_space() {
        // 标记字符删除 + 空白折叠：`#`/`*`/`_` 全去，多空格折一。
        assert_eq!(derive_title("# 修复   _登录_  bug"), "修复 登录 bug");
        assert_eq!(derive_title("> quoted\ttext"), "quoted text");
        // 围栏只抹掉**成对且同行**的内容（Go `.` 不跨行、`.*?` 非贪婪）。
        assert_eq!(derive_title("use ```let x = 1;``` here"), "use here");
        assert_eq!(
            derive_title("unclosed ```fence stays"),
            "unclosed fence stays"
        );
        // 链接还原成文字；图片链接吃 `!`；坏链接不动。
        assert_eq!(derive_title("see [docs](https://x/y) now"), "see docs now");
        assert_eq!(derive_title("![alt](https://x/y)"), "alt");
        assert_eq!(derive_title("[not a link]"), "[not a link]");
        assert_eq!(derive_title("[a][b](u)"), "[a]b");
    }

    #[test]
    fn title_is_capped_at_thirty_runes_with_one_ellipsis() {
        let exact = "a".repeat(TITLE_LIMIT);
        assert_eq!(derive_title(&exact), exact);
        let long = "b".repeat(TITLE_LIMIT + 5);
        let derived = derive_title(&long);
        assert_eq!(derived.chars().count(), TITLE_LIMIT);
        assert_eq!(derived, format!("{}…", "b".repeat(TITLE_LIMIT - 1)));
        // 按 rune 而非字节截断：31 个汉字 = 31 runes。
        let wide = "汉".repeat(TITLE_LIMIT + 1);
        assert_eq!(derive_title(&wide).chars().count(), TITLE_LIMIT);
        // 截断前先 `TrimSpace`（Go 的顺序）：尾空白先被去掉，31 ≠ 超长。
        let trailing = format!("{} ", "c".repeat(TITLE_LIMIT));
        assert_eq!(derive_title(&trailing), "c".repeat(TITLE_LIMIT));
        // 第 29 个 rune 是空白时：先剥右侧空白再拼省略号（总长 29）。
        let spaced = format!("{} yyyy", "x".repeat(28));
        let derived = derive_title(&spaced);
        assert_eq!(derived, format!("{}…", "x".repeat(28)));
        assert!(!derived.ends_with(" …"));
    }
}
