//! R846 — pc-feature-catalog skills + teams integration tests.
//!
//! Black-box tests exercising the public re-exports from `pc_feature_catalog::*`
//! to verify that `skills` and `teams` modules expose their 1:1 Node API surface
//! in the expected shape.

#![allow(clippy::doc_markdown)]

use pc_feature_catalog::skills::{
    is_markdown_path as skills_is_markdown, list_catalog_skills,
    parse_manifest as parse_skills_manifest, resolve_catalog_skill_reference,
    CatalogManifestFile as SkillsManifest, CatalogSkill as SkillsSkill,
    CatalogSkillFileEntry, CatalogSkillListQuery, CatalogSkillSource,
};
use pc_feature_catalog::teams::{
    collect_catalog_team_skill_preparations, is_markdown_path as teams_is_markdown,
    is_pinned_source_ref, list_catalog_teams, parse_yaml_document,
    read_catalog_team_provenance, render_catalog_provenance_yaml,
    render_synthetic_company_markdown, render_yaml_block, render_yaml_file,
    yaml_scalar, CatalogManifest as TeamsManifest, CatalogTeam as TeamsTeam,
    CatalogTeamFileEntry as TeamsFileEntry, CatalogTeamListQuery,
    CatalogTeamSkillRequirement, CatalogTeamSourcePolicy, TargetManagerReference,
    CATALOG_TEAM_FILE_KIND_TASK, SKILL_PREP_ALREADY_IN_PACKAGE,
    SKILL_PREP_BLOCKED, SKILL_PREP_CATALOG_INSTALL_REQUIRED,
    SKILL_REQ_TYPE_CATALOG, SKILL_REQ_TYPE_GITHUB, SKILL_REQ_TYPE_LOCAL,
};
use serde_json::json;

fn build_skills_manifest() -> SkillsManifest {
    SkillsManifest {
        package_name: "@paperclipai/skills-catalog".into(),
        package_version: "9.9.9".into(),
        skills: vec![
            SkillsSkill {
                id: "s-1".into(),
                key: "a/b".into(),
                slug: "a-b".into(),
                name: "Alpha".into(),
                description: "alpha skill".into(),
                category: "general".into(),
                kind: "agent".into(),
                recommended_for_roles: vec!["engineer".into()],
                tags: vec!["writing".into()],
                path: "skills/alpha".into(),
                files: vec![CatalogSkillFileEntry {
                    path: "SKILL.md".into(),
                    sha256: "abc".into(),
                    kind: "doc".into(),
                }],
                source: Some(CatalogSkillSource {
                    kind: "github".into(),
                    hostname: None,
                    owner: Some("o".into()),
                    repo: Some("r".into()),
                    commit: Some("c".into()),
                    path: Some("p".into()),
                }),
                package_name: None,
                package_version: None,
            },
            SkillsSkill {
                id: "s-2".into(),
                key: "c/d".into(),
                slug: "c-d".into(),
                name: "Charlie".into(),
                description: "charlie skill".into(),
                category: "general".into(),
                kind: "team".into(),
                recommended_for_roles: vec![],
                tags: vec![],
                path: "skills/charlie".into(),
                files: vec![],
                source: None,
                package_name: None,
                package_version: None,
            },
        ],
    }
}

