//! `install.rs` 的用例（不依赖数据库、不依赖网络）。
//!
//! 四类：① 空凭据的两条 400；② 活校验失败 ⇒ 400（**不是** 500）；
//! ③ 三类冲突 409 与存储/加密故障 500；④ **凭据纪律**：落库的 config 是 `secretbox` 密文
//! （反例：明文入库即失败）+ `Debug` 不回显 `AppSecret`。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::dingtalk::config::{decode_ciphertext, FIELD_APP_SECRET_ENCRYPTED};
use crate::dingtalk::outbound::openapi::DingTalkApiError;

// =====================================================================
// 替身
// =====================================================================

/// 一个可编程的存储替身。
#[derive(Default)]
struct FakeStore {
    inner: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    listed: Vec<InstallRecord>,
    persisted: Vec<PersistInstall>,
    outcome: Option<PersistOutcome>,
    revoke_result: bool,
    failure: bool,
}

impl FakeStore {
    fn new(outcome: PersistOutcome) -> Self {
        let store = Self::default();
        store.inner.lock().expect("lock").outcome = Some(outcome);
        store
    }

    fn persisted(&self) -> Vec<PersistInstall> {
        self.inner.lock().expect("lock").persisted.clone()
    }
}

#[async_trait]
impl InstallStore for FakeStore {
    async fn list_by_workspace(&self, _workspace_id: Id) -> Result<Vec<InstallRecord>, String> {
        let state = self.inner.lock().expect("lock");
        if state.failure {
            return Err("boom".to_string());
        }
        Ok(state.listed.clone())
    }

    async fn get_in_workspace(
        &self,
        installation_id: Id,
        _workspace_id: Id,
    ) -> Result<Option<InstallRecord>, String> {
        let state = self.inner.lock().expect("lock");
        if state.failure {
            return Err("boom".to_string());
        }
        Ok(state
            .listed
            .iter()
            .find(|record| record.id == installation_id)
            .cloned())
    }

    async fn revoke(&self, _workspace_id: Id, _installation_id: Id) -> Result<bool, String> {
        let state = self.inner.lock().expect("lock");
        if state.failure {
            return Err("boom".to_string());
        }
        Ok(state.revoke_result)
    }

    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String> {
        let mut state = self.inner.lock().expect("lock");
        if state.failure {
            return Err("boom".to_string());
        }
        state.persisted.push(params.clone());
        Ok(state.outcome.clone().expect("outcome"))
    }
}

/// 一个可编程的"铸令牌"替身。
struct FakeProbe {
    accept: bool,
}

#[async_trait]
impl CredentialProbe for FakeProbe {
    async fn fetch_access_token(
        &self,
        _app_key: &str,
        _app_secret: &AppSecret,
    ) -> Result<AccessToken, DingTalkApiError> {
        if self.accept {
            Ok(AccessToken {
                value: "minted".to_string(),
                expire_in: 7200,
            })
        } else {
            Err(DingTalkApiError::Http {
                path: "/v1.0/oauth2/accessToken",
                status: 401,
            })
        }
    }
}

/// 一个 `secretbox` 封装盒（32 字节 key，用例专用）。
fn boxed() -> SecretBox {
    SecretBox::new(&[9u8; 32]).expect("box")
}

