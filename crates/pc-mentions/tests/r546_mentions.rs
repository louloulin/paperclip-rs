//! R546 — pc-mentions 综合测试集。
//!
//! 覆盖 6 个 schemes × build/parse/extract 三层。

#![allow(clippy::doc_markdown)]

use pc_mentions::{
    build_agent_mention_href, build_pipeline_mention_href, build_project_mention_href,
    build_routine_mention_href, build_skill_mention_href, build_user_mention_href,
    extract_agent_mention_ids, extract_pipeline_mentions, extract_project_mention_ids,
    extract_routine_mention_ids, extract_skill_mention_ids, extract_user_mention_ids,
    parse_agent_mention_href, parse_pipeline_mention_href, parse_project_mention_href,
    parse_routine_mention_href, parse_skill_mention_href, parse_user_mention_href,
    AGENT_MENTION_SCHEME, PIPELINE_MENTION_SCHEME, PROJECT_MENTION_SCHEME, ROUTINE_MENTION_SCHEME,
    SKILL_MENTION_SCHEME, USER_MENTION_SCHEME,
};

#[test]
fn r546_project_build_without_color() {
    assert_eq!(
        build_project_mention_href("p1", None),
        format!("{PROJECT_MENTION_SCHEME}p1")
    );
}

#[test]
fn r546_project_build_with_color() {
    assert_eq!(
        build_project_mention_href("p1", Some("#ff00aa")),
        format!("{PROJECT_MENTION_SCHEME}p1?c=ff00aa")
    );
}

#[test]
fn r546_project_build_normalizes_color_forms() {
    assert_eq!(
        build_project_mention_href("p1", Some("#F00")),
        format!("{PROJECT_MENTION_SCHEME}p1?c=ff0000")
    );
    assert_eq!(
        build_project_mention_href("p1", Some("abcdef")),
        format!("{PROJECT_MENTION_SCHEME}p1?c=abcdef")
    );
}

#[test]
fn r546_project_build_invalid_color_dropped() {
    assert_eq!(
        build_project_mention_href("p1", Some("not-a-color")),
        format!("{PROJECT_MENTION_SCHEME}p1")
    );
    assert_eq!(
        build_project_mention_href("p1", Some("")),
        format!("{PROJECT_MENTION_SCHEME}p1")
    );
}

#[test]
fn r546_project_parse_basic() {
    let parsed = parse_project_mention_href("project://p1").unwrap();
    assert_eq!(parsed.project_id, "p1");
    assert_eq!(parsed.color, None);
}

#[test]
fn r546_project_parse_with_color() {
    let parsed = parse_project_mention_href("project://p1?c=ff00aa").unwrap();
    assert_eq!(parsed.project_id, "p1");
    assert_eq!(parsed.color, Some("#ff00aa".into()));
}

#[test]
fn r546_project_parse_with_color_alias() {
    let parsed = parse_project_mention_href("project://p1?color=ff00aa").unwrap();
    assert_eq!(parsed.color, Some("#ff00aa".into()));
}

#[test]
fn r546_project_parse_wrong_scheme_returns_none() {
    assert!(parse_project_mention_href("agent://p1").is_none());
    assert!(parse_project_mention_href("not-a-url").is_none());
}

#[test]
fn r546_project_parse_empty_id_returns_none() {
    assert!(parse_project_mention_href("project://").is_none());
}

#[test]
fn r546_project_round_trip() {
    let href = build_project_mention_href("alpha-1", Some("#abcdef"));
    let parsed = parse_project_mention_href(&href).unwrap();
    assert_eq!(parsed.project_id, "alpha-1");
    assert_eq!(parsed.color, Some("#abcdef".into()));
}

#[test]
fn r546_agent_build_without_icon() {
    assert_eq!(
        build_agent_mention_href("a1", None),
        format!("{AGENT_MENTION_SCHEME}a1")
    );
}

#[test]
fn r546_agent_build_with_icon() {
    assert_eq!(
        build_agent_mention_href("a1", Some("Icon-A")),
        format!("{AGENT_MENTION_SCHEME}a1?i=icon-a")
    );
}

#[test]
fn r546_agent_build_rejects_invalid_icon() {
    assert_eq!(
        build_agent_mention_href("a1", Some("ICON_B")),
        format!("{AGENT_MENTION_SCHEME}a1")
    );
}

#[test]
fn r546_agent_parse_with_icon() {
    let parsed = parse_agent_mention_href("agent://a1?i=robot").unwrap();
    assert_eq!(parsed.agent_id, "a1");
    assert_eq!(parsed.icon, Some("robot".into()));
}

#[test]
fn r546_agent_parse_with_icon_alias() {
    let parsed = parse_agent_mention_href("agent://a1?icon=Robot").unwrap();
    assert_eq!(parsed.icon, Some("robot".into()));
}

#[test]
fn r546_agent_parse_rejects_invalid_icon() {
    let parsed = parse_agent_mention_href("agent://a1?i=BAD_ICON").unwrap();
    assert_eq!(parsed.icon, None);
}

#[test]
fn r546_user_build_parse_round_trip() {
    let href = build_user_mention_href("u-42");
    assert_eq!(href, format!("{USER_MENTION_SCHEME}u-42"));
    let parsed = parse_user_mention_href(&href).unwrap();
    assert_eq!(parsed.user_id, "u-42");
}

#[test]
fn r546_user_parse_rejects_other_scheme() {
    assert!(parse_user_mention_href("agent://u-42").is_none());
}

