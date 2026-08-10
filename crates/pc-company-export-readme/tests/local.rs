//! 单元测试：mermaid_id / mermaid_escape / role_label / skill_source_label +
//! mermaid chart 生成 + README 完整结构。

use pc_company_export_readme::{
    generate_org_chart_mermaid, generate_readme, mermaid_escape, mermaid_id, role_label,
    skill_source_label, CompanyPortabilityManifest, ManifestAgent, ManifestIssue,
    ManifestProject, ManifestSkill, ReadmeOptions, SkillSourceType,
};

#[test]
fn mermaid_id_replaces_non_alphanumeric() {
    assert_eq!(mermaid_id("hello-world"), "hello_world");
    assert_eq!(mermaid_id("foo/bar baz"), "foo_bar_baz");
    assert_eq!(mermaid_id("a_b-c.d"), "a_b_c_d");
    assert_eq!(mermaid_id("valid_slug"), "valid_slug");
}

#[test]
fn mermaid_escape_replaces_special_chars() {
    assert_eq!(mermaid_escape(r#"hello "world""#), "hello &quot;world&quot;");
    assert_eq!(mermaid_escape("<b>tag</b>"), "&lt;b&gt;tag&lt;/b&gt;");
    assert_eq!(mermaid_escape("plain"), "plain");
}

#[test]
fn role_label_maps_known_roles() {
    assert_eq!(role_label("ceo"), "CEO");
    assert_eq!(role_label("cto"), "CTO");
    assert_eq!(role_label("cmo"), "CMO");
    assert_eq!(role_label("cfo"), "CFO");
    assert_eq!(role_label("coo"), "COO");
    assert_eq!(role_label("vp"), "VP");
    assert_eq!(role_label("manager"), "Manager");
    assert_eq!(role_label("engineer"), "Engineer");
    assert_eq!(role_label("agent"), "Agent");
}

#[test]
fn role_label_passes_through_unknown() {
    assert_eq!(role_label("custom-role"), "custom-role");
    assert_eq!(role_label(""), "");
}

#[test]
fn generate_mermaid_returns_none_for_empty() {
    let agents: Vec<ManifestAgent> = Vec::new();
    assert!(generate_org_chart_mermaid(&agents).is_none());
}

#[test]
fn generate_mermaid_with_single_agent() {
    let agents = vec![ManifestAgent {
        slug: "ceo".to_string(),
        name: "Alice".to_string(),
        role: "ceo".to_string(),
        reports_to_slug: None,
    }];
    let chart = generate_org_chart_mermaid(&agents).expect("some");
    assert!(chart.starts_with("```mermaid"));
    assert!(chart.contains("graph TD"));
    assert!(chart.contains("ceo[\"Alice"));
    assert!(chart.contains("CEO"));
    assert!(chart.ends_with("```"));
}

#[test]
fn generate_mermaid_with_parent_child_edge() {
    let agents = vec![
        ManifestAgent {
            slug: "ceo".to_string(),
            name: "Alice".to_string(),
            role: "ceo".to_string(),
            reports_to_slug: None,
        },
        ManifestAgent {
            slug: "eng-1".to_string(),
            name: "Bob".to_string(),
            role: "engineer".to_string(),
            reports_to_slug: Some("ceo".to_string()),
        },
    ];
    let chart = generate_org_chart_mermaid(&agents).expect("some");
    assert!(chart.contains("ceo --> eng_1"));
}

#[test]
fn generate_mermaid_drops_edges_to_unknown_parents() {
    let agents = vec![ManifestAgent {
        slug: "orphan".to_string(),
        name: "Orphan".to_string(),
        role: "engineer".to_string(),
        reports_to_slug: Some("nonexistent".to_string()),
    }];
    let chart = generate_org_chart_mermaid(&agents).expect("some");
    // Edge 应该被丢弃（parent 不在 manifest 中）
    assert!(!chart.contains("-->"));
}

#[test]
fn generate_mermaid_escapes_special_chars_in_label() {
    let agents = vec![ManifestAgent {
        slug: "a".to_string(),
        name: "Hello \"World\"".to_string(),
        role: "engineer".to_string(),
        reports_to_slug: None,
    }];
    let chart = generate_org_chart_mermaid(&agents).expect("some");
    assert!(chart.contains("&quot;World&quot;"));
}

#[test]
fn skill_source_label_github_with_locator_renders_link() {
    let s = ManifestSkill {
        name: "sk".to_string(),
        description: None,
        source_type: Some(SkillSourceType::Github),
        source_locator: Some("https://github.com/foo/bar".to_string()),
    };
    assert_eq!(
        skill_source_label(&s),
        "[github](https://github.com/foo/bar)"
    );
}

#[test]
fn skill_source_label_skills_sh() {
    let s = ManifestSkill {
        name: "sk".to_string(),
        description: None,
        source_type: Some(SkillSourceType::SkillsSh),
        source_locator: Some("https://skills.sh/foo".to_string()),
    };
    assert_eq!(skill_source_label(&s), "[skills_sh](https://skills.sh/foo)");
}

#[test]
fn skill_source_label_url_locator_without_recognized_type() {
    let s = ManifestSkill {
        name: "sk".to_string(),
        description: None,
        source_type: Some(SkillSourceType::Local),
        source_locator: Some("/some/path".to_string()),
    };
    // Local type 但有 locator → 直接用 locator
    assert_eq!(skill_source_label(&s), "/some/path");
}

#[test]
fn skill_source_label_local_without_locator() {
    let s = ManifestSkill {
        name: "sk".to_string(),
        description: None,
        source_type: Some(SkillSourceType::Local),
        source_locator: None,
    };
    assert_eq!(skill_source_label(&s), "local");
}

#[test]
fn skill_source_label_unknown_emits_dash() {
    let s = ManifestSkill {
        name: "sk".to_string(),
        description: None,
        source_type: None,
        source_locator: None,
    };
    assert_eq!(skill_source_label(&s), "\u{2014}");
}

#[test]
fn generate_readme_with_no_agents_no_org_chart_section() {
    let manifest = CompanyPortabilityManifest {
        agents: vec![],
        projects: vec![ManifestProject {
            name: "P1".to_string(),
            description: Some("first project".to_string()),
        }],
        skills: vec![],
        issues: vec![],
    };
    let md = generate_readme(
        &manifest,
        &ReadmeOptions {
            company_name: "Acme".to_string(),
            company_description: None,
        },
    );
    assert!(md.starts_with("# Acme"));
    assert!(md.contains("## What's Inside"));
    // 无 agents → 无 org chart image
    assert!(!md.contains("![Org Chart]"));
    // 无 agents → 无 Agents 表
    assert!(!md.contains("### Agents"));
    // 有 projects → 应列出
    assert!(md.contains("- **P1**"));
    // Footer
    assert!(md.contains("Exported from [Paperclip]"));
}

#[test]
fn generate_readme_with_agents_full_structure() {
    let manifest = CompanyPortabilityManifest {
        agents: vec![
            ManifestAgent {
                slug: "ceo".to_string(),
                name: "Alice".to_string(),
                role: "ceo".to_string(),
                reports_to_slug: None,
            },
            ManifestAgent {
                slug: "eng-1".to_string(),
                name: "Bob".to_string(),
                role: "engineer".to_string(),
                reports_to_slug: Some("ceo".to_string()),
            },
        ],
        projects: vec![],
        skills: vec![ManifestSkill {
            name: "rust-skill".to_string(),
            description: Some("Rust best practices".to_string()),
            source_type: Some(SkillSourceType::Github),
            source_locator: Some("https://github.com/foo/rust-skill".to_string()),
        }],
        issues: vec![ManifestIssue { id: "i-1".to_string() }],
    };
    let md = generate_readme(
        &manifest,
        &ReadmeOptions {
            company_name: "Acme Co.".to_string(),
            company_description: Some("A cool AI company".to_string()),
        },
    );
    assert!(md.contains("# Acme Co."));
    assert!(md.contains("> A cool AI company"));
    assert!(md.contains("![Org Chart](images/org-chart.png)"));
    assert!(md.contains("| Agents | 2 |"));
    assert!(md.contains("| Skills | 1 |"));
    assert!(md.contains("| Tasks | 1 |"));
    assert!(md.contains("| Agent | Role | Reports To |"));
    assert!(md.contains("| Alice | CEO | \u{2014} |"));
    assert!(md.contains("| Bob | Engineer | ceo |"));
    assert!(md.contains("| rust-skill |"));
    assert!(md.contains("[github](https://github.com/foo/rust-skill)"));
    assert!(md.contains("## Getting Started"));
    assert!(md.contains("pnpm paperclipai company import"));
    assert!(md.contains("Exported from [Paperclip]"));
}

#[test]
fn generate_readme_with_empty_manifest_minimal() {
    let manifest = CompanyPortabilityManifest::default();
    let md = generate_readme(
        &manifest,
        &ReadmeOptions {
            company_name: "Empty".to_string(),
            company_description: None,
        },
    );
    assert!(md.contains("# Empty"));
    // 无 agents / projects / skills / issues → What's Inside 表也不显示
    assert!(!md.contains("| Content | Count |"));
}

#[test]
fn deserialize_manifest_from_camel_case_json() {
    let json = serde_json::json!({
        "agents": [
            { "slug": "a", "name": "A", "role": "ceo", "reportsToSlug": null },
            { "slug": "b", "name": "B", "role": "engineer", "reportsToSlug": "a" }
        ],
        "projects": [
            { "name": "P1", "description": "first" }
        ],
        "skills": [
            { "name": "sk1", "description": null, "sourceType": "github", "sourceLocator": "https://github.com/foo" }
        ],
        "issues": [
            { "id": "i-1" }
        ]
    });
    let manifest: CompanyPortabilityManifest = serde_json::from_value(json).expect("deserialize");
    assert_eq!(manifest.agents.len(), 2);
    assert_eq!(manifest.agents[1].reports_to_slug.as_deref(), Some("a"));
    assert_eq!(manifest.projects.len(), 1);
    assert_eq!(manifest.skills[0].source_type, Some(SkillSourceType::Github));
    assert_eq!(manifest.issues.len(), 1);
}
