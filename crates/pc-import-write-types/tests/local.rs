//! 验证 serde camelCase 字段名与 Node 类型定义 1:1 对齐。

use pc_import_write_types::{
    ImportIssueAttachmentRow, ImportIssueCommentRow, ImportIssueDocumentRow, ImportIssueRow,
    ImportIssueWorkProductRow,
};
use serde_json::json;
use uuid::Uuid;

fn sample_id(seed: &str) -> Uuid {
    // 稳定 UUID 便于断言
    let mut h = [0u8; 16];
    let bytes = seed.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        h[i % 16] ^= *b;
    }
    Uuid::from_bytes(h)
}

#[test]
fn import_issue_row_serializes_camel_case() {
    let row = ImportIssueRow {
        id: sample_id("issue-id"),
        ref_: "iss-1".to_string(),
        project_id: Some(sample_id("project")),
        project_workspace_id: None,
        title: "Hello".to_string(),
        description: Some("Body".to_string()),
        assignee_agent_id: Some(sample_id("agent")),
        status: "todo".to_string(),
        priority: "normal".to_string(),
        billing_code: None,
        assignee_adapter_overrides: None,
        execution_workspace_settings: None,
        label_ids: vec![],
        monitor_notes: None,
        monitor_scheduled_by: None,
    };
    let v = serde_json::to_value(&row).unwrap();
    assert!(v.get("id").is_some());
    assert!(v.get("ref").is_some()); // ref_ → ref
    assert!(v.get("projectId").is_some());
    assert!(v.get("projectWorkspaceId").is_some());
    assert!(v.get("assigneeAgentId").is_some());
    assert!(v.get("billingCode").is_some());
    assert!(v.get("assigneeAdapterOverrides").is_some());
    assert!(v.get("executionWorkspaceSettings").is_some());
    assert!(v.get("labelIds").is_some());
    assert!(v.get("monitorNotes").is_some());
    assert!(v.get("monitorScheduledBy").is_some());
    // 不应有 snake_case 字段
    assert!(v.get("project_id").is_none());
    assert!(v.get("monitor_notes").is_none());
}

#[test]
fn import_issue_comment_row_serializes_camel_case() {
    let row = ImportIssueCommentRow {
        id: sample_id("comment"),
        company_id: sample_id("co"),
        issue_id: sample_id("iss"),
        body: "hi".to_string(),
        author_type: "user".to_string(),
        author_agent_id: None,
        author_user_id: Some("u-1".to_string()),
        presentation: Some(json!({"foo": "bar"})),
        metadata: Some(json!({"k": "v"})),
        created_at: None,
    };
    let v = serde_json::to_value(&row).unwrap();
    assert!(v.get("authorType").is_some());
    assert!(v.get("authorAgentId").is_some());
    assert!(v.get("authorUserId").is_some());
    assert_eq!(v.get("body").unwrap().as_str().unwrap(), "hi");
}

#[test]
fn import_attachment_row_serializes_camel_case() {
    let row = ImportIssueAttachmentRow {
        company_id: sample_id("co"),
        issue_id: sample_id("iss"),
        issue_comment_id: Some(sample_id("c")),
        provider: "s3".to_string(),
        object_key: "k".to_string(),
        content_type: "image/png".to_string(),
        byte_size: 1024,
        sha256: "abc".to_string(),
        original_filename: Some("a.png".to_string()),
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    let v = serde_json::to_value(&row).unwrap();
    assert!(v.get("issueCommentId").is_some());
    assert!(v.get("objectKey").is_some());
    assert!(v.get("contentType").is_some());
    assert!(v.get("byteSize").is_some());
    assert!(v.get("sha256").is_some());
    assert!(v.get("originalFilename").is_some());
    assert!(v.get("createdByAgentId").is_some());
    assert!(v.get("createdByUserId").is_some());
}

#[test]
fn import_document_row_serializes_camel_case() {
    let row = ImportIssueDocumentRow {
        company_id: sample_id("co"),
        issue_id: sample_id("iss"),
        key: "body".to_string(),
        title: None,
        format: "markdown".to_string(),
        body: "# hi".to_string(),
        created_by_agent_id: None,
        created_by_user_id: Some("u".to_string()),
        created_by_run_id: None,
        source_trust: None,
    };
    let v = serde_json::to_value(&row).unwrap();
    assert!(v.get("createdByAgentId").is_some());
    assert!(v.get("createdByUserId").is_some());
    assert!(v.get("createdByRunId").is_some());
    assert!(v.get("sourceTrust").is_some());
}

#[test]
fn import_work_product_row_uses_type_not_kind() {
    let row = ImportIssueWorkProductRow {
        company_id: sample_id("co"),
        issue_id: sample_id("iss"),
        project_id: None,
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: Some("123".to_string()),
        title: "PR".to_string(),
        url: Some("https://example.com".to_string()),
        status: "open".to_string(),
        review_state: "pending".to_string(),
        is_primary: true,
        health_status: "ok".to_string(),
        summary: None,
        metadata: None,
        execution_workspace_id: None,
        runtime_service_id: None,
        created_by_run_id: None,
        source_trust: None,
    };
    let v = serde_json::to_value(&row).unwrap();
    // kind → "type" via serde rename
    assert_eq!(v.get("type").unwrap().as_str().unwrap(), "pr");
    assert!(v.get("kind").is_none());
    assert!(v.get("externalId").is_some());
    assert!(v.get("reviewState").is_some());
    assert!(v.get("isPrimary").is_some());
    assert!(v.get("healthStatus").is_some());
    assert!(v.get("executionWorkspaceId").is_some());
    assert!(v.get("runtimeServiceId").is_some());
    assert!(v.get("createdByRunId").is_some());
    assert!(v.get("sourceTrust").is_some());
}

#[test]
fn deserialize_import_issue_row_from_camel_json() {
    let v = json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "ref": "iss-ref",
        "projectId": null,
        "projectWorkspaceId": null,
        "title": "Hello",
        "description": null,
        "assigneeAgentId": null,
        "status": "todo",
        "priority": "normal",
        "billingCode": null,
        "assigneeAdapterOverrides": null,
        "executionWorkspaceSettings": null,
        "labelIds": [],
        "monitorNotes": null,
        "monitorScheduledBy": null
    });
    let row: ImportIssueRow = serde_json::from_value(v).expect("deserialize");
    assert_eq!(row.ref_, "iss-ref");
    assert_eq!(row.title, "Hello");
}