#[test]
fn r546_skill_build_without_slug() {
    assert_eq!(
        build_skill_mention_href("s1", None),
        format!("{SKILL_MENTION_SCHEME}s1")
    );
}

#[test]
fn r546_skill_build_with_slug() {
    assert_eq!(
        build_skill_mention_href("s1", Some("My-Slug")),
        format!("{SKILL_MENTION_SCHEME}s1?s=my-slug")
    );
}

#[test]
fn r546_skill_build_rejects_leading_dash() {
    assert_eq!(
        build_skill_mention_href("s1", Some("-bad")),
        format!("{SKILL_MENTION_SCHEME}s1")
    );
}

#[test]
fn r546_skill_parse_with_slug() {
    let parsed = parse_skill_mention_href("skill://s1?s=my-slug").unwrap();
    assert_eq!(parsed.skill_id, "s1");
    assert_eq!(parsed.slug, Some("my-slug".into()));
}

#[test]
fn r546_skill_parse_with_slug_alias() {
    let parsed = parse_skill_mention_href("skill://s1?slug=My-Slug").unwrap();
    assert_eq!(parsed.slug, Some("my-slug".into()));
}

#[test]
fn r546_routine_build_parse_round_trip() {
    let href = build_routine_mention_href("r-9");
    assert_eq!(href, format!("{ROUTINE_MENTION_SCHEME}r-9"));
    let parsed = parse_routine_mention_href(&href).unwrap();
    assert_eq!(parsed.routine_id, "r-9");
}

#[test]
fn r546_pipeline_build_without_stage() {
    assert_eq!(
        build_pipeline_mention_href("pl-1", None),
        format!("{PIPELINE_MENTION_SCHEME}pl-1")
    );
}

#[test]
fn r546_pipeline_build_with_stage() {
    assert_eq!(
        build_pipeline_mention_href("pl-1", Some("review")),
        format!("{PIPELINE_MENTION_SCHEME}pl-1?stage=review")
    );
}

#[test]
fn r546_pipeline_build_empty_stage_treated_as_none() {
    assert_eq!(
        build_pipeline_mention_href("pl-1", Some("   ")),
        format!("{PIPELINE_MENTION_SCHEME}pl-1")
    );
}

#[test]
fn r546_pipeline_parse_with_stage() {
    let parsed = parse_pipeline_mention_href("pipeline://pl-1?stage=review").unwrap();
    assert_eq!(parsed.pipeline_id, "pl-1");
    assert_eq!(parsed.stage_key, Some("review".into()));
}

#[test]
fn r546_pipeline_parse_without_stage() {
    let parsed = parse_pipeline_mention_href("pipeline://pl-1").unwrap();
    assert_eq!(parsed.pipeline_id, "pl-1");
    assert_eq!(parsed.stage_key, None);
}

#[test]
fn r546_extract_project_mention_ids_basic() {
    let md = "Hi [P](project://p1) and [P2](project://p2).";
    assert_eq!(extract_project_mention_ids(md), vec!["p1", "p2"]);
}

#[test]
fn r546_extract_project_mention_ids_dedup_preserves_order() {
    let md = "[P](project://p1) and [P2](project://p2) and [P3](project://p1)";
    assert_eq!(extract_project_mention_ids(md), vec!["p1", "p2"]);
}

#[test]
fn r546_extract_project_mention_ids_empty_markdown() {
    assert!(extract_project_mention_ids("").is_empty());
    assert!(extract_project_mention_ids("no mentions here").is_empty());
}

#[test]
fn r546_extract_agent_mention_ids_basic() {
    let md = "Tag [Alice](agent://a1) and [Bob](agent://a2).";
    assert_eq!(extract_agent_mention_ids(md), vec!["a1", "a2"]);
}

#[test]
fn r546_extract_user_mention_ids_basic() {
    let md = "Hey [@alice](user://u1).";
    assert_eq!(extract_user_mention_ids(md), vec!["u1"]);
}

#[test]
fn r546_extract_skill_mention_ids_with_slug() {
    let md = "Use [Rust](skill://rust?s=systems) here.";
    assert_eq!(extract_skill_mention_ids(md), vec!["rust"]);
}

#[test]
fn r546_extract_routine_mention_ids_basic() {
    let md = "Run [Triage](routine://r1).";
    assert_eq!(extract_routine_mention_ids(md), vec!["r1"]);
}

#[test]
fn r546_extract_pipeline_mentions_preserves_stage() {
    let md = "See [Review](pipeline://p1?stage=review) and [Done](pipeline://p1?stage=done).";
    let mentions = extract_pipeline_mentions(md);
    assert_eq!(mentions.len(), 2);
    assert_eq!(mentions[0].pipeline_id, "p1");
    assert_eq!(mentions[0].stage_key.as_deref(), Some("review"));
    assert_eq!(mentions[1].pipeline_id, "p1");
    assert_eq!(mentions[1].stage_key.as_deref(), Some("done"));
}

#[test]
fn r546_extract_pipeline_mentions_dedup_by_stage() {
    let md = "[R](pipeline://p1?stage=review) and [R](pipeline://p1?stage=review)";
    let mentions = extract_pipeline_mentions(md);
    assert_eq!(mentions.len(), 1);
}

#[test]
fn r546_extract_project_mention_ids_ignores_other_schemes() {
    let md = "[R](routine://r1) and [P](project://p1)";
    assert_eq!(extract_project_mention_ids(md), vec!["p1"]);
}
