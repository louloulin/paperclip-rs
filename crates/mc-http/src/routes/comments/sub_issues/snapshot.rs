//! 抓取上下文的**构建**那一半：上游 `service/source_context.go` 的读侧
//! （祖先路径 → 线程历史 → 附件 → 快照）。
//!
//! 拆文件是门 ⑩（单文件 800 行硬上限）的要求，先例 = `routes/comments/dto.rs`
//! （M2-B）、`routes/uploads/serve.rs`（M10-B2）。路由面 / 错误面 / 纯函数留在
//! `sub_issues.rs`。

// 父模块是本面的**单一**依赖源（DTO / 错误 / 上限 / 工具全在 `sub_issues.rs`），
// 列全 20 个名字比 `use super::*` 可读性更差 ⇒ 逐个豁免 wildcard。
#[allow(clippy::wildcard_imports)]
use super::*;

/// 上游 `BuildSourceContext`（`service/source_context.go:404`）。
///
/// 逐条对应上游的失败出口：`ErrAnchorCommentDeleted` / `ErrSourceIssueDeleted` /
/// `ErrSourceContextInvalid` / `ErrSourceContextTooLarge`。
///
/// 三条豁免各有理由：`too_many_lines` 是因为它**逐条对照**上游 `BuildSourceContext`
/// 的分支顺序（锚评论三道 → issue → 祖先路径 → 线程历史 → 附件 → 快照 → 用量），拆开会让
/// 「上游哪一步对应本仓哪一步」断在函数之间（与 `routes/uploads.rs::upload_file` 的同款
/// 豁免）；`cast_possible_wrap` 是 `len as i64` 那一格（`len` 最大是 SQL 的 `LIMIT 257`，
/// 不可能回绕）；`result_large_err` 是因为 `Err` 侧的 `TooLarge` 带着 `limits`，而 limits
/// **必须**随错误一起回给客户端。
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_wrap,
    clippy::result_large_err
)]
pub async fn build_snapshot(state: &AppState, anchor_id: Id) -> Result<Snapshot, PreviewError> {
    // --- 锚评论（上游 `GetCommentInWorkspace` + `DeletedAt` + `Type` 三道） ---
    let anchor: AnchorRow = sqlx::query_as(
        "SELECT id, issue_id, parent_id, type, author_type, created_at, deleted_at \
         FROM comment WHERE id = $1",
    )
    .bind(anchor_id.as_uuid())
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| PreviewError::CaptureFailed)?
    .ok_or(PreviewError::AnchorCommentDeleted)?;
    if anchor.deleted_at.is_some() {
        return Err(PreviewError::AnchorCommentDeleted);
    }
    if anchor.comment_type != "comment" {
        // 上游：`anchor.Type != "comment"` ⇒ ErrSourceContextInvalid。
        return Err(PreviewError::InvalidPath);
    }

    // --- 源 issue（上游 `BuildSourceIssueSnapshot`） ---
    let source_issue = build_issue_snapshot(state, anchor.issue_id).await?;

    // --- 祖先路径（上游 `ListCommentAncestorPath`） ---
    let workspace_id = workspace_of_anchor(state, anchor_id).await?;
    let ancestors = list_ancestor_path(state, anchor_id, workspace_id, anchor.issue_id).await?;
    if ancestors.is_empty()
        || ancestors.last().is_none_or(|r| r.id != anchor_id.as_uuid())
        || ancestors.first().is_some_and(|r| r.parent_id.is_some())
    {
        return Err(PreviewError::InvalidPath);
    }
    if ancestors.len() as i64 > MAX_COMMENTS {
        return Err(PreviewError::TooLarge(LimitUsage {
            comment_count: ancestors.len(),
            ..LimitUsage::default()
        }));
    }
    if has_duplicates(&ancestors) {
        return Err(PreviewError::InvalidPath);
    }
    let root_id = ancestors[0].id;

    // --- 线程历史（上游 `ListCommentThreadHistory`：截至锚评论的**完整**线程） ---
    let history =
        list_thread_history(state, root_id, workspace_id, anchor.issue_id, &anchor).await?;
    if history.is_empty()
        || history.first().is_none_or(|r| r.id != root_id)
        || history.last().is_none_or(|r| r.id != anchor_id.as_uuid())
        || has_duplicate_ids(&history)
    {
        return Err(PreviewError::InvalidPath);
    }
    if history.len() as i64 > MAX_COMMENTS {
        return Err(PreviewError::TooLarge(LimitUsage {
            comment_count: history.len(),
            ..LimitUsage::default()
        }));
    }

    // --- 附件（issue 面 + 评论面；上游 `ListSourceContext*Attachments`） ---
    let issue_attachments = list_issue_attachments(state, workspace_id, anchor.issue_id).await?;
    let comment_ids: Vec<Uuid> = history.iter().map(|r| r.id).collect();
    let comment_attachments =
        list_comment_attachments(state, workspace_id, anchor.issue_id, &comment_ids).await?;

    // --- 快照 ---
    let mut by_comment: HashMap<Uuid, Vec<AttachmentSnapshot>> = HashMap::new();
    for att in &comment_attachments {
        let comment_uuid = att.comment_id.expect("评论附件必有 comment_id");
        by_comment
            .entry(comment_uuid)
            .or_default()
            .push(attachment_snapshot(
                att,
                "comment",
                &comment_uuid.to_string(),
            ));
    }
    let mut comment_thread = Vec::with_capacity(history.len());
    for row in &history {
        let name = resolve_author_name(state, &row.author_type, row.author_id).await?;
        comment_thread.push(CommentSnapshot {
            id: row.id.to_string(),
            parent_id: row.parent_id.map(|p| p.to_string()),
            comment_type: row.comment_type.clone(),
            content: if row.deleted_at.is_some() {
                // 墓碑的 `content` 是空的（上游 `Deleted` 注释逐字）。
                String::new()
            } else {
                row.content.clone()
            },
            author: AuthorSnapshot {
                author_type: row.author_type.clone(),
                id: row.author_id.to_string(),
                name,
            },
            created_at: rfc3339(&row.created_at),
            updated_at: rfc3339(&row.updated_at),
            revision: row.revision,
            attachments: by_comment.remove(&row.id).unwrap_or_default(),
            deleted: row.deleted_at.is_some(),
        });
    }

    let mut source_issue = source_issue;
    for att in &issue_attachments {
        source_issue
            .attachments
            .push(attachment_snapshot(att, "issue", &source_issue.id));
    }
    let snapshot = Snapshot {
        version: 0,
        captured_by_user_id: String::new(),
        captured_at: String::new(),
        source_issue,
        comment_thread,
        anchor_comment_id: anchor_id.to_string(),
    };
    let limits = limit_usage(&snapshot);
    if limits.text_bytes > MAX_TEXT_BYTES
        || limits.attachment_count > MAX_ATTACHMENTS
        || limits.attachment_bytes > MAX_ATTACHMENT_BYTES
    {
        return Err(PreviewError::TooLarge(limits));
    }
    Ok(snapshot)
}

