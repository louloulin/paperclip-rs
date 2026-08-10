//! 为 company export bundle 生成 README.md（含 Mermaid org chart）。
//!
//! 对齐 Node `services/company-export-readme.ts`：
//! - `ROLE_LABELS`: 角色字符串 → 显示标签的映射
//! - `generateOrgChartMermaid`: 构造 Mermaid TD flowchart 字符串（无 agents 时返回 None）
//! - `generateReadme`: 完整 README.md（含 What's Inside / Agents / Projects / Skills 表）
//! - `mermaidId`: sanitize slug 成合法 mermaid ID（`[a-zA-Z0-9_]`）
//! - `mermaidEscape`: 转义 `"` / `<` / `>`

use serde::{Deserialize, Serialize};

/// Manifest 中的 agent 投影。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestAgent {
    pub slug: String,
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub reports_to_slug: Option<String>,
}

/// Manifest 中的 project 投影。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestProject {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Skill source 类型（与 Node 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceType {
    Github,
    SkillsSh,
    Url,
    Local,
    /// 未知类型，作为 fallback。
    #[serde(other)]
    Unknown,
}

/// Manifest 中的 skill 投影。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSkill {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "sourceType")]
    pub source_type: Option<SkillSourceType>,
    #[serde(default, rename = "sourceLocator")]
    pub source_locator: Option<String>,
}

/// Manifest 中的 issue 投影（仅 count 参与 README，所以最简 shape）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestIssue {
    pub id: String,
}

/// Company portability manifest（与 Node `CompanyPortabilityManifest` 1:1 对齐）。
///
/// 这个结构是 `generateReadme` 唯一依赖的输入；其他字段（counts 等）由 caller 决定。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompanyPortabilityManifest {
    #[serde(default)]
    pub agents: Vec<ManifestAgent>,
    #[serde(default)]
    pub projects: Vec<ManifestProject>,
    #[serde(default)]
    pub skills: Vec<ManifestSkill>,
    #[serde(default)]
    pub issues: Vec<ManifestIssue>,
}

/// `generateReadme` 选项。
#[derive(Debug, Clone)]
pub struct ReadmeOptions {
    pub company_name: String,
    pub company_description: Option<String>,
}

