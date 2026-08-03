use pc_agent::{
    AgentInstructionsService, InstructionAgent, InstructionsBundleUpdate,
};
use pc_errors::Error;
use serde_json::json;

#[tokio::test]
async fn managed_bundle_crud_and_path_guards_match_node_contract() {
    let root = tempfile::tempdir().expect("temp root");
    let service = AgentInstructionsService::new(root.path().join("instance"));
    let agent = InstructionAgent {
        id: "agent-1".into(),
        company_id: "company-1".into(),
        name: "Agent One".into(),
        adapter_config: json!({"promptTemplate": "# Legacy\n"}),
    };

    let written = service
        .write_file(&agent, "AGENTS.md", "# Managed\n", false)
        .await
        .expect("write entry");
    assert_eq!(written.file.path, "AGENTS.md");
    assert_eq!(written.file.content, "# Managed\n");
    assert_eq!(written.bundle.mode.as_deref(), Some("managed"));
    assert!(written.bundle.root_path.as_deref().is_some_and(|path| path.ends_with(
        "companies/company-1/agents/agent-1/instructions"
    )));
    assert_eq!(
        written.adapter_config["instructionsEntryFile"],
        "AGENTS.md"
    );

    let configured = InstructionAgent {
        adapter_config: written.adapter_config,
        ..agent.clone()
    };
    service
        .write_file(&configured, "docs/TOOLS.md", "## Tools\n", false)
        .await
        .expect("write nested file");
    let detail = service
        .read_file(&configured, "docs/TOOLS.md")
        .await
        .expect("read nested file");
    assert_eq!(detail.language, "markdown");
    assert!(detail.markdown);

    assert!(matches!(
        service.read_file(&configured, "../secret.txt").await,
        Err(Error::Unprocessable { .. })
    ));
    assert!(matches!(
        service.delete_file(&configured, "AGENTS.md").await,
        Err(Error::Unprocessable { .. })
    ));
    let bundle = service
        .delete_file(&configured, "docs/TOOLS.md")
        .await
        .expect("delete nested file")
        .bundle;
    let paths = bundle
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"AGENTS.md"));
    assert!(paths.contains(&"promptTemplate.legacy.md"));
    assert!(!paths.contains(&"docs/TOOLS.md"));
}

#[tokio::test]
async fn external_bundle_requires_absolute_root() {
    let root = tempfile::tempdir().expect("temp root");
    let service = AgentInstructionsService::new(root.path().join("instance"));
    let agent = InstructionAgent {
        id: "agent-2".into(),
        company_id: "company-1".into(),
        name: "Agent Two".into(),
        adapter_config: json!({}),
    };
    let error = service
        .update_bundle(
            &agent,
            InstructionsBundleUpdate {
                mode: Some("external".into()),
                root_path: Some(Some("relative/path".into())),
                ..InstructionsBundleUpdate::default()
            },
        )
        .await
        .expect_err("relative external root must fail");
    assert!(matches!(error, Error::Unprocessable { .. }));
}
