#![forbid(unsafe_code)]
//! Pipeline case outputs —— 与 Node \\u{60}server/src/services/pipeline-case-outputs.ts\\u{60} 1:1。
//!
//! 当前 R639 子集：
//! - 类型契约：\\u{60}PipelineCaseOutputItem\\u{60} / \\u{60}PipelineCaseOutputsResponse\\u{60} / \\u{60}PipelineCaseOutputContextSummary\\u{60} / ...
//! - 纯函数：\\u{60}summarize_pipeline_case_outputs_for_context\\u{60} /
//!   \\u{60}format_pipeline_case_output_context_markdown\\u{60} / \\u{60}sort_outputs\\u{60} /
//!   \\u{60}context_fetch_hint\\u{60} / \\u{60}truncate_context_excerpt\\u{60} / \\u{60}preview_for\\u{60} / ...
//!
//! 后续轮次可扩展：\\u{60}pipeline_case_outputs_service\\u{60} 多表 JOIN（见 Node 266-...）。

pub mod pure;
pub mod service;
pub mod types;

pub use pure::{
    content_path, context_fetch_hint, deliverable_document_rank, download_path,
    format_pipeline_case_output_context_markdown, normalize_preview_text, output_sort_group,
    preview_for, sanitize_output_context_summary, sort_outputs_in_place,
    source_document_path, source_issue_path, summarize_pipeline_case_outputs_for_context,
    truncate_context_excerpt, OutputSortGroup, TruncatedExcerpt, CONTEXT_OUTPUT_EXCERPT_MAX_LENGTH,
    CONTEXT_OUTPUT_EXCERPT_TOTAL_MAX_LENGTH, CONTEXT_OUTPUT_ITEM_LIMIT, DELIVERABLE_TITLE_PATTERNS,
    PREVIEW_TEXT_MAX_LENGTH,
};
pub use types::{
    PipelineCaseOutputContextSourceIssue, PipelineCaseOutputContextSummary,
    PipelineCaseOutputContextSummaryItem, PipelineCaseOutputItem, PipelineCaseOutputItemKind,
    PipelineCaseOutputsResponse, SourceTrustMetadata,
};