fn build_teams_manifest() -> TeamsManifest {
    TeamsManifest {
        package_name: "@paperclipai/teams-catalog".into(),
        package_version: "2.0.0".into(),
        teams: vec![TeamsTeam {
            id: "team-A".into(),
            key: Some("team/key".into()),
            slug: "team-a".into(),
            name: "Team A".into(),
            description: "team-a description".into(),
            category: "engineering".into(),
            kind: "bundled".into(),
            recommended_for_company_types: vec!["startup".into()],
            tags: vec![],
            path: "teams/a".into(),
            entrypoint: "TEAM.md".into(),
            files: vec![TeamsFileEntry {
                path: "TEAM.md".into(),
                kind: CATALOG_TEAM_FILE_KIND_TASK.into(),
            }],
            agent_slugs: vec!["alpha".into()],
            root_agent_slugs: vec!["alpha".into()],
            project_slugs: vec!["p1".into()],
            required_skills: vec![
                CatalogTeamSkillRequirement {
                    kind: SKILL_REQ_TYPE_CATALOG.into(),
                    ref_: "alias".into(),
                    agent_slugs: vec!["alpha".into()],
                    resolved: true,
                    catalog_skill_id: Some("cat-1".into()),
                    catalog_skill_key: Some("ns/cool".into()),
                    source_locator: None,
                    source_ref: None,
                },
                CatalogTeamSkillRequirement {
                    kind: SKILL_REQ_TYPE_LOCAL.into(),
                    ref_: "local-ref".into(),
                    agent_slugs: vec!["alpha".into()],
                    resolved: true,
                    catalog_skill_id: None,
                    catalog_skill_key: None,
                    source_locator: None,
                    source_ref: None,
                },
                CatalogTeamSkillRequirement {
                    kind: SKILL_REQ_TYPE_GITHUB.into(),
                    ref_: "owner/repo".into(),
                    agent_slugs: vec!["alpha".into()],
                    resolved: true,
                    catalog_skill_id: None,
                    catalog_skill_key: None,
                    source_locator: None,
                    source_ref: Some("not-hex".to_string()),
                },
            ],
            compatibility: "compatible".into(),
            trust_level: "default".into(),
            content_hash: "content-hash".into(),
            package_name: None,
            package_version: None,
        }],
    }
}

#[test]
fn r846_public_re_exports_resolve() {
    // Make sure both modules are reachable via the crate root.
    let _ = pc_feature_catalog::skills::list_catalog_skills;
    let _ = pc_feature_catalog::teams::list_catalog_teams;
    let _ = pc_feature_catalog::skills::CatalogSkillListQuery::default();
    let _ = pc_feature_catalog::teams::CatalogTeamListQuery::default();
}

#[test]
fn r846_skills_list_resolves_by_id_and_key() {
    let manifest = build_skills_manifest();
    let all = list_catalog_skills(&manifest, &CatalogSkillListQuery::default());
    assert_eq!(all.len(), 2);
    // package metadata injected
    for s in &all {
        assert_eq!(s.package_name.as_deref(), Some("@paperclipai/skills-catalog"));
        assert_eq!(s.package_version.as_deref(), Some("9.9.9"));
    }
    let by_id = resolve_catalog_skill_reference(&manifest.skills, "s-1");
    assert!(by_id.skill.is_some());
    let by_key = resolve_catalog_skill_reference(&manifest.skills, "a/b");
    assert!(by_key.skill.is_some());
    let missing = resolve_catalog_skill_reference(&manifest.skills, "nope");
    assert!(missing.skill.is_none());
    assert!(!missing.ambiguous);
}

#[test]
fn r846_skills_parse_manifest_round_trip() {
    let manifest = build_skills_manifest();
    let json = serde_json::to_string(&manifest).unwrap();
    let parsed = parse_skills_manifest(&json).unwrap();
    assert_eq!(parsed.skills.len(), 2);
    assert_eq!(parsed.package_name, "@paperclipai/skills-catalog");
}

#[test]
fn r846_skills_markdown_helper() {
    assert!(skills_is_markdown("SKILL.md"));
    assert!(!skills_is_markdown("foo.txt"));
}

#[test]
fn r846_teams_list_filters_and_resolves() {
    let manifest = build_teams_manifest();
    let all = list_catalog_teams(&manifest, &CatalogTeamListQuery::default());
    assert_eq!(all.len(), 1);
    let filtered = list_catalog_teams(
        &manifest,
        &CatalogTeamListQuery {
            kind: Some("optional".into()),
            ..Default::default()
        },
    );
    assert_eq!(filtered.len(), 0);
}

