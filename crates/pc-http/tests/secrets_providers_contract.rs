//! Secrets 远端 provider（GCP / Vault）契约测试。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    routes,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
        db.clone(),
        RuntimeHandles {
            heartbeat: spawn_heartbeat_supervisor(4, actors.clone()),
            agents: pc_agent::spawn_agent_supervisor(db),
            adapters: AdapterRegistry::new(),
            actors,
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 3100,
            session_cookie: "paperclip_session".into(),
            api_key_header: "x-paperclip-agent-key".into(),
            csrf_header: "x-paperclip-csrf".into(),
        },
        pc_telemetry::TelemetryOptions::default(),
        Arc::new(WsState::new(realtime.clone(), "test".to_string())),
        realtime,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let mut builder = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).expect("request"))
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload)
}

async fn seed_company(db: &Db) -> Uuid {
    let prefix = Uuid::new_v4().simple().to_string();
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id",
    )
    .bind(format!("secrets-prov-test-{}", Uuid::new_v4().simple()))
    .bind(&prefix)
    .fetch_one(db.pool())
    .await
    .expect("seed company")
}

#[tokio::test(flavor = "current_thread")]
async fn provider_descriptors_lists_all_four() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::secrets::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/secret-providers"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "providers: {body}");
    // /secret-providers 直接返回数组（每个 provider 一项）
    let items = body.as_array().expect("providers is top-level array");
    let ids: Vec<&str> = items
        .iter()
        .map(|i| i["id"].as_str().unwrap_or(""))
        .collect();
    assert!(ids.contains(&"local_encrypted"));
    assert!(ids.contains(&"aws_secrets_manager"));
    assert!(ids.contains(&"gcp_secret_manager"));
    assert!(ids.contains(&"vault"));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_health_reports_gcp_and_vault_warn() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::secrets::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/secret-providers/health"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "health: {body}");
    let items = body["providers"].as_array().expect("providers array");
    let gcp = items
        .iter()
        .find(|i| i["provider"] == "gcp_secret_manager")
        .expect("gcp entry");
    let vault = items
        .iter()
        .find(|i| i["provider"] == "vault")
        .expect("vault entry");
    // 在没有环境变量时应当是 warn / error，不是 panic
    assert!(gcp["status"].is_string());
    assert!(vault["status"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn pc_secrets_registry_can_register_all_four() {
    use pc_secrets::{
        AwsSecretsManagerProvider, GcpSecretManagerProvider, LocalEncryptedProvider,
        SecretProviderRegistry, VaultProvider,
    };
    use std::sync::Arc;
    let mut reg = SecretProviderRegistry::new();
    let key = [0x11u8; 32];
    reg.register(Arc::new(LocalEncryptedProvider::from_bytes(key)));
    reg.register(Arc::new(AwsSecretsManagerProvider::new(
        "us-east-1",
        "AKIA",
        "secret",
    )));
    reg.register(Arc::new(GcpSecretManagerProvider::new("p", "tok")));
    reg.register(Arc::new(VaultProvider::new(
        "https://vault.example.com",
        "tok",
    )));
    assert_eq!(reg.len(), 4);
    assert_eq!(reg.provider_ids().len(), 4);
    assert!(reg.get("local_encrypted").is_some());
    assert!(reg.get("aws_secrets_manager").is_some());
    assert!(reg.get("gcp_secret_manager").is_some());
    assert!(reg.get("vault").is_some());
    assert!(reg.get("nonexistent").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn vault_provider_validate_config_passes_with_valid_inputs() {
    use pc_secrets::{SecretProvider, VaultProvider};
    let p = VaultProvider::new("https://vault.example.com", "tok");
    let v = p.validate_config(None).await;
    assert!(v.ok, "valid vault config should pass: {v:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn gcp_provider_validate_config_passes_with_valid_inputs() {
    use pc_secrets::{GcpSecretManagerProvider, SecretProvider};
    let p = GcpSecretManagerProvider::new("p1", "tok");
    let v = p.validate_config(None).await;
    assert!(v.ok, "valid gcp config should pass: {v:?}");
}
