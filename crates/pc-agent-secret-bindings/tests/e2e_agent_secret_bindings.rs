//! E2E tests for `pc-agent-secret-bindings`.
//!
//! 与 Node `server/src/services/agent-secret-bindings.ts` 1:1 对齐。

use std::sync::Mutex;

use async_trait::async_trait;
use pc_agent_secret_bindings::{
    collect_secret_refs, collect_user_secret_refs, sync_agent_adapter_env_bindings,
    sync_agent_adapter_env_bindings_fallback, BindingTarget, BindingTargetType, SecretBindingError,
    SecretBindingResult, SecretBindingSync, SecretProjectionClass, SecretRef, SecretVersionSelector,
    UserSecretRef,
};
use serde_json::{json, Value};

// ============================================================================
// Mock secrets service
// ============================================================================

#[derive(Default)]
struct MockSync {
    secret_refs: Mutex<Vec<(String, BindingTarget<'static>, Vec<SecretRef>)>>,
    user_decls: Mutex<Vec<(String, BindingTarget<'static>, Vec<UserSecretRef>)>>,
    env_bindings: Mutex<Vec<(String, BindingTarget<'static>, Value)>>,
}

#[async_trait]
impl SecretBindingSync for MockSync {
    async fn sync_secret_refs(
        &self,
        company_id: &str,
        target: BindingTarget<'_>,
        refs: &[SecretRef],
    ) -> SecretBindingResult<()> {
        self.secret_refs.lock().unwrap().push((
            company_id.to_string(),
            BindingTarget {
                target_type: target.target_type,
                target_id: Box::leak(target.target_id.to_string().into_boxed_str()),
            },
            refs.to_vec(),
        ));
        Ok(())
    }

    async fn sync_user_secret_declarations(
        &self,
        company_id: &str,
        target: BindingTarget<'_>,
        refs: &[UserSecretRef],
    ) -> SecretBindingResult<()> {
        self.user_decls.lock().unwrap().push((
            company_id.to_string(),
            BindingTarget {
                target_type: target.target_type,
                target_id: Box::leak(target.target_id.to_string().into_boxed_str()),
            },
            refs.to_vec(),
        ));
        Ok(())
    }

    async fn sync_env_bindings(
        &self,
        company_id: &str,
        target: BindingTarget<'_>,
        env_value: Value,
    ) -> SecretBindingResult<()> {
        self.env_bindings.lock().unwrap().push((
            company_id.to_string(),
            BindingTarget {
                target_type: target.target_type,
                target_id: Box::leak(target.target_id.to_string().into_boxed_str()),
            },
            env_value,
        ));
        Ok(())
    }
}

// ============================================================================
// Version / projection parsing
// ============================================================================

#[test]
fn r670_secret_ref_in_env_uses_env_dotted_config_path() {
    let config = json!({
        "env": {
            "API_TOKEN": {
                "type": "secret_ref",
                "secretId": "00000000-0000-0000-0000-000000000001",
                "version": "latest"
            }
        }
    });
    let refs = collect_secret_refs(&config);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].config_path, "env.API_TOKEN");
    assert_eq!(
        refs[0].secret_id,
        "00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(refs[0].version_selector, SecretVersionSelector::Latest);
}

#[test]
fn r670_secret_ref_in_top_level_uses_key_as_config_path() {
    let config = json!({
        "OPENAI_KEY": {
            "type": "secret_ref",
            "secretId": "00000000-0000-0000-0000-000000000002"
        }
    });
    let refs = collect_secret_refs(&config);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].config_path, "OPENAI_KEY");
    assert_eq!(
        refs[0].version_selector,
        SecretVersionSelector::Latest // 默认值
    );
}

#[test]
fn r670_secret_ref_with_explicit_version_number() {
    let config = json!({
        "env": {
            "TOKEN": {
                "type": "secret_ref",
                "secretId": "00000000-0000-0000-0000-000000000003",
                "version": 7
            }
        }
    });
    let refs = collect_secret_refs(&config);
    assert_eq!(refs[0].version_selector, SecretVersionSelector::Version(7));
}

#[test]
fn r670_secret_ref_with_projection_class_class3_static_lease() {
    let config = json!({
        "env": {
            "LEASE": {
                "type": "secret_ref",
                "secretId": "00000000-0000-0000-0000-000000000004",
                "projectionClass": "class_3_static_lease"
            }
        }
    });
    let refs = collect_secret_refs(&config);
    assert_eq!(
        refs[0].projection_class,
        Some(SecretProjectionClass::Class3StaticLease)
    );
}

