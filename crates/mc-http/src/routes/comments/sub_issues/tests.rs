use super::*;

fn issue() -> IssueSnapshot {
    IssueSnapshot {
        id: "i1".into(),
        identifier: "LUM-1".into(),
        number: 1,
        title: "t".into(),
        description: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-02T00:00:00Z".into(),
        revision: 7,
        attachments: vec![],
    }
}

fn comment() -> CommentSnapshot {
    CommentSnapshot {
        id: "c1".into(),
        parent_id: None,
        comment_type: "comment".into(),
        content: "hello".into(),
        author: AuthorSnapshot {
            author_type: "user".into(),
            id: "u1".into(),
            name: "Ada".into(),
        },
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-03T00:00:00Z".into(),
        revision: 3,
        attachments: vec![],
        deleted: false,
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        version: 0,
        captured_by_user_id: String::new(),
        captured_at: String::new(),
        source_issue: issue(),
        comment_thread: vec![comment()],
        anchor_comment_id: "c1".into(),
    }
}

/// 摘要排除四类噪声（采集元数据 / issue 的 `updated_at`+`revision` /
/// 评论的 `name`+`updated_at`+`revision`），但**不**排除内容与附件。
#[test]
fn digest_ignores_live_metadata_but_not_content() {
    let base = snapshot();
    let d0 = snapshot_digest(&base).expect("digest");

    // 改名不改摘要。
    let mut renamed = base.clone();
    renamed.comment_thread[0].author.name = "Grace".into();
    assert_eq!(snapshot_digest(&renamed).expect("d"), d0);
    // 编辑后逐字改回 ⇒ 摘要仍相同。
    let mut touched = base.clone();
    touched.comment_thread[0].updated_at = "2027-01-01T00:00:00Z".into();
    touched.comment_thread[0].revision = 99;
    touched.source_issue.updated_at = "2027-01-01T00:00:00Z".into();
    touched.source_issue.revision = 99;
    assert_eq!(snapshot_digest(&touched).expect("d"), d0);
    // 采集元数据不参与。
    let mut captured = base.clone();
    captured.version = 1;
    captured.captured_by_user_id = "u9".into();
    captured.captured_at = "2026-02-02T00:00:00Z".into();
    assert_eq!(snapshot_digest(&captured).expect("d"), d0);

    // 内容变了 ⇒ 摘要变。
    let mut edited = base.clone();
    edited.comment_thread[0].content = "hello!".into();
    assert_ne!(snapshot_digest(&edited).expect("d"), d0);
    // 附件变了 ⇒ 摘要变（附件**不是**噪声）。
    let mut with_att = base.clone();
    with_att.source_issue.attachments.push(AttachmentSnapshot {
        id: "a1".into(),
        owner_type: "issue".into(),
        owner_id: "i1".into(),
        filename: "a.png".into(),
        content_type: "image/png".into(),
        size_bytes: 3,
        created_at: "2026-01-01T00:00:00Z".into(),
    });
    assert_ne!(snapshot_digest(&with_att).expect("d"), d0);
}

/// 摘要不抹掉**将被持久化的**那份快照（上游 `append` 那一段的注释逐字）。
#[test]
fn digest_projection_does_not_mutate_the_input() {
    let base = snapshot();
    let _ = snapshot_digest(&base).expect("digest");
    assert_eq!(base.source_issue.updated_at, "2026-01-02T00:00:00Z");
    assert_eq!(base.comment_thread[0].author.name, "Ada");
    assert_eq!(base.comment_thread[0].revision, 3);
}

