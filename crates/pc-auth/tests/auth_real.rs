//! M7 真实验证：pc-auth 关键路径（password/session/actor/authz）。
//!
//! 与 Node `services/authorization.ts` + `auth/better-auth.ts` 行为对齐。

use pc_auth::{
    generate_session_token, hash_password, hash_token, verify_password, Actor, ActorSource,
    AuthContext, AuthError, KeyScope,
};
use uuid::Uuid;

fn user(id: &str, cids: Vec<Uuid>) -> Actor {
    Actor::User {
        id: id.into(),
        name: None,
        email: None,
        is_instance_admin: false,
        company_ids: cids,
        memberships: vec![],
        run_id: None,
    }
}

fn agent(id: Uuid, company_id: Uuid) -> Actor {
    Actor::Agent {
        id,
        company_id,
        key_id: None,
        key_scope: KeyScope::default(),
        run_id: None,
        on_behalf_of_user_id: None,
        on_behalf_of_memberships: vec![],
    }
}

#[test]
fn password_hash_verify_roundtrip() {
    let stored = hash_password("correct horse battery staple").expect("hash");
    assert!(stored.len() > 40, "stored hash should be long enough");
    assert!(verify_password("correct horse battery staple", &stored));
    assert!(!verify_password("wrong password", &stored));
}

#[test]
fn session_token_format_and_hash() {
    let tok = generate_session_token();
    assert!(tok.len() >= 32);
    let h1 = hash_token(&tok);
    let h2 = hash_token(&tok);
    assert_eq!(h1, h2, "hash deterministic");
    assert_ne!(h1, tok, "hash != plaintext token");
}

#[test]
fn actor_helpers() {
    let anon = Actor::anonymous();
    assert!(!anon.is_authenticated());
    let sys = Actor::system();
    assert!(sys.is_authenticated());
    let aid = Uuid::new_v4();
    let cid = Uuid::new_v4();
    let u = user("u-123", vec![cid]);
    assert_eq!(u.user_id(), Some("u-123"));
    let a = agent(aid, cid);
    assert_eq!(a.agent_id(), Some(aid));
    assert_eq!(a.company_id(), Some(cid));
    assert!(!u.is_instance_admin());
    assert!(u.has_company_access(cid));
    assert!(!u.has_company_access(Uuid::new_v4()));
}

#[test]
fn auth_context_require_user_and_company() {
    let ctx = AuthContext::system();
    let r = ctx.require_user();
    assert!(r.is_err());
    let ctx = AuthContext::anonymous();
    assert!(ctx.require_authenticated().is_err());

    let cid = Uuid::new_v4();
    let ctx = AuthContext::for_actor(user("u1", vec![cid]), ActorSource::Session, "POST");
    assert_eq!(ctx.require_user().unwrap(), "u1");
    assert!(ctx.require_company_access(cid).is_ok());
    let other = Uuid::new_v4();
    assert!(ctx.require_company_access(other).is_err());
}

#[test]
fn instance_admin_passes_any_company() {
    let actor = Actor::User {
        id: "admin1".into(),
        name: None,
        email: None,
        is_instance_admin: true,
        company_ids: vec![],
        memberships: vec![],
        run_id: None,
    };
    let ctx = AuthContext::for_actor(actor, ActorSource::Session, "GET");
    let any = Uuid::new_v4();
    assert!(ctx.require_company_access(any).is_ok());
}

#[test]
fn key_scope_serializes() {
    let s = KeyScope {
        can_manage_company: true,
        ..KeyScope::default()
    };
    let json = serde_json::to_string(&s).unwrap();
    assert!(json.contains("can_manage_company"));
}

#[test]
fn actor_source_serializes() {
    let j = serde_json::to_string(&ActorSource::Session).unwrap();
    assert!(j.contains("session"));
    let j = serde_json::to_string(&ActorSource::ApiKey).unwrap();
    assert!(j.contains("api_key") || j.contains("ApiKey"));
}