pub use service::{
    get_case_pipeline_id, get_company_issue_prefix, list_case_outputs, list_documents_for_issues,
    list_sources, CaseOutputDocumentRow, CaseOutputSourceRow,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PipelineCaseOutputContextSourceIssue, PipelineCaseOutputContextSummary, PipelineCaseOutputContextSummaryItem, PipelineCaseOutputItem, PipelineCaseOutputItemKind, PipelineCaseOutputsResponse};

    fn document_item(title: &str, updated_at: &str) -> PipelineCaseOutputItem {
        PipelineCaseOutputItem {
            id: format!("document:{title}"),
            kind: PipelineCaseOutputItemKind::Document,
            title: title.to_string(),
            source_issue_id: "issue-1".into(),
            source_issue_identifier: Some("PC-1".into()),
            source_issue_path: "/PC/PC-1".into(),
            source_issue_title: "Source".into(),
            source_issue_status: "done".into(),
            source_role: "work".into(),
            source_trust: None,
            source_run_id: None,
            source_agent_id: None,
            preview: Some("body preview content".into()),
            created_at: "2026-08-12T00:00:00Z".into(),
            updated_at: updated_at.into(),
            document_id: Some(format!("doc-{title}")),
            document_key: Some(format!("{title}-key")),
            document_title: Some(title.to_string()),
            format: Some("markdown".into()),
            latest_revision_id: Some(format!("rev-{title}")),
            latest_revision_number: Some(3),
            document_path: Some(format!("/PC/PC-1/documents/{title}-key")),
            work_product_id: None,
            r#type: None,
            provider: None,
            external_id: None,
            url: None,
            status: None,
            review_state: None,
            attachment_id: None,
            filename: None,
            content_type: None,
            content_path: None,
            download_path: None,
            body: None,
        }
    }

    #[test]
    fn r639_content_and_download_paths_match_node_shape() {
        assert_eq!(content_path("att-1"), "/api/attachments/att-1/content");
        assert_eq!(download_path("att-1"), "/api/attachments/att-1/content?download=1");
    }

    #[test]
    fn r639_normalize_preview_text_collapses_whitespace_and_truncates() {
        let input = "\\u{60}\\u{60}\\u{60}code\\u{60}\\u{60}\\u{60}  multiple   spaces\n\nand lines";
        let result = normalize_preview_text(Some(input)).expect("non-empty");
        assert_eq!(result, "code multiple spaces and lines");
        let long = "x".repeat(PREVIEW_TEXT_MAX_LENGTH + 100);
        let result = normalize_preview_text(Some(&long)).expect("non-empty");
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), PREVIEW_TEXT_MAX_LENGTH + 3);
    }

    #[test]
    fn r639_truncate_context_excerpt_marks_truncation() {
        let short = "hello";
        let r = truncate_context_excerpt(Some(short), 10);
        assert_eq!(r.excerpt.as_deref(), Some("hello"));
        assert!(!r.excerpt_truncated);
        let long = "x".repeat(20);
        let r = truncate_context_excerpt(Some(&long), 5);
        assert_eq!(r.excerpt.as_deref().unwrap().chars().count(), 5);
        assert!(r.excerpt_truncated);
    }

    #[test]
    fn r639_deliverable_rank_matches_title_patterns() {
        let brief = document_item("Project Brief", "2026-08-12T00:00:00Z");
        assert_eq!(deliverable_document_rank(&brief), 0);
        let other = document_item("Random Notes", "2026-08-12T00:00:00Z");
        assert_eq!(deliverable_document_rank(&other), 1);
    }

    #[test]
    fn r639_sort_outputs_groups_deliverables_first_then_updated_desc() {
        let mut items = vec![
            document_item("Notes", "2026-08-10T00:00:00Z"),
            document_item("Project Brief", "2026-08-09T00:00:00Z"),
            document_item("Spec", "2026-08-12T00:00:00Z"),
        ];
        sort_outputs_in_place(&mut items);
        // Deliverable ("Project Brief", "Spec") first by updated_at desc, then non-deliverable
        assert_eq!(items[0].title, "Spec");
        assert_eq!(items[1].title, "Project Brief");
        assert_eq!(items[2].title, "Notes");
    }

    #[test]
    fn r639_summarize_bounded_to_5_items_and_caps_excerpt_total() {
        let items: Vec<PipelineCaseOutputItem> = (0..10)
            .map(|i| {
                let mut item = document_item(&format!("Item-{i}"), "2026-08-12T00:00:00Z");
                item.preview = Some("x".repeat(CONTEXT_OUTPUT_EXCERPT_MAX_LENGTH));
                item
            })
            .collect();
        let outputs = PipelineCaseOutputsResponse {
            company_id: None,
            case_id: None,
            generated_at: "2026-08-12T00:00:00Z".into(),
            items,
        };
        let summary = summarize_pipeline_case_outputs_for_context(&outputs, None);
        assert_eq!(summary.item_count, CONTEXT_OUTPUT_ITEM_LIMIT);
        assert_eq!(summary.total_item_count, 10);
        assert_eq!(summary.omitted_item_count, 5);
        assert_eq!(summary.excerpt_max_chars, CONTEXT_OUTPUT_EXCERPT_MAX_LENGTH);
        let total_chars: usize = summary
            .items
            .iter()
            .filter_map(|i| i.excerpt.as_ref().map(|s| s.chars().count()))
            .sum();
        assert!(total_chars <= CONTEXT_OUTPUT_EXCERPT_TOTAL_MAX_LENGTH);
    }

    #[test]
    fn r639_format_markdown_handles_empty_summary() {
        let empty_outputs = PipelineCaseOutputsResponse {
            company_id: None,
            case_id: None,
            generated_at: "2026-08-12T00:00:00Z".into(),
            items: vec![],
        };
        let summary = summarize_pipeline_case_outputs_for_context(&empty_outputs, None);
        let md = format_pipeline_case_output_context_markdown(Some(&summary))
            .expect("markdown for empty");
        assert!(md.contains("No linked task outputs are available yet."));
    }

    #[test]
    fn r639_format_markdown_emits_json_block_for_non_empty() {
        let mut item = document_item("Project Brief", "2026-08-12T00:00:00Z");
        item.preview = Some("Brief excerpt".into());
        let outputs = PipelineCaseOutputsResponse {
            company_id: None,
            case_id: None,
            generated_at: "2026-08-12T00:00:00Z".into(),
            items: vec![item],
        };
        let summary = summarize_pipeline_case_outputs_for_context(&outputs, Some(3));
        let md = format_pipeline_case_output_context_markdown(Some(&summary))
            .expect("markdown for non-empty");
        assert!(md.contains("## Pipeline Item Outputs"));
        assert!(md.contains("\\u{60}\\u{60}\\u{60}json"));
        assert!(md.contains("\\u{60}\\u{60}\\u{60}"));
        assert!(md.contains("Project Brief"));
    }

    #[test]
    fn r639_format_markdown_returns_none_for_null_input() {
        assert!(format_pipeline_case_output_context_markdown(None).is_none());
    }

    #[test]
    fn r639_context_fetch_hint_per_kind() {
        let mut doc = document_item("Brief", "2026-08-12T00:00:00Z");
        doc.document_path = Some("/PC/PC-1/documents/brief".into());
        let hint_doc = context_fetch_hint(&doc);
        assert!(hint_doc.contains("/PC/PC-1/documents/brief"));
        let mut wp = document_item("Work", "2026-08-12T00:00:00Z");
        wp.kind = PipelineCaseOutputItemKind::WorkProduct;
        wp.url = Some("https://example.com/work".into());
        let hint_wp = context_fetch_hint(&wp);
        assert!(hint_wp.contains("https://example.com/work"));
        let mut att = document_item("Attachment", "2026-08-12T00:00:00Z");
        att.kind = PipelineCaseOutputItemKind::Attachment;
        att.content_path = Some("/api/attachments/a/content".into());
        att.download_path = Some("/api/attachments/a/content?download=1".into());
        let hint_att = context_fetch_hint(&att);
        assert!(hint_att.contains("/api/attachments/a/content"));
        assert!(hint_att.contains("download=1"));
    }

    // ----- R773 edge cases for pure helpers -----

    #[test]
    fn r773_normalize_preview_text_handles_empty_string() {
        assert_eq!(normalize_preview_text(Some("")), None);
        assert_eq!(normalize_preview_text(None), None);
        assert_eq!(normalize_preview_text(Some("   \n\t  ")), None);
    }

    #[test]
    fn r773_normalize_preview_text_keeps_short_text_intact() {
        let out = normalize_preview_text(Some("hello world")).unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn r773_truncate_context_excerpt_returns_truncated_flag() {
        let r = truncate_context_excerpt(Some("hello world"), 5);
        assert_eq!(r.excerpt.as_deref(), Some("hello"));
        assert!(r.excerpt_truncated);
    }

    #[test]
    fn r773_truncate_context_excerpt_returns_input_when_short() {
        let r = truncate_context_excerpt(Some("hi"), 10);
        assert_eq!(r.excerpt.as_deref(), Some("hi"));
        assert!(!r.excerpt_truncated);
    }

    #[test]
    fn r773_deliverable_rank_returns_99_for_non_document() {
        let mut item = document_item("Brief", "2026-08-12T00:00:00Z");
        item.kind = PipelineCaseOutputItemKind::WorkProduct;
        assert_eq!(deliverable_document_rank(&item), 99);
        item.kind = PipelineCaseOutputItemKind::Attachment;
        assert_eq!(deliverable_document_rank(&item), 99);
    }

    #[test]
    fn r773_output_sort_group_classifies_each_kind() {
        let mut item = document_item("Brief", "2026-08-12T00:00:00Z");
        assert_eq!(output_sort_group(&item), OutputSortGroup::Deliverable);
        item.title = "Random Note".into();
        assert_eq!(output_sort_group(&item), OutputSortGroup::Document);
        item.kind = PipelineCaseOutputItemKind::WorkProduct;
        assert_eq!(output_sort_group(&item), OutputSortGroup::WorkProduct);
        item.kind = PipelineCaseOutputItemKind::Attachment;
        assert_eq!(output_sort_group(&item), OutputSortGroup::Attachment);
    }

    #[test]
    fn r773_source_issue_path_falls_back_to_id_when_identifier_empty() {
        assert_eq!(source_issue_path("acme", Some("PC-1"), "issue-7"), "/acme/PC-1");
        assert_eq!(source_issue_path("acme", None, "issue-7"), "/issues/issue-7");
        assert_eq!(source_issue_path("acme", Some(""), "issue-7"), "/issues/issue-7");
    }

    #[test]
    fn r773_source_document_path_concatenates_document_key() {
        let p = source_document_path("acme", Some("PC-1"), "issue-7", "brief");
        assert_eq!(p, "/acme/PC-1/documents/brief");
    }

    #[test]
    fn r773_context_fetch_hint_attachment_includes_untrusted_warning() {
        let mut att = document_item("Attachment", "2026-08-12T00:00:00Z");
        att.kind = PipelineCaseOutputItemKind::Attachment;
        att.content_path = Some("/api/attachments/a/content".into());
        att.download_path = Some("/api/attachments/a/content?download=1".into());
        let hint = context_fetch_hint(&att);
        assert!(hint.contains("untrusted"));
    }

    #[test]
    fn r773_sanitize_output_context_summary_caps_total_length() {
        let item = |title: &str| PipelineCaseOutputContextSummaryItem {
            id: format!("doc:{title}"),
            kind: PipelineCaseOutputItemKind::Document,
            title: title.into(),
            key: Some(title.into()),
            revision_id: None,
            revision_number: None,
            source_issue: PipelineCaseOutputContextSourceIssue {
                id: "issue-1".into(),
                identifier: Some("PC-1".into()),
                title: "Source".into(),
                status: "done".into(),
                path: "/PC/PC-1".into(),
                role: "work".into(),
            },
            source_run_id: None,
            source_agent_id: None,
            source_trust: None,
            excerpt: Some("x".repeat(400)),
            excerpt_truncated: false,
            fetch_hint: Some("GET /x".into()),
        };
        let summary = PipelineCaseOutputContextSummary {
            generated_at: "2026-08-12T00:00:00Z".into(),
            item_count: 5,
            total_item_count: 5,
            omitted_item_count: 0,
            excerpt_max_chars: 400,
            redaction_note: String::new(),
            items: vec![item("a"), item("b"), item("c"), item("d"), item("e")],
        };
        let s = sanitize_output_context_summary(summary);
        let total_chars: usize = s.items.iter()
            .filter_map(|i| i.excerpt.as_ref())
            .map(|e| e.chars().count())
            .sum();
        assert!(total_chars <= CONTEXT_OUTPUT_EXCERPT_TOTAL_MAX_LENGTH);
    }

    #[test]
    fn r773_summarize_pipeline_case_outputs_handles_empty_response() {
        let response = PipelineCaseOutputsResponse {
            company_id: None,
            case_id: None,
            generated_at: "2026-08-12T00:00:00Z".into(),
            items: vec![],
        };
        let summary = summarize_pipeline_case_outputs_for_context(&response, Some(5));
        assert_eq!(summary.item_count, 0);
        assert_eq!(summary.total_item_count, 0);
        assert_eq!(summary.omitted_item_count, 0);
        assert!(summary.items.is_empty());
    }
}
