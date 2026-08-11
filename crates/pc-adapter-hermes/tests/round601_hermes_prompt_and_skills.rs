// R601 Hermes prompt_template + wake_prompt + skills 真实 e2e 验证
//
// 1. prompt_template.render_template + render_conditional_sections 真实组合
// 2. wake_prompt.render_wake_prompt + select_task_markdown 真实组合
// 3. skills.build_skill_snapshot 真实读取 ~/.hermes/skills + paperclip
//    runtime skills 目录（用真实 fs fixture，无 mock）

use pc_adapter_hermes::prompt_template::{
    join_prompt_sections, render_conditional_sections, render_template,
};
use pc_adapter_hermes::skills::{build_skill_snapshot, scan_hermes_skills};
use pc_adapter_hermes::wake_prompt::{
    render_wake_prompt, select_task_markdown, stringify_wake_payload,
};
use serde_json::json;
use std::path::PathBuf;

#[test]
fn prompt_template_full_render_path() {
    let template = "\
You are {{agent.name}} (id: {{agent.id}}).

{{#noTask}}No task assigned. Stand by.{{/noTask}}
{{#taskTitle}}Task: {{taskTitle}}
Body: {{taskBody}}{{/taskTitle}}

Available tools: {{tools}}
";
    let vars = json!({
        "agent": {"name": "Hermes-1", "id": "agent-uuid-1234"},
        "taskTitle": "Fix login bug",
        "taskBody": "Users can't login with OAuth",
        "tools": "terminal,file,web",
        "taskId": "T-123",
        "noTask": null
    });
    let rendered = render_conditional_sections(template, &vars);
    let final_prompt = render_template(&rendered, &vars);

    assert!(final_prompt.contains("Hermes-1"));
    assert!(final_prompt.contains("agent-uuid-1234"));
    assert!(final_prompt.contains("Fix login bug"));
    assert!(final_prompt.contains("Users can't login")); // body rendered
    assert!(final_prompt.contains("terminal,file,web"));
    // taskId is set → noTask section must be removed
    assert!(!final_prompt.contains("No task assigned"));
}

#[test]
fn prompt_template_join_with_real_sections() {
    let wake = "## wake context\n- reason: issue_assigned";
    let task = "## task\nFix the bug";
    let handoff = "";
    let template = "## base prompt";

    let rendered = render_template(template, &json!({}));
    let joined = join_prompt_sections(
        &[Some(wake), Some(task), Some(handoff), Some(&rendered)],
        "\n\n",
    );

    // Blank handoff section must be filtered out
    assert!(!joined.contains("\n\n\n"));
    assert!(joined.contains("wake context"));
    assert!(joined.contains("Fix the bug"));
    assert!(joined.contains("base prompt"));
}

#[test]
fn wake_prompt_full_render_with_recovery_contract() {
    let wake = json!({
        "reason": "issue_recovery_action_restored",
        "issue": {
            "id": "T-42",
            "identifier": "ENG-42",
            "title": "Recover from process_lost"
        },
        "recovery": {
            "cause": "process_lost",
            "failureSummary": "killed by SIGKILL after 30min"
        },
        "commentId": "C-7"
    });

    let prompt = render_wake_prompt(Some(&wake), false);
    assert!(prompt.contains("Recovery contract"));
    assert!(prompt.contains("process_lost"));
    assert!(prompt.contains("ENG-42"));
    assert!(prompt.contains("Recover from process_lost"));
    assert!(prompt.contains("C-7"));
}

#[test]
fn wake_prompt_assignment_full_vs_resume_compact() {
    let context_assignment = json!({
        "paperclipTaskMarkdown": "FULL_BRIEF",
        "paperclipTaskMarkdownCompact": "COMPACT_BRIEF",
        "paperclipWake": {"reason": "issue_assigned"}
    });
    let context_resume = json!({
        "paperclipTaskMarkdown": "FULL_BRIEF",
        "paperclipTaskMarkdownCompact": "COMPACT_BRIEF",
        "paperclipWake": {"reason": "comment_added"}
    });

    // resumed + assignment → FULL (issue already familiar to session)
    assert_eq!(
        select_task_markdown(Some(&context_assignment), true),
        "FULL_BRIEF"
    );
    // resumed + non-assignment → COMPACT (avoid double-render)
    assert_eq!(
        select_task_markdown(Some(&context_resume), true),
        "COMPACT_BRIEF"
    );
    // fresh → FULL
    assert_eq!(
        select_task_markdown(Some(&context_assignment), false),
        "FULL_BRIEF"
    );
}

#[test]
fn wake_payload_json_serializes_for_env() {
    let wake = json!({
        "reason": "issue_assigned",
        "issue": {"id": "T-1", "title": "Do thing"},
        "commentId": "C-1"
    });
    let serialized = stringify_wake_payload(Some(&wake)).expect("serialize");
    let reparsed: serde_json::Value = serde_json::from_str(&serialized).expect("parse back");
    assert_eq!(reparsed, wake);
}

#[tokio::test]
async fn skills_real_fs_with_runtime_and_hermes() {
    // Real fixture: create two temp dirs
    let hermes_home =
        std::env::temp_dir().join(format!("paperclip-hermes-home-{}", uuid::Uuid::new_v4()));
    let runtime =
        std::env::temp_dir().join(format!("paperclip-runtime-skills-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&hermes_home).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();

    // ~/.hermes/skills/terminal/SKILL.md (user installed)
    let hermes_skills = hermes_home.join(".hermes").join("skills").join("terminal");
    std::fs::create_dir_all(&hermes_skills).unwrap();
    std::fs::write(
        hermes_skills.join("SKILL.md"),
        "---\ndescription: terminal sandbox tools\n---\nbody",
    )
    .unwrap();

    // runtime/skills/code-review/SKILL.md (paperclip managed)
    let review = runtime.join("code-review");
    std::fs::create_dir_all(&review).unwrap();
    std::fs::write(
        review.join("SKILL.md"),
        "---\ndescription: code review guidelines\n---",
    )
    .unwrap();

    let config = json!({
        "env": {"HOME": hermes_home.to_string_lossy()},
        "desiredSkills": ["code-review", "nonexistent"]
    });
    let snapshot = build_skill_snapshot(&config, Some(&runtime)).await;

    assert!(snapshot.supported);
    assert_eq!(snapshot.mode, "persistent");
    assert_eq!(snapshot.desired_skills, vec!["code-review", "nonexistent"]);

    // code-review appears once, marked managed+configured
    let review_entries: Vec<_> = snapshot
        .entries
        .iter()
        .filter(|e| e.key == "code-review")
        .collect();
    assert_eq!(review_entries.len(), 1);
    assert!(review_entries[0].managed);
    assert_eq!(
        review_entries[0].state,
        pc_adapter_hermes::skills::SkillState::Configured
    );

    // terminal appears once, marked user-installed + read-only
    let terminal_entries: Vec<_> = snapshot
        .entries
        .iter()
        .filter(|e| e.key == "terminal")
        .collect();
    assert_eq!(terminal_entries.len(), 1);
    assert!(!terminal_entries[0].managed);
    assert!(terminal_entries[0].read_only);

    // nonexistent is marked Missing with warning
    let nonexistent = snapshot
        .entries
        .iter()
        .find(|e| e.key == "nonexistent")
        .expect("missing entry");
    assert_eq!(
        nonexistent.state,
        pc_adapter_hermes::skills::SkillState::Missing
    );
    assert!(snapshot.warnings.iter().any(|w| w.contains("nonexistent")));

    // scan_hermes_skills directly also returns terminal
    let hermes_only = scan_hermes_skills(&hermes_home.join(".hermes").join("skills")).await;
    assert_eq!(hermes_only.len(), 1);
    assert_eq!(hermes_only[0].key, "terminal");

    std::fs::remove_dir_all(&hermes_home).ok();
    std::fs::remove_dir_all(&runtime).ok();
}

#[tokio::test]
async fn skills_desired_filter_marks_managed_as_configured() {
    let hermes_home =
        std::env::temp_dir().join(format!("paperclip-hermes-home-{}", uuid::Uuid::new_v4()));
    let runtime =
        std::env::temp_dir().join(format!("paperclip-runtime-skills-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&hermes_home).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();

    // runtime has skills: a (desired), b (not desired)
    for name in &["a", "b"] {
        let dir = runtime.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\ndescription: {name}\n---"),
        )
        .unwrap();
    }

    let config = json!({
        "env": {"HOME": hermes_home.to_string_lossy()},
        "desiredSkills": ["a"]
    });
    let snapshot = build_skill_snapshot(&config, Some(&runtime)).await;

    let a = snapshot.entries.iter().find(|e| e.key == "a").expect("a");
    assert_eq!(a.state, pc_adapter_hermes::skills::SkillState::Configured);
    let b = snapshot.entries.iter().find(|e| e.key == "b").expect("b");
    assert_eq!(b.state, pc_adapter_hermes::skills::SkillState::Available);

    std::fs::remove_dir_all(&hermes_home).ok();
    std::fs::remove_dir_all(&runtime).ok();
}

#[allow(dead_code)]
fn _ensure_path_buf_used(_: PathBuf) {}
