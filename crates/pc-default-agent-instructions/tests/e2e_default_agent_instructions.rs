use std::sync::Arc;
use pc_default_agent_instructions::{
    AgentInstructionsRole, DefaultAgentInstructionsHookEvent, DefaultAgentInstructionsService,
    RecordingDefaultHook,
};

#[tokio::test]
async fn resolve_role_and_load_bundle() {
    let s = DefaultAgentInstructionsService::new();
    assert_eq!(s.resolve_role("ceo"), AgentInstructionsRole::Ceo);
    assert_eq!(s.resolve_role("default"), AgentInstructionsRole::Default);
    assert_eq!(s.resolve_role("unknown"), AgentInstructionsRole::Default);
    let bundle = s.load_bundle_canonical(AgentInstructionsRole::Ceo);
    assert_eq!(bundle.len(), 4);
    assert!(bundle.contains_key("AGENTS.md"));
    assert!(bundle.contains_key("HEARTBEAT.md"));
    assert!(bundle.contains_key("SOUL.md"));
    assert!(bundle.contains_key("TOOLS.md"));
    let default_bundle = s.load_bundle_canonical(AgentInstructionsRole::Default);
    assert_eq!(default_bundle.len(), 1);
}

#[tokio::test]
async fn load_bundle_for_dispatches() {
    let h = Arc::new(RecordingDefaultHook::default());
    let s = DefaultAgentInstructionsService::with_hooks(vec![h.clone()]);
    let bundle = s.load_bundle_for("ceo").await.unwrap();
    assert_eq!(bundle.len(), 4);
    assert!(h.events_snapshot().iter().any(|e| matches!(e, DefaultAgentInstructionsHookEvent::Resolved { role, .. } if role == "ceo")));
    let _ = s.load_bundle_for("unknown").await.unwrap();
}

#[tokio::test]
async fn validation() {
    let s = DefaultAgentInstructionsService::new();
    assert!(s.load_bundle_for("").await.is_err());
    assert!(s.load_bundle_for("   ").await.is_err());
}