/// 把 slug 清理成合法 mermaid 节点 ID（仅保留 `[a-zA-Z0-9_]`）。
pub fn mermaid_id(slug: &str) -> String {
    slug.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Escape Mermaid label 中的特殊字符。
pub fn mermaid_escape(s: &str) -> String {
    s.replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Role → 显示标签映射（与 Node `ROLE_LABELS` 1:1）。
pub fn role_label(role: &str) -> &str {
    match role {
        "ceo" => "CEO",
        "cto" => "CTO",
        "cmo" => "CMO",
        "cfo" => "CFO",
        "coo" => "COO",
        "vp" => "VP",
        "manager" => "Manager",
        "engineer" => "Engineer",
        "agent" => "Agent",
        _ => role,
    }
}

/// 生成 Mermaid TD flowchart 字符串。无 agents 时返回 `None`。
pub fn generate_org_chart_mermaid(
    agents: &[ManifestAgent],
) -> Option<String> {
    if agents.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = Vec::new();
    lines.push("```mermaid".to_string());
    lines.push("graph TD".to_string());

    // Node definitions
    for agent in agents {
        let id = mermaid_id(&agent.slug);
        let label = role_label(&agent.role);
        lines.push(format!(
            "    {id}[\"{name}<br/><small>{label}</small>\"]",
            name = mermaid_escape(&agent.name),
            label = mermaid_escape(label),
        ));
    }

    // Edges (parent → child)；只保留指向同 manifest 中存在的 slug。
    let slug_set: std::collections::HashSet<&str> =
        agents.iter().map(|a| a.slug.as_str()).collect();
    for agent in agents {
        if let Some(parent_slug) = &agent.reports_to_slug {
            if slug_set.contains(parent_slug.as_str()) {
                lines.push(format!(
                    "    {} --> {}",
                    mermaid_id(parent_slug),
                    mermaid_id(&agent.slug)
                ));
            }
        }
    }

    lines.push("```".to_string());
    Some(lines.join("\n"))
}

/// Skill source 的显示标签（含 markdown link）。
pub fn skill_source_label(skill: &ManifestSkill) -> String {
    if let Some(locator) = &skill.source_locator {
        match skill.source_type.as_ref() {
            Some(SkillSourceType::Github)
            | Some(SkillSourceType::SkillsSh)
            | Some(SkillSourceType::Url) => {
                let t = match skill.source_type.as_ref() {
                    Some(SkillSourceType::Github) => "github",
                    Some(SkillSourceType::SkillsSh) => "skills_sh",
                    Some(SkillSourceType::Url) => "url",
                    _ => "url",
                };
                return format!("[{t}]({locator})");
            }
            _ => return locator.clone(),
        }
    }
    match skill.source_type.as_ref() {
        Some(SkillSourceType::Local) => "local".to_string(),
        Some(other) => format!("{other:?}").to_lowercase(),
        None => "\u{2014}".to_string(),
    }
}

/// 生成 README.md。
pub fn generate_readme(
    manifest: &CompanyPortabilityManifest,
    options: &ReadmeOptions,
) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("# {}", options.company_name));
    lines.push(String::new());
    if let Some(desc) = &options.company_description {
        lines.push(format!("> {desc}"));
        lines.push(String::new());
    }

    // Org chart image（导出时由 caller 渲染为 images/org-chart.png）。
    if !manifest.agents.is_empty() {
        lines.push("![Org Chart](images/org-chart.png)".to_string());
        lines.push(String::new());
    }

    // What's Inside
    lines.push("## What's Inside".to_string());
    lines.push(String::new());
    lines.push(
        "> This is an [Agent Company](https://agentcompanies.io) package from [Paperclip](https://paperclip.ing)"
            .to_string(),
    );
    lines.push(String::new());

    let mut counts: Vec<(&str, usize)> = Vec::new();
    if !manifest.agents.is_empty() {
        counts.push(("Agents", manifest.agents.len()));
    }
    if !manifest.projects.is_empty() {
        counts.push(("Projects", manifest.projects.len()));
    }
    if !manifest.skills.is_empty() {
        counts.push(("Skills", manifest.skills.len()));
    }
    if !manifest.issues.is_empty() {
        counts.push(("Tasks", manifest.issues.len()));
    }
    if !counts.is_empty() {
        lines.push("| Content | Count |".to_string());
        lines.push("|---------|-------|".to_string());
        for (label, count) in counts {
            lines.push(format!("| {label} | {count} |"));
        }
        lines.push(String::new());
    }

    // Agents table
    if !manifest.agents.is_empty() {
        lines.push("### Agents".to_string());
        lines.push(String::new());
        lines.push("| Agent | Role | Reports To |".to_string());
        lines.push("|-------|------|------------|".to_string());
        for agent in &manifest.agents {
            let label = role_label(&agent.role);
            let reports_to = agent
                .reports_to_slug
                .clone()
                .unwrap_or_else(|| "\u{2014}".to_string());
            lines.push(format!(
                "| {} | {} | {} |",
                agent.name, label, reports_to
            ));
        }
        lines.push(String::new());
    }

    // Projects list
    if !manifest.projects.is_empty() {
        lines.push("### Projects".to_string());
        lines.push(String::new());
        for project in &manifest.projects {
            let desc = project
                .description
                .as_ref()
                .map(|d| format!(" \u{2014} {d}"))
                .unwrap_or_default();
            lines.push(format!("- **{}**{desc}", project.name));
        }
        lines.push(String::new());
    }

    // Skills table
    if !manifest.skills.is_empty() {
        lines.push("### Skills".to_string());
        lines.push(String::new());
        lines.push("| Skill | Description | Source |".to_string());
        lines.push("|-------|-------------|--------|".to_string());
        for skill in &manifest.skills {
            let desc = skill.description.clone().unwrap_or_else(|| "\u{2014}".to_string());
            let source = skill_source_label(skill);
            lines.push(format!("| {} | {} | {} |", skill.name, desc, source));
        }
        lines.push(String::new());
    }

    // Getting Started
    lines.push("## Getting Started".to_string());
    lines.push(String::new());
    lines.push("```bash".to_string());
    lines.push("pnpm paperclipai company import this-github-url-or-folder".to_string());
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push("See [Paperclip](https://paperclip.ing) for more information.".to_string());
    lines.push(String::new());

    // Footer
    lines.push("---".to_string());
    let today = chrono::Utc::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    lines.push(format!(
        "Exported from [Paperclip](https://paperclip.ing) on {today}"
    ));
    lines.push(String::new());

    lines.join("\n")
}
