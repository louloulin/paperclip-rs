#![forbid(unsafe_code)]

//! Seed demo data for e2e UI workflow validation.
//!
//! Per design doc D1: deterministic UUID namespace
//!   `00000000-0000-0000-0000-0000000000XX` for e2e assertions.
//!
//! Idempotent: if demo company already exists, reuse it.

use anyhow::{Context, Result};
use pc_repos::Db;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

/// Demo company UUID — deterministic for e2e assertions.
pub const DEMO_COMPANY_ID: Uuid = Uuid::from_bytes([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// Demo admin user ID (string form, since `user.id` is TEXT in DB).
pub const DEMO_ADMIN_USER_ID: &str = "00000000-0000-0000-0000-000000000010";

/// Seed outcome — useful for e2e assertions.
#[derive(Debug, Clone)]
pub struct SeedOutcome {
    pub company_id: Uuid,
    pub admin_user_id: String,
    pub admin_username: String,
    pub agents_seeded: usize,
    pub issues_seeded: usize,
    pub pipelines_seeded: usize,
    pub projects_seeded: usize,
    pub routines_seeded: usize,
    pub reused: bool,
}

/// Run demo seed.
///
/// 1. Check if demo company exists; if so, return reused=true
/// 2. Otherwise create demo company + admin user + 5 agents + 10 issues
///    + 2 pipelines + 2 projects
pub async fn seed_demo(db: &Db, username: &str, _realtime: Option<Arc<()>>) -> Result<SeedOutcome> {
    // Check if demo company already exists
    let company_repo = pc_repos::company::CompanyRepo::new(db);
    if company_repo.exists(DEMO_COMPANY_ID).await? {
        info!(company_id = %DEMO_COMPANY_ID, "demo company already exists, reusing");
        return Ok(SeedOutcome {
            company_id: DEMO_COMPANY_ID,
            admin_user_id: DEMO_ADMIN_USER_ID.to_string(),
            admin_username: username.to_string(),
            agents_seeded: 0,
            issues_seeded: 0,
            pipelines_seeded: 0,
            projects_seeded: 0,
            routines_seeded: 0,
            reused: true,
        });
    }

    info!(company_id = %DEMO_COMPANY_ID, username, "seeding demo data");

    // 1. Create company
    let company = company_repo
        .create(
            "Demo Company",
            Some("Demo company seeded for e2e UI workflow validation"),
        )
        .await
        .context("create demo company")?;
    let actual_company_id = company.id;
    warn!(
        requested = %DEMO_COMPANY_ID,
        actual = %actual_company_id,
        "demo company UUID is determined by DB default; using actual UUID"
    );

    // 2. Create admin user via AuthRepo::ensure_user
    let auth_repo = pc_repos::auth::AuthRepo::new(db);
    let email = format!("{username}@demo.local");
    match auth_repo
        .ensure_user(DEMO_ADMIN_USER_ID, username, &email)
        .await
        .context("ensure demo admin user")?
    {
        Some(_) => info!(user_id = DEMO_ADMIN_USER_ID, "admin user created"),
        None => info!(user_id = DEMO_ADMIN_USER_ID, "admin user already existed"),
    };

    // 3. Seed 5 agents via AgentRepo::create_simple
    let agent_repo = pc_repos::agent::AgentRepo::new(db);
    let mut agents_seeded = 0usize;
    let agent_specs = [
        ("claude-local-demo", "executor"),
        ("codex-local-demo", "reviewer"),
        ("gemini-local-demo", "planner"),
        ("grok-local-demo", "monitor"),
        ("hermes-demo", "router"),
    ];
    for (name, role) in agent_specs {
        match agent_repo
            .create_simple(actual_company_id, name, role)
            .await
        {
            Ok(_) => agents_seeded += 1,
            Err(e) => warn!(error = %e, name, "create agent failed"),
        }
    }

    // 4. Seed 10 issues via IssueRepo::create
    let issue_repo = pc_repos::issue::IssueRepo::new(db);
    let mut issues_seeded = 0usize;
    let issue_titles = [
        "Setup CI pipeline",
        "Add authentication",
        "Write API docs",
        "Fix bug #123",
        "Refactor legacy code",
        "Add integration tests",
        "Update dependencies",
        "Performance audit",
        "Security review",
        "Deploy to staging",
    ];
    for title in issue_titles {
        match issue_repo
            .create(actual_company_id, title, None, "medium", None)
            .await
        {
            Ok(_) => issues_seeded += 1,
            Err(e) => warn!(error = %e, title, "create issue failed"),
        }
    }

    // 5. Seed 2 pipelines via PipelineRepo::create
    let pipeline_repo = pc_repos::pipeline::PipelineRepo::new(db);
    let mut pipelines_seeded = 0usize;
    let pipeline_specs = [("demo-build", "Demo build pipeline"), ("demo-deploy", "Demo deploy pipeline")];
    for (key, name) in pipeline_specs {
        match pipeline_repo
            .create(actual_company_id, key, name, None)
            .await
        {
            Ok(_) => pipelines_seeded += 1,
            Err(e) => warn!(error = %e, key, "create pipeline failed"),
        }
    }

    // 6. Seed 2 projects via ProjectRepo
    let project_repo = pc_repos::project::ProjectRepo::new(db);
    let mut projects_seeded = 0usize;
    let project_specs = [
        ("Demo Project Alpha", "active"),
        ("Demo Project Beta", "backlog"),
    ];
    for (name, status) in project_specs {
        let new_project = pc_repos::project::NewProject {
            company_id: actual_company_id,
            goal_id: None,
            name: name.to_string(),
            description: Some(format!("Seed project: {name}")),
            status: match status {
                "active" => pc_repos::project::ProjectStatus::Active,
                _ => pc_repos::project::ProjectStatus::Backlog,
            },
            lead_agent_id: None,
            target_date: None,
            color: None,
            icon: None,
            env: None,
        };
        match project_repo.create(&new_project).await {
            Ok(_) => projects_seeded += 1,
            Err(e) => warn!(error = %e, name, "create project failed"),
        }
    }

    Ok(SeedOutcome {
        company_id: actual_company_id,
        admin_user_id: DEMO_ADMIN_USER_ID.to_string(),
        admin_username: username.to_string(),
        agents_seeded,
        issues_seeded,
        pipelines_seeded,
        projects_seeded,
        routines_seeded: 0, // Routines skipped (CreateRoutine has no cron field — see R-FUTURE)
        reused: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_company_id_is_correct() {
        assert_eq!(
            DEMO_COMPANY_ID.as_bytes()[15],
            0x01,
            "demo company UUID suffix must be 0x01"
        );
    }

    #[test]
    fn demo_admin_user_id_is_correct() {
        assert!(
            DEMO_ADMIN_USER_ID.starts_with("00000000-0000-0000-0000-000000000010"),
            "demo admin user ID must end with 0x10"
        );
    }

    #[test]
    fn seed_outcome_clone_works() {
        let outcome = SeedOutcome {
            company_id: DEMO_COMPANY_ID,
            admin_user_id: DEMO_ADMIN_USER_ID.to_string(),
            admin_username: "admin".into(),
            agents_seeded: 5,
            issues_seeded: 10,
            pipelines_seeded: 2,
            projects_seeded: 2,
            routines_seeded: 0,
            reused: false,
        };
        let cloned = outcome.clone();
        assert_eq!(outcome.company_id, cloned.company_id);
        assert_eq!(outcome.admin_username, cloned.admin_username);
        assert_eq!(outcome.agents_seeded, cloned.agents_seeded);
        assert_eq!(outcome.issues_seeded, cloned.issues_seeded);
    }

    #[test]
    fn seed_outcome_reused_preserves_counts() {
        let outcome = SeedOutcome {
            company_id: DEMO_COMPANY_ID,
            admin_user_id: DEMO_ADMIN_USER_ID.to_string(),
            admin_username: "admin".into(),
            agents_seeded: 0,
            issues_seeded: 0,
            pipelines_seeded: 0,
            projects_seeded: 0,
            routines_seeded: 0,
            reused: true,
        };
        assert!(outcome.reused);
        assert_eq!(outcome.agents_seeded, 0);
        assert_eq!(outcome.issues_seeded, 0);
    }
}