#[test]
fn r670_secret_ref_with_projection_class_unclassified() {
    let config = json!({
        "env": {
            "TOKEN": {
                "type": "secret_ref",
                "secretId": "00000000-0000-0000-0000-000000000005",
                "projectionClass": "unclassified"
            }
        }
    });
    let refs = collect_secret_refs(&config);
    assert_eq!(
        refs[0].projection_class,
        Some(SecretProjectionClass::Unclassified)
    );
}

#[test]
fn r670_secret_ref_with_projection_allowlist_key() {
    let config = json!({
        "env": {
            "KEY": {
                "type": "secret_ref",
                "secretId": "00000000-0000-0000-0000-000000000006",
                "projectionAllowlistKey": "myAllowlist"
            }
        }
    });
    let refs = collect_secret_refs(&config);
    assert_eq!(
        refs[0].projection_allowlist_key.as_deref(),
        Some("myAllowlist")
    );
}

#[test]
fn r670_plain_string_or_plain_object_binding_is_skipped() {
    let config = json!({
        "env": {
            "PLAIN_STR": "literal-value",
            "PLAIN_OBJ": { "type": "plain", "value": "literal-value" },
            "REAL_SECRET": {
                "type": "secret_ref",
                "secretId": "00000000-0000-0000-0000-000000000007"
            }
        }
    });
    let refs = collect_secret_refs(&config);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].secret_id, "00000000-0000-0000-0000-000000000007");
}

#[test]
fn r670_secret_ref_missing_secret_id_is_skipped() {
    let config = json!({
        "env": {
            "BAD": { "type": "secret_ref" },
            "GOOD": {
                "type": "secret_ref",
                "secretId": "00000000-0000-0000-0000-000000000008"
            }
        }
    });
    let refs = collect_secret_refs(&config);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].config_path, "env.GOOD");
}

#[test]
fn r670_secret_ref_wrong_type_is_skipped() {
    let config = json!({
        "env": {
            "USER": {
                "type": "user_secret_ref",
                "key": "myKey"
            }
        }
    });
    let refs = collect_secret_refs(&config);
    assert!(refs.is_empty(), "user_secret_ref 不应被 collect_secret_refs 提取");
}

// ============================================================================
// User secret ref extraction
// ============================================================================

#[test]
fn r670_user_secret_ref_in_env_uses_env_key_and_dotted_path() {
    let config = json!({
        "env": {
            "USER_API_KEY": {
                "type": "user_secret_ref",
                "key": "apiKey",
                "required": false,
                "allowMissingOverride": true
            }
        }
    });
    let refs = collect_user_secret_refs(&config);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].definition_key, "apiKey");
    assert_eq!(refs[0].env_key, "USER_API_KEY");
    assert_eq!(refs[0].config_path, "env.USER_API_KEY");
    assert!(!refs[0].required);
    assert!(refs[0].allow_missing_override);
}

#[test]
fn r670_user_secret_ref_top_level_uses_key_for_both() {
    let config = json!({
        "USER_TOKEN": {
            "type": "user_secret_ref",
            "key": "userToken"
        }
    });
    let refs = collect_user_secret_refs(&config);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].definition_key, "userToken");
    assert_eq!(refs[0].env_key, "USER_TOKEN");
    assert_eq!(refs[0].config_path, "USER_TOKEN");
}

#[test]
fn r670_user_secret_ref_defaults_required_true_and_allow_missing_false() {
    let config = json!({
        "env": {
            "DEF": { "type": "user_secret_ref", "key": "k" }
        }
    });
    let refs = collect_user_secret_refs(&config);
    assert!(refs[0].required);
    assert!(!refs[0].allow_missing_override);
}

#[test]
fn r670_user_secret_ref_with_explicit_version_number() {
    let config = json!({
        "env": {
            "VER": {
                "type": "user_secret_ref",
                "key": "k",
                "version": 3
            }
        }
    });
    let refs = collect_user_secret_refs(&config);
    assert_eq!(refs[0].version_selector, SecretVersionSelector::Version(3));
}

#[test]
fn r670_secret_ref_and_user_secret_ref_are_isolated() {
    let config = json!({
        "env": {
            "S": { "type": "secret_ref", "secretId": "00000000-0000-0000-0000-000000000009" },
            "U": { "type": "user_secret_ref", "key": "userKey" }
        }
    });
    let s_refs = collect_secret_refs(&config);
    let u_refs = collect_user_secret_refs(&config);
    assert_eq!(s_refs.len(), 1);
    assert_eq!(u_refs.len(), 1);
    assert_eq!(s_refs[0].config_path, "env.S");
    assert_eq!(u_refs[0].config_path, "env.U");
}

