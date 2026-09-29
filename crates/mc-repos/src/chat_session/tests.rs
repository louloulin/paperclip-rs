//! `chat_session` 仓储的单测（列清单同源性的钉子）。

use super::types::{prefixed_session_columns, SESSION_COLUMNS};

#[test]
fn prefixed_columns_cover_every_session_column() {
    let cols = prefixed_session_columns("cs");
    let parts: Vec<&str> = cols.split(", ").collect();
    assert_eq!(parts.len(), 17);
    assert_eq!(parts[0], "cs.id");
    assert_eq!(parts[16], "cs.explicitly_created_at");
    // 与 `RETURNING *` 的列序一致（见 generated/chat.sql.go 的 Scan 顺序）。
    assert_eq!(
        parts,
        vec![
            "cs.id",
            "cs.workspace_id",
            "cs.agent_id",
            "cs.creator_id",
            "cs.title",
            "cs.session_id",
            "cs.work_dir",
            "cs.status",
            "cs.created_at",
            "cs.updated_at",
            "cs.unread_since",
            "cs.runtime_id",
            "cs.last_read_at",
            "cs.is_agent_intro",
            "cs.pinned_at",
            "cs.project_id",
            "cs.explicitly_created_at",
        ]
    );
}

#[test]
fn session_columns_is_the_same_17_columns() {
    // `\` 续行会连同换行与行首空白一起去掉 ⇒ 直接按 `, ` 切。
    let parts: Vec<&str> = SESSION_COLUMNS.split(", ").collect();
    assert_eq!(parts.len(), 17);
    assert_eq!(parts[0], "id");
    assert_eq!(parts[16], "explicitly_created_at");
}