#[test]
fn r846_teams_yaml_helpers() {
    // Scalar
    assert_eq!(yaml_scalar(&json!(null)), "null");
    assert_eq!(yaml_scalar(&json!("hello")), "\"hello\"");
    assert_eq!(yaml_scalar(&json!(123)), "123");
    // Block render
    let lines = render_yaml_block(&json!({"a": 1, "b": "x"}), 0);
    assert_eq!(lines, vec!["a: 1", "b: \"x\""]);
    // File render ends with newline
    let file = render_yaml_file(&json!({"k": "v"}));
    assert!(file.ends_with('\n'));
}

#[test]
fn r846_teams_synthetic_markdown_includes_frontmatter_and_body() {
    let manifest = build_teams_manifest();
    let md = render_synthetic_company_markdown(&manifest.teams[0]);
    assert!(md.starts_with("---\n"));
    assert!(md.contains("schema: agentcompanies/v1"));
    assert!(md.contains("# Team A"));
}

#[test]
fn r846_teams_provenance_read_round_trip() {
    let meta = json!({
        "paperclip": {
            "catalogTeam": {
                "catalogId": "team-1",
                "catalogKey": "k",
                "originHash": "h"
            }
        }
    });
    let p = read_catalog_team_provenance(Some(&meta)).expect("present");
    assert_eq!(p.catalog_id, "team-1");
    assert_eq!(p.catalog_key.as_deref(), Some("k"));
    assert_eq!(p.origin_hash.as_deref(), Some("h"));
}

#[test]
fn r846_teams_is_pinned_source_ref_predicate() {
    assert!(is_pinned_source_ref(Some(
        "0123456789abcdef0123456789abcdef01234567"
    )));
    assert!(!is_pinned_source_ref(Some("tooshort")));
    assert!(!is_pinned_source_ref(None));
}

#[test]
fn r846_teams_preparation_actions_cover_required_paths() {
    let manifest = build_teams_manifest();
    // bundled team with three requirements: catalog / local / github (unpinned)
    let result = collect_catalog_team_skill_preparations(
        &manifest.teams[0],
        &CatalogTeamSourcePolicy {
            allow_external_sources: true,
            ..Default::default()
        },
    );
    let actions: Vec<&str> = result.preparations.iter().map(|p| p.action.as_str()).collect();
    assert!(actions.contains(&SKILL_PREP_CATALOG_INSTALL_REQUIRED));
    assert!(actions.contains(&SKILL_PREP_ALREADY_IN_PACKAGE));
    assert!(actions.contains(&SKILL_PREP_BLOCKED));
    // github with unpinned ref + bundled → blocked
    assert!(!result.errors.is_empty());
}

#[test]
fn r846_teams_render_catalog_provenance_yaml_smoke() {
    let manifest = build_teams_manifest();
    let yaml = render_catalog_provenance_yaml(
        &manifest.teams[0],
        &manifest,
        Some(&TargetManagerReference {
            agent_id: "agent-x".into(),
            slug: "alpha".into(),
        }),
    );
    assert!(yaml.contains("schema: \"paperclip/v1\""));
    assert!(yaml.contains("agents:"));
    assert!(yaml.contains("projects:"));
    assert!(yaml.contains("reportsToExistingAgentId: \"agent-x\""));
}

#[test]
fn r846_teams_parse_yaml_document_extracts_top_level_keys() {
    let yaml = "schema: paperclip/v1\ncount: 5";
    let v = parse_yaml_document(yaml);
    assert_eq!(v["schema"], json!("paperclip/v1"));
    assert_eq!(v["count"], json!(5));
}

#[test]
fn r846_teams_markdown_helper() {
    assert!(teams_is_markdown("TEAM.md"));
    assert!(teams_is_markdown("docs/team.md"));
    assert!(!teams_is_markdown("foo.ts"));
}