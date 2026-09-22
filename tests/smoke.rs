//! Workspace-level integration smoke test.
//!
//! 验证 mc-config + mc-core + mc-errors + mc-auth + mc-authz +
//! mc-realtime + mc-feature-flags + mc-storage 的最小交互。

use mc_auth::password::{hash_password, verify_password};
use mc_authz::{authorize, Action, AuthorizationRequest, Decision, Principal, Resource};
use mc_core::actor::{spawn_system_actor, ActorKey, ActorRegistry};
use mc_core::workspace::WorkspaceRole;
use mc_core::Id;
use mc_errors::Error;

#[test]
fn full_smoke() {
    // 1. Config builds from minimal env.
    let cfg = mc_config::Config::build_with(|name| match name {
        "MULTICA_DATABASE_URL" => Some("postgres://u:p@host:5432/db".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(cfg.server.port, 3500);

    // 2. Core types construct.
    let id = Id::new();
    assert!(!id.is_nil());

    // 3. Errors round-trip.
    let err = Error::NotFound { resource: "issue".into() };
    assert_eq!(err.http_status(), 404);
    assert_eq!(err.code(), "not_found");

    // 4. Auth password round-trip.
    let hashed = hash_password("hunter2");
    assert!(verify_password("hunter2", &hashed));
    assert!(!verify_password("wrong", &hashed));

    // 5. Authz owner allows everything.
    let owner = Principal::User {
        id: Id::new(),
        role: WorkspaceRole::Owner,
    };
    assert_eq!(
        decide(&AuthorizationRequest::new(owner, Resource::Workspace, Action::Admin)),
        Decision::Allow
    );

    // 6. Actor registry works.
    let reg = ActorRegistry::new();
    let sys = spawn_system_actor("root");
    reg.register(ActorKey::new("system", "root"), sys).unwrap();
    assert_eq!(reg.list().len(), 1);

    // 7. Realtime bus works.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        use mc_realtime::{envelope, EventBus};
        let bus = EventBus::with_capacity(4);
        let mut sub = bus.subscribe();
        bus.publish(envelope("issue", "i-1", None, serde_json::json!({"k": 1})));
        let env = sub.recv().await.expect("event arrived");
        assert_eq!(env.resource, "issue");
    });

    // 8. Feature flag catalog.
    let catalog = mc_feature_flags::FeatureFlagCatalog::new();
    catalog.register(
        mc_feature_flags::FeatureKey::new("multica.test.flag"),
        true,
        None,
    );
    assert!(catalog.is_enabled(&mc_feature_flags::FeatureKey::new("multica.test.flag")));
}

fn decide(req: &AuthorizationRequest) -> Decision {
    match authorize(req) {
        Ok(()) => Decision::Allow,
        Err(_) => Decision::Deny,
    }
}