// ============================================================================
// Non-object / null / array inputs
// ============================================================================

#[test]
fn r670_collect_returns_empty_for_null_config() {
    assert!(collect_secret_refs(&Value::Null).is_empty());
    assert!(collect_user_secret_refs(&Value::Null).is_empty());
}

#[test]
fn r670_collect_returns_empty_for_array_config() {
    let arr = json!([1, 2, 3]);
    assert!(collect_secret_refs(&arr).is_empty());
    assert!(collect_user_secret_refs(&arr).is_empty());
}

#[test]
fn r670_collect_returns_empty_for_string_config() {
    let s = json!("not an object");
    assert!(collect_secret_refs(&s).is_empty());
    assert!(collect_user_secret_refs(&s).is_empty());
}

// ============================================================================
// Service sync
// ============================================================================

#[tokio::test]
async fn r670_sync_dispatches_both_secret_refs_and_user_decls() {
    let config = json!({
        "env": {
            "API_TOKEN": {
                "type": "secret_ref",
                "secretId": "00000000-0000-0000-0000-000000000010"
            },
            "USER_API_KEY": {
                "type": "user_secret_ref",
                "key": "userApiKey"
            }
        },
        "STATIC_KEY": {
            "type": "secret_ref",
            "secretId": "00000000-0000-0000-0000-000000000011"
        }
    });
    let svc = MockSync::default();
    sync_agent_adapter_env_bindings(&svc, "company-1", "agent-1", &config)
        .await
        .unwrap();

    let s_calls = svc.secret_refs.lock().unwrap();
    assert_eq!(s_calls.len(), 1);
    assert_eq!(s_calls[0].0, "company-1");
    assert_eq!(s_calls[0].1.target_id, "agent-1");
    assert_eq!(s_calls[0].1.target_type, BindingTargetType::Agent);
    assert_eq!(s_calls[0].2.len(), 2, "secret_refs 应包含 env + top-level 两条");

    let u_calls = svc.user_decls.lock().unwrap();
    assert_eq!(u_calls.len(), 1);
    assert_eq!(u_calls[0].2.len(), 1, "user_decls 应包含 1 条 user_secret_ref");

    // env_bindings 在主流程下不应被调用
    assert!(svc.env_bindings.lock().unwrap().is_empty());
}

#[tokio::test]
async fn r670_sync_propagates_service_errors() {
    struct FailingSync;
    #[async_trait]
    impl SecretBindingSync for FailingSync {
        async fn sync_secret_refs(
            &self,
            _company_id: &str,
            _target: BindingTarget<'_>,
            _refs: &[SecretRef],
        ) -> SecretBindingResult<()> {
            Err(SecretBindingError::Service("boom".into()))
        }
        async fn sync_user_secret_declarations(
            &self,
            _company_id: &str,
            _target: BindingTarget<'_>,
            _refs: &[UserSecretRef],
        ) -> SecretBindingResult<()> {
            Ok(())
        }
        async fn sync_env_bindings(
            &self,
            _company_id: &str,
            _target: BindingTarget<'_>,
            _env_value: Value,
        ) -> SecretBindingResult<()> {
            Ok(())
        }
    }
    let config = json!({
        "env": {
            "S": { "type": "secret_ref", "secretId": "00000000-0000-0000-0000-000000000012" }
        }
    });
    let result = sync_agent_adapter_env_bindings(&FailingSync, "c", "a", &config).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn r670_fallback_dispatches_env_value() {
    let config = json!({
        "env": {
            "X": "literal",
            "Y": { "type": "plain", "value": "p" }
        }
    });
    let svc = MockSync::default();
    sync_agent_adapter_env_bindings_fallback(&svc, "company-1", "agent-1", &config)
        .await
        .unwrap();
    let calls = svc.env_bindings.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "company-1");
    assert_eq!(calls[0].1.target_id, "agent-1");
    assert_eq!(
        calls[0].2,
        json!({ "X": "literal", "Y": { "type": "plain", "value": "p" } })
    );
    // fallback 路径下不应触发 sync_secret_refs
    assert!(svc.secret_refs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn r670_fallback_with_no_env_uses_null() {
    let config = json!({ "STATIC_KEY": "value" });
    let svc = MockSync::default();
    sync_agent_adapter_env_bindings_fallback(&svc, "c", "a", &config)
        .await
        .unwrap();
    let calls = svc.env_bindings.lock().unwrap();
    assert_eq!(calls[0].2, Value::Null);
}
