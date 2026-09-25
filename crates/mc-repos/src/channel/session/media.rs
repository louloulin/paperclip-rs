//! 媒体绑定（上游 `BindMediaRefs`）：附件行 + 挂到消息 + 占位替换 + 清 pending 标记。
//!
//! 拆出本文件是**门 ⑩** 的要求（`session.rs` 一度 1,695 行 > 800 硬限）。远端下载 / 上传在
//! **调用之前**完成（脱离 connector 的 ACK 路径）；本文件的职责只是把它们**归属**到一条
//! 消息（或一个 issue）上。

use super::tx::Tx;
use super::{
    map_sqlx_err, BindMediaRefsParams, BindMediaResult, ChannelChatSessionRepo, Id, MediaRef,
    Result, Uuid, CHANNEL_COMMAND_MESSAGE_KIND,
};

impl ChannelChatSessionRepo {
    /// 绑定媒体（上游 `BindMediaRefs`）：附件行 + 挂到消息 + 占位替换 + 清 pending 标记。
    ///
    /// 远端下载 / 上传在**调用之前**完成（脱离 connector 的 ACK 路径）。被对账器接管的
    /// 意图行**不**落附件（对象正在被删，占位文本留着）。
    ///
    /// `too_many_lines`：同一张表的两条落地路径（issue 归属 / 消息归属）加上内联占位替换，
    /// 拆开就只能靠"调用方记得按顺序调三个函数"来维持正确性。
    #[allow(clippy::too_many_lines)]
    pub async fn bind_media(&self, input: &BindMediaRefsParams) -> Result<BindMediaResult> {
        let mut result = BindMediaResult::default();
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        if !input.media_refs.is_empty() {
            let message_id = input.message_id.ok_or(crate::RepoError::NotFound)?;
            let mut created: Vec<Id> = Vec::new();
            for media in &input.media_refs {
                if media.storage_key.is_empty() || media.storage_url.is_empty() {
                    tx.rollback().await.map_err(map_sqlx_err)?;
                    return Err(crate::RepoError::Conflict);
                }
                if !self
                    .media
                    .claim_and_delete_in_tx(&mut tx, &media.storage_key)
                    .await?
                {
                    tracing::warn!(
                        storage_key = media.storage_key.as_str(),
                        "channel media: intent claimed by reconciler; skipping attach"
                    );
                    continue;
                }
                let attachment_id = Id::new();
                let filename = if media.filename.is_empty() {
                    default_media_filename(media)
                } else {
                    media.filename.clone()
                };
                let content_type = if media.mime_type.is_empty() {
                    "application/octet-stream".to_string()
                } else {
                    media.mime_type.clone()
                };
                sqlx::query(
                    "INSERT INTO attachment (id, workspace_id, issue_id, chat_session_id, \
                     chat_message_id, uploader_type, uploader_id, filename, url, content_type, \
                     size_bytes) \
                     VALUES ($1, $2, $3, $4, NULL, 'member', $5, $6, $7, $8, $9)",
                )
                .bind(attachment_id.0)
                .bind(input.workspace_id.0)
                .bind(input.issue_id.map(|id| id.0))
                .bind(if input.issue_id.is_some() {
                    None
                } else {
                    Some(input.session_id.0)
                })
                .bind(input.sender.0)
                .bind(&filename)
                .bind(&media.storage_url)
                .bind(&content_type)
                .bind(media.size_bytes)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
                created.push(attachment_id);
            }
            if !created.is_empty() {
                if input.issue_id.is_some() {
                    materialize_issue_media(&mut tx, input, &created).await?;
                } else {
                    if let Some(title) = &input.media_title {
                        let updated: Option<(Uuid,)> = sqlx::query_as(
                            "UPDATE chat_session SET title = $3 WHERE id = $1 AND title = '' \
                             AND EXISTS (SELECT 1 FROM chat_message WHERE id = $2 \
                                 AND chat_session_id = $1 AND role = 'user' \
                                 AND message_kind != $4) \
                             AND NOT EXISTS (SELECT 1 FROM chat_message WHERE chat_session_id = $1 \
                                 AND role = 'user' AND message_kind != $4 AND id != $2) \
                             RETURNING id",
                        )
                        .bind(input.session_id.0)
                        .bind(message_id.0)
                        .bind(title)
                        .bind(CHANNEL_COMMAND_MESSAGE_KIND)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(map_sqlx_err)?;
                        if updated.is_some() {
                            result.initial_title = Some(title.clone());
                            result.title_source = title.clone();
                        }
                    }
                    let linked: Vec<(Uuid,)> = sqlx::query_as(
                        "UPDATE attachment SET chat_message_id = $1 \
                         WHERE id = ANY($2) AND chat_session_id = $3 AND workspace_id = $4 \
                           AND chat_message_id IS NULL RETURNING id",
                    )
                    .bind(message_id.0)
                    .bind(created.iter().map(|id| id.0).collect::<Vec<_>>())
                    .bind(input.session_id.0)
                    .bind(input.workspace_id.0)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
                    result.linked = linked.len();
                    let linked_ids: Vec<Uuid> = linked.into_iter().map(|(id,)| id).collect();
                    let replacements: Vec<(String, String)> = input
                        .media_refs
                        .iter()
                        .zip(created.iter())
                        .filter(|(media, id)| {
                            !media.inline_placeholder.is_empty() && linked_ids.contains(&id.0)
                        })
                        .map(|(media, id)| {
                            (
                                media.inline_placeholder.clone(),
                                inline_attachment_markdown(media, *id),
                            )
                        })
                        .collect();
                    if let Some(body) = compose_inline_media_body(&input.body, &replacements) {
                        sqlx::query(
                            "UPDATE chat_message SET content = $3 \
                             WHERE id = $1 AND chat_session_id = $2 AND role = 'user' \
                               AND channel_ingested",
                        )
                        .bind(message_id.0)
                        .bind(input.session_id.0)
                        .bind(&body)
                        .execute(&mut *tx)
                        .await
                        .map_err(map_sqlx_err)?;
                    }
                }
                result.linked = result.linked.max(created.len());
            }
        }
        if let Some(message_id) = input.message_id {
            sqlx::query(
                "UPDATE chat_message SET channel_media_pending_until = NULL \
                 WHERE id = $1 AND chat_session_id = $2",
            )
            .bind(message_id.0)
            .bind(input.session_id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(result)
    }
}

/// `/issue` 轮次的媒体落法：把附件 markdown 追加 / 替换进 issue 描述（上游同名语句）。
///
/// 描述仍然等于创建时的原文（`issue_description_base`）⇒ 用**带内联布局**的版本整体替换；
/// 否则**追加**（保留用户改过的字节优先于布局保真）。
async fn materialize_issue_media(
    tx: &mut Tx<'_>,
    input: &BindMediaRefsParams,
    created: &[Id],
) -> Result<()> {
    let Some(issue_id) = input.issue_id else {
        return Ok(());
    };
    let markdown = created
        .iter()
        .map(|id| format!("[attachment]({})", attachment_download_path(*id)))
        .collect::<Vec<_>>()
        .join("\n\n");
    sqlx::query(
        "UPDATE issue SET description = CASE \
             WHEN $3::text IS NOT NULL AND COALESCE(description, '') = $3::text THEN $4::text \
             WHEN description IS NULL OR description = '' THEN $5 \
             ELSE description || E'\\n\\n' || $5 END, \
             revision = revision + 1, updated_at = now() \
         WHERE id = $1 AND workspace_id = $2",
    )
    .bind(issue_id.0)
    .bind(input.workspace_id.0)
    .bind(input.issue_description_base.as_deref())
    .bind(append_issue_media_description(input, &markdown))
    .bind(&markdown)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// 追加形态的 issue 描述（去掉指令行之后剩下的正文 + 附件 markdown）。
fn append_issue_media_description(input: &BindMediaRefsParams, markdown: &str) -> String {
    let suffix = if input.issue_command_text.trim().is_empty() {
        input.body.trim().to_string()
    } else {
        String::new()
    };
    if suffix.is_empty() {
        markdown.to_string()
    } else {
        format!("{suffix}\n\n{markdown}")
    }
}

fn attachment_download_path(id: Id) -> String {
    format!("/api/attachments/{}/download", id.0)
}

fn inline_attachment_markdown(media: &MediaRef, id: Id) -> String {
    if media.message_kind == mc_core::channel::message::MessageKind::Image {
        return format!("![]({})", attachment_download_path(id));
    }
    let label = if media.filename.is_empty() {
        "attachment".to_string()
    } else {
        media
            .filename
            .replace('\\', "\\\\")
            .replace('[', "\\[")
            .replace(']', "\\]")
    };
    format!("[{label}]({})", attachment_download_path(id))
}

fn default_media_filename(media: &MediaRef) -> String {
    let prefix = match media.message_kind {
        mc_core::channel::message::MessageKind::Image => "image",
        mc_core::channel::message::MessageKind::Video => "video",
        mc_core::channel::message::MessageKind::Audio => "audio",
        _ => "file",
    };
    let extension = match media.mime_type.as_str() {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "video/mp4" => ".mp4",
        _ => "",
    };
    format!("{prefix}-{}{extension}", Id::new().0)
}

/// 按占位标记替换正文里的内联媒体（返回 `None` = 没有任何替换，别白写一次 UPDATE）。
///
/// 第 n 次出现由 [`MediaRef::inline_index`] 决定 ⇒ 部分媒体失败时不会串位。
fn compose_inline_media_body(body: &str, replacements: &[(String, String)]) -> Option<String> {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for (marker, markdown) in replacements {
        if marker.is_empty() || markdown.is_empty() {
            continue;
        }
        if let Some(start) = body.find(marker.as_str()) {
            edits.push((start, start + marker.len(), markdown.clone()));
        }
    }
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|(start, _, _)| *start);
    let mut out = String::with_capacity(body.len());
    let mut last = 0;
    for (start, end, text) in edits {
        if start < last {
            continue;
        }
        out.push_str(&body[last..start]);
        out.push_str(&text);
        last = end;
    }
    out.push_str(&body[last..]);
    Some(out)
}