fn record(workspace_id: Id, agent_id: Id) -> InstallRecord {
    InstallRecord {
        id: Id::new(),
        workspace_id,
        agent_id,
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: json!({}),
        installed_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn service(store: Arc<FakeStore>, accept: bool) -> InstallService {
    InstallService::new(store, Arc::new(FakeProbe { accept }), boxed())
}

// =====================================================================
// 形状与活校验
// =====================================================================

#[tokio::test]
async fn empty_credentials_are_rejected_before_any_network_call() {
    let store = Arc::new(FakeStore::default());
    let service = service(Arc::clone(&store), false);
    let workspace_id = Id::new();
    let agent_id = Id::new();
    let user_id = Id::new();

    let empty_key = RegisterByoParams::new(workspace_id, agent_id, user_id, "   ", "secret");
    let error = service.register_byo(&empty_key).await.expect_err("key");
    assert_eq!(error, InstallError::InvalidAppKey);
    assert_eq!(error.http_status(), 400);

    let empty_secret = RegisterByoParams::new(workspace_id, agent_id, user_id, "dingkey", "");
    let error = service
        .register_byo(&empty_secret)
        .await
        .expect_err("secret");
    assert_eq!(error, InstallError::InvalidAppSecret);
    assert_eq!(error.http_status(), 400);

    // 两条都在铸令牌**之前**返回 ⇒ 替身一次都没被调用、也没落库。
    assert!(store.persisted().is_empty());
}

#[tokio::test]
async fn a_refused_mint_is_a_user_error_not_a_server_error() {
    let store = Arc::new(FakeStore::default());
    let service = service(Arc::clone(&store), false);
    let params = RegisterByoParams::new(Id::new(), Id::new(), Id::new(), "dingkey", "bad-secret");
    let error = service.register_byo(&params).await.expect_err("rejected");
    assert_eq!(error, InstallError::CredentialValidation);
    assert_eq!(error.http_status(), 400, "上游逐字：这是用户错误");
    assert!(store.persisted().is_empty());
}

// =====================================================================
// 落库：密文（**反例：明文入库即失败**）
// =====================================================================

#[tokio::test]
async fn the_stored_config_carries_secretbox_ciphertext_not_plaintext() {
    let workspace_id = Id::new();
    let agent_id = Id::new();
    let stored = record(workspace_id, agent_id);
    let store = Arc::new(FakeStore::new(PersistOutcome::Stored(Box::new(stored))));
    let service = service(Arc::clone(&store), true);

    let plaintext = "very-secret-app-secret";
    let params = RegisterByoParams::new(workspace_id, agent_id, Id::new(), "dingkey", plaintext);
    let record = service.register_byo(&params).await.expect("installed");

    let persisted = store.persisted();
    assert_eq!(persisted.len(), 1);
    let config = &persisted[0].config;
    assert_eq!(persisted[0].app_id, "dingkey");
    assert_eq!(config["app_id"], json!("dingkey"));
    assert_eq!(config["robot_code"], json!("dingkey"));

    // ① **明文不在 config 里**（任何一层：整段 JSON、单个键的值）。
    let rendered = config.to_string();
    assert!(!rendered.contains(plaintext), "{rendered}");

    // ② 密文列**是** `secretbox` 的单块字节（能被同一个盒子打开），不是明文、不是哈希。
    let sealed = config[FIELD_APP_SECRET_ENCRYPTED]
        .as_str()
        .expect("ciphertext column");
    let bytes = decode_ciphertext(sealed).expect("base64");
    assert_ne!(bytes, plaintext.as_bytes());
    let opened = boxed().open(&bytes).expect("secretbox round trip");
    assert_eq!(opened, plaintext.as_bytes(), "只有封装盒能还原它");

    // ③ 响应记录里的 `config` 也不进 `Debug`。
    assert!(!format!("{record:?}").contains(plaintext));
}

#[tokio::test]
async fn the_second_install_of_the_same_robot_reuses_the_row_in_place() {
    // 上游逐字：再连**同一个**机器人是原地更新那一行（`ON CONFLICT … DO UPDATE`）。
    // 端口把这件事表达成 `Stored`，而且**不**新增第二个 `app_id`。
    let workspace_id = Id::new();
    let agent_id = Id::new();
    let existing = record(workspace_id, agent_id);
    let store = Arc::new(FakeStore::new(PersistOutcome::Stored(Box::new(
        existing.clone(),
    ))));
    let service = service(Arc::clone(&store), true);

    let first = RegisterByoParams::new(workspace_id, agent_id, Id::new(), "dingkey", "s1");
    let second = RegisterByoParams::new(workspace_id, agent_id, Id::new(), "dingkey", "s2");
    let a = service.register_byo(&first).await.expect("first");
    let b = service.register_byo(&second).await.expect("second");
    assert_eq!(a.id, b.id, "同一行被原地更新");
    let persisted = store.persisted();
    assert_eq!(persisted.len(), 2);
    assert_eq!(persisted[0].app_id, persisted[1].app_id);
}

#[tokio::test]
async fn the_three_conflict_outcomes_map_to_their_own_sentinels() {
    let workspace_id = Id::new();
    let agent_id = Id::new();
    let cases = [
        (
            PersistOutcome::OwnedByAnotherWorkspace,
            InstallError::OwnedByAnotherWorkspace,
            "dingtalk_robot_owned_by_another_workspace",
        ),
        (
            PersistOutcome::OwnedBySameWorkspace,
            InstallError::OwnedBySameWorkspace,
            "dingtalk_robot_owned_by_same_workspace",
        ),
        (
            PersistOutcome::OwnedByArchivedAgent,
            InstallError::OwnedByArchivedAgent,
            "dingtalk_robot_owned_by_archived_agent",
        ),
    ];
    for (outcome, expected, code) in cases {
        let store = Arc::new(FakeStore::new(outcome));
        let service = service(Arc::clone(&store), true);
        let params = RegisterByoParams::new(workspace_id, agent_id, Id::new(), "dingkey", "s");
        let error = service.register_byo(&params).await.expect_err("conflict");
        assert_eq!(error, expected);
        assert_eq!(error.http_status(), 409);
        assert_eq!(error.code(), code);
    }
}

#[tokio::test]
async fn store_failures_are_opaque_and_500() {
    let store = Arc::new(FakeStore::default());
    store.inner.lock().expect("lock").failure = true;
    let service = service(Arc::clone(&store), true);
    let params = RegisterByoParams::new(Id::new(), Id::new(), Id::new(), "dingkey", "plain-secret");
    let error = service.register_byo(&params).await.expect_err("store");
    assert_eq!(error.http_status(), 500);
    let rendered = error.to_string();
    assert!(!rendered.contains("plain-secret"), "{rendered}");
    assert!(!rendered.contains("dingkey"), "{rendered}");
}

// =====================================================================
// 读面
// =====================================================================

#[tokio::test]
async fn workspace_scoped_reads_report_not_found_instead_of_leaking() {
    let store = Arc::new(FakeStore::default());
    let service = service(Arc::clone(&store), true);
    let error = service
        .get_in_workspace(Id::new(), Id::new())
        .await
        .expect_err("missing");
    assert_eq!(error, InstallError::NotFound);
    assert_eq!(error.http_status(), 404);
    assert!(!service.revoke(Id::new(), Id::new()).await.expect("revoke"));
}

#[test]
fn register_params_debug_redacts_the_pasted_secret() {
    let params = RegisterByoParams::new(Id::new(), Id::new(), Id::new(), "dingkey", "shh-secret");
    let rendered = format!("{params:?}");
    assert!(!rendered.contains("shh-secret"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(rendered.contains("dingkey"), "AppKey 不是秘密：{rendered}");
}

#[test]
fn install_record_public_config_exposes_only_the_non_secret_columns() {
    let workspace_id = Id::new();
    let agent_id = Id::new();
    let mut record = record(workspace_id, agent_id);
    record.config = json!({
        "app_id": "dingkey",
        "robot_code": "dingkey",
        FIELD_APP_SECRET_ENCRYPTED: "Y2lwaGVydGV4dA==",
    });
    assert!(record.is_active());
    let public = record.public_config();
    assert_eq!(public.app_id, "dingkey");
    assert_eq!(public.robot_code, "dingkey");
    assert!(!format!("{public:?}").contains("Y2lwaGVydGV4dA=="));
}