// ---------------------------------------------------------------------------
// 查询（上游 `comment.sql:762/790` 与 `attachment.sql:57/70` 逐字）
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
pub(super) struct AnchorRow {
    id: Uuid,
    issue_id: Uuid,
    #[allow(dead_code)]
    parent_id: Option<Uuid>,
    #[sqlx(rename = "type")]
    comment_type: String,
    #[allow(dead_code)]
    author_type: String,
    created_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
pub(super) struct AncestorRow {
    id: Uuid,
    parent_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
pub(super) struct HistoryRow {
    id: Uuid,
    parent_id: Option<Uuid>,
    #[sqlx(rename = "type")]
    comment_type: String,
    content: String,
    author_type: String,
    author_id: Uuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    revision: i64,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
pub(super) struct AttachmentRow {
    id: Uuid,
    comment_id: Option<Uuid>,
    filename: String,
    content_type: String,
    size_bytes: i64,
    created_at: DateTime<Utc>,
}

pub(super) const ATTACHMENT_COLUMNS: &str =
    "id, comment_id, filename, content_type, size_bytes, created_at";

/// 锚评论所在的工作区（上游那条 `GetCommentInWorkspace` 的 `workspace_id` 那一格）。
pub(super) async fn workspace_of_anchor(
    state: &AppState,
    anchor_id: Id,
) -> Result<Uuid, PreviewError> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT workspace_id FROM comment WHERE id = $1")
        .bind(anchor_id.as_uuid())
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| PreviewError::CaptureFailed)?;
    row.map(|(id,)| id)
        .ok_or(PreviewError::AnchorCommentDeleted)
}

/// 上游 `BuildSourceIssueSnapshot`（`service/source_context.go:257`）。
///
/// 逐条：issue 不存在 ⇒ `ErrSourceIssueDeleted`；`identifier` 在本仓**是列**（上游是
/// `workspace.IssuePrefix + "-" + number` 现场拼的）⇒ 直接取列值。
#[allow(clippy::result_large_err)] // 同上：`TooLarge` 要带 `limits`。
pub async fn build_issue_snapshot(
    state: &AppState,
    issue_id: Uuid,
) -> Result<IssueSnapshot, PreviewError> {
    let row: Option<IssueCoreRow> = sqlx::query_as(
        "SELECT identifier, number, title, description, revision, created_at, updated_at \
             FROM issue WHERE id = $1",
    )
    .bind(issue_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| PreviewError::CaptureFailed)?;
    let Some(core) = row else {
        return Err(PreviewError::SourceIssueDeleted);
    };
    Ok(IssueSnapshot {
        id: issue_id.to_string(),
        identifier: core.identifier,
        number: core.number,
        title: core.title,
        description: core.description,
        created_at: rfc3339(&core.created_at),
        updated_at: rfc3339(&core.updated_at),
        revision: core.revision,
        // 附件在 `build_snapshot` 里回填（需要 workspace 条件）。
        attachments: Vec::new(),
    })
}

/// issue 快照的**非附件**部分（上游 `GetIssueInWorkspace` 的那一行）。
#[derive(sqlx::FromRow)]
pub(super) struct IssueCoreRow {
    identifier: String,
    number: i32,
    title: String,
    description: Option<String>,
    revision: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// 上游 `ListCommentAncestorPath`（`comment.sql:762`）：从锚评论一路向上，**根在前**。
///
/// 游标防护逐字对齐上游那一段：`path.depth <= 256` **且** `NOT path.cycle`。
/// 上游用 `visited_ids` 数组判环；本仓用 `c.id = ANY(path.visited)` 的等价写法 ——
/// `cycle` 标记由 `depth < 256` 这一格封顶（与上游同一条上界）。
pub(super) async fn list_ancestor_path(
    state: &AppState,
    comment_id: Id,
    workspace_id: Uuid,
    issue_id: Uuid,
) -> Result<Vec<AncestorRow>, PreviewError> {
    let rows = sqlx::query_as::<_, AncestorRow>(
        "WITH RECURSIVE ancestor_path AS ( \
             SELECT c.id, c.parent_id, ARRAY[c.id]::uuid[] AS visited_ids, \
                    1::integer AS depth, false AS cycle \
             FROM comment c \
             WHERE c.id = $1 AND c.workspace_id = $2 AND c.issue_id = $3 \
           UNION ALL \
             SELECT parent.id, parent.parent_id, path.visited_ids || parent.id, \
                    path.depth + 1, parent.id = ANY(path.visited_ids) \
             FROM ancestor_path path \
             JOIN comment parent ON parent.id = path.parent_id \
             WHERE parent.workspace_id = $2 AND parent.issue_id = $3 \
               AND path.depth <= 256 AND NOT path.cycle \
         ) \
         SELECT id, parent_id FROM ancestor_path ORDER BY depth DESC",
    )
    .bind(comment_id.as_uuid())
    .bind(workspace_id)
    .bind(issue_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| PreviewError::CaptureFailed)?;
    Ok(rows)
}

/// 上游 `ListCommentThreadHistory`（`comment.sql:790`）：根到锚评论的**完整**线程。
///
/// 时间戳并列时用 **UUID** 作稳定 tiebreaker（上游注释逐字：issue 时间线就是这么破平局的）
/// ⇒ 边界是 `(created_at, id) <= (anchor_created_at, anchor_id)`。
pub(super) async fn list_thread_history(
    state: &AppState,
    root_id: Uuid,
    workspace_id: Uuid,
    issue_id: Uuid,
    anchor: &AnchorRow,
) -> Result<Vec<HistoryRow>, PreviewError> {
    const THREAD_HISTORY_SQL: &str = "WITH RECURSIVE thread_history AS ( \
             SELECT c.* FROM comment c \
             WHERE c.id = $1 AND c.workspace_id = $2 AND c.issue_id = $3 \
               AND c.parent_id IS NULL \
               AND (c.created_at, c.id) <= ($4::timestamptz, $5::uuid) \
           UNION ALL \
             SELECT child.* FROM comment child \
             JOIN thread_history parent ON child.parent_id = parent.id \
             WHERE child.workspace_id = $2 AND child.issue_id = $3 \
               AND (child.created_at, child.id) <= ($4::timestamptz, $5::uuid) \
         ) \
         SELECT id, parent_id, type, content, author_type, author_id, \
                created_at, updated_at, revision, deleted_at \
         FROM thread_history ORDER BY created_at, id LIMIT $6";
    sqlx::query_as::<_, HistoryRow>(THREAD_HISTORY_SQL)
        .bind(root_id)
        .bind(workspace_id)
        .bind(issue_id)
        .bind(anchor.created_at)
        .bind(anchor.id)
        .bind(MAX_COMMENTS + 1)
        .fetch_all(state.db.pool())
        .await
        .map_err(|_| PreviewError::CaptureFailed)
}

/// 上游 `ListSourceContextIssueAttachments`（`attachment.sql:57`）。
pub(super) async fn list_issue_attachments(
    state: &AppState,
    workspace_id: Uuid,
    issue_id: Uuid,
) -> Result<Vec<AttachmentRow>, PreviewError> {
    sqlx::query_as::<_, AttachmentRow>(&format!(
        "SELECT {ATTACHMENT_COLUMNS} FROM attachment \
         WHERE workspace_id = $1 AND issue_id = $2 \
           AND comment_id IS NULL AND source_context_id IS NULL \
         ORDER BY created_at ASC, id ASC"
    ))
    .bind(workspace_id)
    .bind(issue_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| PreviewError::CaptureFailed)
}

/// 上游 `ListSourceContextCommentAttachments`（`attachment.sql:70`）。
pub(super) async fn list_comment_attachments(
    state: &AppState,
    workspace_id: Uuid,
    issue_id: Uuid,
    comment_ids: &[Uuid],
) -> Result<Vec<AttachmentRow>, PreviewError> {
    if comment_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, AttachmentRow>(&format!(
        "SELECT {ATTACHMENT_COLUMNS} FROM attachment \
         WHERE workspace_id = $1 AND issue_id = $2 AND comment_id = ANY($3::uuid[]) \
           AND source_context_id IS NULL \
         ORDER BY created_at ASC, id ASC"
    ))
    .bind(workspace_id)
    .bind(issue_id)
    .bind(comment_ids)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| PreviewError::CaptureFailed)
}

/// 附件行 → 快照项（上游 `attachmentSnapshot`）。
pub(super) fn attachment_snapshot(
    att: &AttachmentRow,
    owner_type: &str,
    owner_id: &str,
) -> AttachmentSnapshot {
    AttachmentSnapshot {
        id: att.id.to_string(),
        owner_type: owner_type.to_owned(),
        owner_id: owner_id.to_owned(),
        filename: att.filename.clone(),
        content_type: att.content_type.clone(),
        size_bytes: att.size_bytes,
        created_at: rfc3339(&att.created_at),
    }
}

/// 上游 `sourceContextAuthorName`：`member` / `agent` 两种；其它类型上游是**报错**。
///
/// 本仓的 `author_type` 词表比上游宽（`user` 是本仓对 `member` 的本地写法，
/// 见迁移 `538_actor_type_vocabulary`）⇒ 两种拼写都当同一个人类作者。
pub(super) async fn resolve_author_name(
    state: &AppState,
    author_type: &str,
    author_id: Uuid,
) -> Result<String, PreviewError> {
    let name: Option<(String,)> = match author_type {
        "member" | "user" => sqlx::query_as("SELECT name FROM \"user\" WHERE id = $1")
            .bind(author_id)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|_| PreviewError::CaptureFailed)?,
        "agent" => sqlx::query_as("SELECT name FROM agent WHERE id = $1")
            .bind(author_id)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|_| PreviewError::CaptureFailed)?,
        // 上游：unsupported author type ⇒ 捕获失败（500），不是静默留空名。
        _ => return Err(PreviewError::CaptureFailed),
    };
    name.map(|(n,)| n).ok_or(PreviewError::CaptureFailed)
}

pub(super) fn has_duplicates(rows: &[AncestorRow]) -> bool {
    let mut seen = HashSet::new();
    rows.iter().any(|r| !seen.insert(r.id))
}

pub(super) fn has_duplicate_ids(rows: &[HistoryRow]) -> bool {
    let mut seen = HashSet::new();
    rows.iter().any(|r| !seen.insert(r.id))
}
