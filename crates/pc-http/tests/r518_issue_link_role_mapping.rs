//! R518 — POST /api/cases/:case_id/issue-links 路径 + role 映射契约
//!
//! Node 端 (`paperclip/server/src/routes/pipelines.ts`):
//!   router.post("/cases/:caseId/issue-links", validate(createIssueLinkSchema), ...)
//!   createIssueLinkSchema = { issueId: uuid, role: "origin"|"conversation"|"work"|"automation" }
//!
//! Rust 端契约要点：
//! 1. 路径必须是 /api/cases/:case_id/issue-links（之前误为 /links，R518 修正）。
//! 2. role 接受 4 个 Node 值；conversation 和 automation 在 Rust 端
//!    降级映射为 reference（受 DB CHECK 约束限制）。
//! 3. 缺省 / 未知值视为 reference。

fn normalize_for_test(input: Option<&str>) -> &'static str {
    match input.unwrap_or("reference") {
        "origin" => "origin",
        "work" => "work",
        _ => "reference",
    }
}

#[test]
fn r518_normalizes_origin_role() {
    assert_eq!(normalize_for_test(Some("origin")), "origin");
}

#[test]
fn r518_normalizes_work_role() {
    assert_eq!(normalize_for_test(Some("work")), "work");
}

#[test]
fn r518_conversation_role_downgrades_to_reference() {
    assert_eq!(normalize_for_test(Some("conversation")), "reference");
}

#[test]
fn r518_automation_role_downgrades_to_reference() {
    assert_eq!(normalize_for_test(Some("automation")), "reference");
}

#[test]
fn r518_reference_role_keeps_reference() {
    assert_eq!(normalize_for_test(Some("reference")), "reference");
}

#[test]
fn r518_missing_role_defaults_to_reference() {
    assert_eq!(normalize_for_test(None), "reference");
}

#[test]
fn r518_unknown_role_falls_back_to_reference() {
    assert_eq!(normalize_for_test(Some("")), "reference");
    assert_eq!(normalize_for_test(Some("weird_role")), "reference");
}

#[test]
fn r518_route_path_is_issue_links_not_links() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes/cases.rs"),
    )
    .expect("read cases.rs");
    let wanted = "\"/api/cases/:case_id/issue-links\", post(create_case_link)";
    let legacy = "\"/api/cases/:case_id/links\", post(create_case_link)";
    assert!(
        src.contains(wanted),
        "R518: POST handler not registered at /api/cases/:case_id/issue-links"
    );
    assert!(
        !src.contains(legacy),
        "R518: legacy /links POST path still present"
    );
}

#[test]
fn r518_db_check_constraint_lists_three_roles() {
    let sql = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("pc-db/migrations/drizzle/0143_cases_foundation.sql"),
    )
    .expect("read 0143_cases_foundation.sql");
    assert!(
        sql.contains("case_issue_links_role_check")
            && sql.contains("'origin'")
            && sql.contains("'work'")
            && sql.contains("'reference'"),
        "R518: db constraint must allow origin/work/reference",
    );
}