/// 墓碑：`deleted = false` 时整个键**缺席**（`Option::None` 同 `omitempty` 逐字）。
#[test]
fn deleted_false_is_omitted() {
    let json = serde_json::to_value(comment()).expect("json");
    assert!(json.get("deleted").is_none());
    let mut tomb = comment();
    tomb.deleted = true;
    let json = serde_json::to_value(&tomb).expect("json");
    assert_eq!(
        json.get("deleted").and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

/// 用量：评论条数、正文字节（整份快照的 JSON 长度）、附件条数与字节（两面合计）。
#[test]
fn limit_usage_sums_both_owners() {
    let mut s = snapshot();
    s.source_issue.attachments.push(AttachmentSnapshot {
        id: "a1".into(),
        owner_type: "issue".into(),
        owner_id: "i1".into(),
        filename: "a".into(),
        content_type: "image/png".into(),
        size_bytes: 10,
        created_at: "2026-01-01T00:00:00Z".into(),
    });
    s.comment_thread[0].attachments.push(AttachmentSnapshot {
        id: "a2".into(),
        owner_type: "comment".into(),
        owner_id: "c1".into(),
        filename: "b".into(),
        content_type: "image/png".into(),
        size_bytes: 20,
        created_at: "2026-01-01T00:00:00Z".into(),
    });
    let usage = limit_usage(&s);
    assert_eq!(usage.comment_count, 1);
    assert_eq!(usage.attachment_count, 2);
    assert_eq!(usage.attachment_bytes, 30);
    assert_eq!(
        usage.text_bytes,
        serde_json::to_vec(&s).expect("json").len()
    );
}

/// 错误面：状态码 / `code` 逐条冻结（上游 `writeSourceContextError`）。
#[test]
fn error_status_and_code_are_verbatim() {
    for (err, status, code) in [
        (
            PreviewError::AnchorCommentDeleted,
            409,
            "anchor_comment_deleted",
        ),
        (
            PreviewError::SourceIssueDeleted,
            409,
            "source_issue_deleted",
        ),
        (
            PreviewError::InvalidPath,
            409,
            "source_context_invalid_path",
        ),
        (
            PreviewError::TooLarge(LimitUsage::default()),
            422,
            "source_context_too_large",
        ),
        (
            PreviewError::CaptureFailed,
            500,
            "source_context_capture_failed",
        ),
    ] {
        assert_eq!(err.status().as_u16(), status, "{code}");
        assert_eq!(err.code(), code);
        assert!(!err.message().is_empty());
    }
}

/// `too_large` 是**唯一**带 `limits` 的分支（上游 `if errors.Is(...TooLarge)`）。
#[test]
fn only_too_large_carries_limits() {
    let resp = PreviewError::TooLarge(LimitUsage {
        comment_count: 257,
        ..LimitUsage::default()
    })
    .into_response();
    assert_eq!(resp.status().as_u16(), 422);
    let resp = PreviewError::InvalidPath.into_response();
    assert_eq!(resp.status().as_u16(), 409);
}

/// token 三段式：`sha256:<64 hex>:<uuid>`（上游 `Token` / `ParseSourceContextToken`）。
#[test]
fn capture_token_shape() {
    let d = "a".repeat(64);
    let t = capture_token(&d, "i1");
    assert_eq!(t, format!("sha256:{d}:i1"));
    assert_eq!(t.split(':').count(), 3);
}

/// 上游的四个上限常量逐字。
#[test]
fn upstream_limits_are_frozen() {
    assert_eq!(MAX_COMMENTS, 256);
    assert_eq!(MAX_TEXT_BYTES, 1_048_576);
    assert_eq!(MAX_ATTACHMENTS, 100);
    assert_eq!(MAX_ATTACHMENT_BYTES, 524_288_000);
}

/// 路由级：**机器凭据** ⇒ 403（上游 `r.With(handler.RequireHumanActor)`）。
///
/// 零库：人类闸在读路径参数与查库**之前**。
#[tokio::test]
async fn route_is_human_only() {
    use std::sync::Arc as StdArc;

    use axum::body::Body as AxumBody;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt as _;

    use crate::routes::ws::test_support::{lazy_db, state_with};
    let state = state_with(
        lazy_db(),
        mc_storage::Storage::new(),
        StdArc::new(mc_ws::hub::Hub::new()),
    );
    let app = crate::apply_default_middleware(router()).with_state(state);
    for source in crate::actor_guard::MACHINE_ACTOR_SOURCES {
        let req = HttpRequest::get(
            "/api/comments/11111111-1111-4111-8111-111111111111/sub-issue-preview",
        )
        .header("x-actor-source", source)
        .header("x-multica-user-id", "22222222-2222-4222-8222-222222222222")
        .body(AxumBody::empty())
        .expect("req");
        let resp = app.clone().oneshot(req).await.expect("response");
        assert_eq!(
            resp.status().as_u16(),
            403,
            "actor source {source} 必须是 403"
        );
    }
}
