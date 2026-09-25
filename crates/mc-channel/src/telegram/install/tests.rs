//! `telegram::install` 的用例（写者 M7-5）。
//!
//! 上游 `install_test.go` / `install_error_test.go` 的判决逐条移植：令牌形状、`getMe` 的两种
//! 失败分类、webhook 互斥、三类活主冲突、workspace 收窄，外加**凭据不回显**的反例。
//! 全部注入端口替身 ⇒ 不需要库、不需要网络。

use std::sync::Mutex;

use super::*;
use crate::telegram::api::{ApiResult, EditMessageText, SendMessage, WebhookInfo};
use crate::telegram::config::{decode_credentials, Decrypter};
use crate::telegram::inbound::{Message, Update, User};

const KEY: [u8; 32] = [
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
];

/// 一个贴进来的 bot token（**不是真令牌**：GitHub 的 push protection 会拦"形态像真令牌"
/// 的字面量，所以这里刻意用自述形态）。
const TOKEN: &str = "123456:not-a-real-token-itest-only";

fn boxed() -> SecretBox {
    SecretBox::new(&KEY).expect("32 字节密钥")
}

// ---------------------------------------------------------------------------
// 端口替身
// ---------------------------------------------------------------------------

/// 可脚本化的 [`TelegramApi`] 替身：只回答安装面用到的两个方法，并记下调用顺序。
#[derive(Default)]
struct FakeApi {
    me: Option<ApiResult<User>>,
    webhook: Option<ApiResult<WebhookInfo>>,
    calls: Mutex<Vec<&'static str>>,
}

impl FakeApi {
    fn with_me(me: ApiResult<User>) -> Self {
        Self {
            me: Some(me),
            webhook: Some(Ok(WebhookInfo::default())),
            ..Self::default()
        }
    }

    fn with_me_and_webhook(me: ApiResult<User>, webhook: ApiResult<WebhookInfo>) -> Self {
        Self {
            me: Some(me),
            webhook: Some(webhook),
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls").clone()
    }
}

/// 一个"形状合法、没有 webhook"的 bot（`getMe` 的 happy path）。
fn healthy_bot() -> User {
    User {
        id: 123_456,
        is_bot: true,
        first_name: "Acme".to_string(),
        username: "acme_bot".to_string(),
        ..User::default()
    }
}

#[async_trait]
impl TelegramApi for FakeApi {
    async fn get_me(&self, _bot_token: &str) -> ApiResult<User> {
        self.calls.lock().expect("calls").push("getMe");
        self.me
            .clone()
            .unwrap_or(Err(ApiError::Malformed { method: "getMe" }))
    }

    async fn get_webhook_info(&self, _bot_token: &str) -> ApiResult<WebhookInfo> {
        self.calls.lock().expect("calls").push("getWebhookInfo");
        self.webhook.clone().unwrap_or(Err(ApiError::Malformed {
            method: "getWebhookInfo",
        }))
    }

    async fn get_updates(&self, _bot_token: &str, _offset: i64) -> ApiResult<Vec<Update>> {
        Err(ApiError::Malformed {
            method: "getUpdates",
        })
    }

    async fn send_message(&self, _bot_token: &str, _params: &SendMessage) -> ApiResult<Message> {
        Err(ApiError::Malformed {
            method: "sendMessage",
        })
    }

    /// M7-6 补的端口方法：安装校验路径**不调用**它（本替身也就不用记它）。
    async fn edit_message_text(
        &self,
        _bot_token: &str,
        _params: &EditMessageText,
    ) -> ApiResult<()> {
        Err(ApiError::Malformed {
            method: "editMessageText",
        })
    }

    async fn send_chat_action(
        &self,
        _bot_token: &str,
        _chat_id: i64,
        _message_thread_id: i64,
    ) -> ApiResult<()> {
        Err(ApiError::Malformed {
            method: "sendChatAction",
        })
    }
}

/// 可脚本化的 [`InstallStore`] 替身。
#[derive(Default)]
struct FakeStore {
    persisted: Mutex<Option<PersistInstall>>,
    outcome: Option<PersistOutcome>,
    listed: Vec<InstallRecord>,
    single: Option<InstallRecord>,
    revoked: bool,
    failure: Option<String>,
}

/// 一行最小安装（用例的读面装置）。
fn record() -> InstallRecord {
    InstallRecord {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: serde_json::json!({
            "app_id": "123456",
            "bot_username": "acme_bot",
            "bot_token_encrypted": "CIPHERTEXT-DO-NOT-LOG",
        }),
        installed_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[async_trait]
impl InstallStore for FakeStore {
    async fn list_by_workspace(&self, _workspace_id: Id) -> Result<Vec<InstallRecord>, String> {
        if let Some(message) = &self.failure {
            return Err(message.clone());
        }
        Ok(self.listed.clone())
    }

    async fn get_in_workspace(
        &self,
        _installation_id: Id,
        _workspace_id: Id,
    ) -> Result<Option<InstallRecord>, String> {
        if let Some(message) = &self.failure {
            return Err(message.clone());
        }
        Ok(self.single.clone())
    }

    async fn revoke(&self, _workspace_id: Id, _installation_id: Id) -> Result<bool, String> {
        if let Some(message) = &self.failure {
            return Err(message.clone());
        }
        Ok(self.revoked)
    }

    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String> {
        *self.persisted.lock().expect("persisted") = Some(params.clone());
        if let Some(message) = &self.failure {
            return Err(message.clone());
        }
        Ok(self
            .outcome
            .clone()
            .unwrap_or_else(|| PersistOutcome::Stored(Box::new(record()))))
    }
}

/// 装好的服务（端口 + 一个乱序的 32 字节密钥盒）。
fn build_service(api: Arc<dyn TelegramApi>, store: Arc<FakeStore>) -> InstallService {
    InstallService::new(store, api, boxed())
}

/// 一个装好的安装入参。
fn params() -> RegisterParams {
    RegisterParams::new(Id::new(), Id::new(), Id::new(), TOKEN)
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 上游 `TestRegisterValidatesAndEncryptsBotToken`：密文入库、明文**不在** config 里，
/// 且密文能被同一个密钥盒解回原令牌。
#[tokio::test]
async fn register_validates_and_encrypts_the_bot_token() {
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::with_me(Ok(healthy_bot())));
    let installed = build_service(api.clone(), store.clone())
        .register(&params())
        .await
        .expect("register");
    assert!(installed.is_active());

    let persisted = store
        .persisted
        .lock()
        .expect("persisted")
        .clone()
        .expect("persist called");
    assert_eq!(persisted.app_id, "123456", "路由键 = 令牌前缀里的 bot id");
    assert_eq!(
        persisted.config["bot_username"], "acme_bot",
        "getMe 的用户名落进配置（@-mention 判定要用）"
    );
    let raw = serde_json::to_string(&persisted.config).expect("json");
    assert!(!raw.contains("not-a-real-token"), "明文令牌进库了：{raw}");
    assert!(!raw.contains(TOKEN), "明文令牌进库了：{raw}");

    let creds =
        decode_credentials(&persisted.config, &Decrypter::secret_box(boxed())).expect("decrypt");
    assert_eq!(creds.bot_token, TOKEN);
    assert_eq!(creds.bot_id, "123456");
    assert_eq!(creds.bot_username, "acme_bot");
    assert_eq!(api.calls(), vec!["getMe", "getWebhookInfo"]);
}

/// 上游 `TestRegisterRejectsMalformedTokenBeforeNetwork`：形状不对 ⇒ **一次网络都不打**。
#[tokio::test]
async fn register_rejects_a_malformed_token_before_any_network_call() {
    for bad in ["123456", ":abc", "123456:", "abc:def", ""] {
        let store = Arc::new(FakeStore::default());
        let api = Arc::new(FakeApi::with_me(Ok(healthy_bot())));
        let error = build_service(api.clone(), store.clone())
            .register(&RegisterParams::new(Id::new(), Id::new(), Id::new(), bad))
            .await
            .expect_err("rejected");
        assert_eq!(error, InstallError::InvalidBotToken, "{bad:?}");
        assert!(api.calls().is_empty(), "{bad:?} 不该打网络");
        assert!(store.persisted.lock().expect("persisted").is_none());
    }
}

/// 上游 `TestRegisterRefusesConfiguredWebhook`：长轮询与 webhook 互斥 ⇒ 拒，且**不**落库。
#[tokio::test]
async fn register_refuses_a_bot_with_an_outgoing_webhook() {
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::with_me_and_webhook(
        Ok(healthy_bot()),
        Ok(WebhookInfo {
            url: "https://example.test/telegram".to_string(),
            pending_update_count: 3,
        }),
    ));
    let error = build_service(api, store.clone())
        .register(&params())
        .await
        .expect_err("refused");
    assert_eq!(error, InstallError::WebhookConfigured);
    assert_eq!(error.http_status(), 400);
    assert!(store.persisted.lock().expect("persisted").is_none());
}

/// 上游 `TestClassifyCredentialVerificationError`：401 是**权威拒绝**（400），
/// 其余（够不着 / 坏响应 / 5xx）是**没有判决**（503），两者绝不能混。
#[tokio::test]
async fn a_rejection_and_an_outage_are_classified_apart() {
    let rejected = ApiError::Api {
        method: "getMe",
        code: 401,
        description: "Unauthorized".to_string(),
        retry_after: None,
    };
    assert_eq!(
        classify_credential_verification_error(&rejected),
        InstallError::CredentialsRejected
    );
    for undecidable in [
        ApiError::Transport { method: "getMe" },
        ApiError::Malformed { method: "getMe" },
        ApiError::Api {
            method: "getMe",
            code: 500,
            description: "Internal Server Error".to_string(),
            retry_after: None,
        },
    ] {
        assert_eq!(
            classify_credential_verification_error(&undecidable),
            InstallError::CredentialsUnverifiable,
            "{undecidable:?}"
        );
    }

    // 端到端：503 的那条路**不**落库，且文案明说"令牌没被保存"。
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::with_me(Err(ApiError::Transport {
        method: "getMe",
    })));
    let error = build_service(api, store.clone())
        .register(&params())
        .await
        .expect_err("unverifiable");
    assert_eq!(error, InstallError::CredentialsUnverifiable);
    assert_eq!(error.http_status(), 503);
    assert!(store.persisted.lock().expect("persisted").is_none());
}

/// 上游 `TestRegister` 的"响应不是带用户名的 bot"分支。
#[tokio::test]
async fn a_response_that_is_not_a_bot_with_a_username_is_a_rejection() {
    for me in [
        User {
            id: 1,
            is_bot: false,
            username: "acme".to_string(),
            ..User::default()
        },
        User {
            id: 1,
            is_bot: true,
            username: String::new(),
            ..User::default()
        },
    ] {
        let store = Arc::new(FakeStore::default());
        let api = Arc::new(FakeApi::with_me(Ok(me)));
        let error = build_service(api, store.clone())
            .register(&params())
            .await
            .expect_err("rejected");
        assert_eq!(error, InstallError::CredentialsRejected);
        assert_eq!(error.http_status(), 400);
        assert!(store.persisted.lock().expect("persisted").is_none());
    }
}

/// 上游 `TestLiveBotOwnerConflictIsClassified`：三类活主各自成哨兵，且都翻 409。
#[tokio::test]
async fn live_owner_conflicts_are_classified() {
    let cases = [
        (
            PersistOutcome::OwnedBySameWorkspace,
            InstallError::OwnedBySameWorkspace,
        ),
        (
            PersistOutcome::OwnedByArchivedAgent,
            InstallError::OwnedByArchivedAgent,
        ),
        (
            PersistOutcome::OwnedByAnotherWorkspace,
            InstallError::OwnedByAnotherWorkspace,
        ),
    ];
    for (outcome, expected) in cases {
        let store = Arc::new(FakeStore {
            outcome: Some(outcome),
            ..FakeStore::default()
        });
        let api = Arc::new(FakeApi::with_me(Ok(healthy_bot())));
        let error = build_service(api, store)
            .register(&params())
            .await
            .expect_err("conflict");
        assert_eq!(error, expected);
        assert_eq!(error.http_status(), 409);
    }
}

/// 上游 `TestInstallationManagementStaysWorkspaceAndChannelScoped` +
/// `TestGetInstallationMapsMissingRowWithoutLeakingScope`：越权读 = 与不存在同结果。
#[tokio::test]
async fn installation_management_is_workspace_scoped() {
    let workspace_id = Id::new();
    let row = record();
    let store = Arc::new(FakeStore {
        listed: vec![row.clone()],
        single: Some(row.clone()),
        revoked: true,
        ..FakeStore::default()
    });
    let api = Arc::new(FakeApi::default());
    let service = build_service(api, store.clone());

    assert_eq!(service.list(workspace_id).await.expect("list").len(), 1);
    let found = service
        .get_in_workspace(row.id, workspace_id)
        .await
        .expect("get");
    assert_eq!(found.id, row.id);
    assert!(service.revoke(workspace_id, row.id).await.expect("revoke"));

    // 另一行读不到 ⇒ NotFound（不是越权泄漏）。
    let empty = Arc::new(FakeStore::default());
    let service = build_service(Arc::new(FakeApi::default()), empty);
    assert_eq!(
        service
            .get_in_workspace(row.id, workspace_id)
            .await
            .expect_err("not found"),
        InstallError::NotFound
    );
    // 撤销没打到行 ⇒ false（不是错误）。
    assert!(!service.revoke(workspace_id, row.id).await.expect("revoke"));
}

/// 存储层故障是不透明的 500（不回显 SQL / 参数）。
#[tokio::test]
async fn store_failures_surface_as_opaque_500s() {
    let store = Arc::new(FakeStore {
        failure: Some("duplicate key value violates unique constraint \"x\"".to_string()),
        ..FakeStore::default()
    });
    let service = build_service(Arc::new(FakeApi::with_me(Ok(healthy_bot()))), store);
    let error = service.register(&params()).await.expect_err("store error");
    assert_eq!(error.http_status(), 500);
    assert!(matches!(error, InstallError::Store { .. }));
    assert_eq!(error.code(), "telegram_store_error");
}

/// `DoD` 第 6 条：**错误路径与 `Debug` 都不回显**粘贴进来的令牌。
#[test]
fn errors_and_debug_never_echo_the_pasted_token() {
    let cases = [
        InstallError::NotFound,
        InstallError::InvalidBotToken,
        InstallError::CredentialsRejected,
        InstallError::CredentialsUnverifiable,
        InstallError::OwnedByAnotherWorkspace,
        InstallError::OwnedBySameWorkspace,
        InstallError::OwnedByArchivedAgent,
        InstallError::WebhookConfigured,
        InstallError::Seal,
        InstallError::Encode,
        InstallError::Store {
            message: "boom".to_string(),
        },
    ];
    for error in cases {
        let text = format!("{error} {error:?}");
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!text.contains("not-a-real-token"), "{text}");
    }

    let params = params();
    let rendered = format!("{params:?}");
    assert!(!rendered.contains(TOKEN), "{rendered}");
    assert!(rendered.contains("<redacted>"));

    let mut row = record();
    row.config = serde_json::json!({ "app_id": "1", "bot_token_encrypted": "CIPHERTEXT" });
    let rendered = format!("{row:?}");
    assert!(!rendered.contains("CIPHERTEXT"), "{rendered}");
    assert!(rendered.contains("<redacted>"));
    assert_eq!(InstallRecord::public_config(&row).bot_id, "1");
}

/// 状态码逐条对齐上游 `handler/telegram.go` 的 switch（形态门之外的第二道防线）。
#[test]
fn statuses_match_the_upstream_switch() {
    let cases = [
        (InstallError::NotFound, 404),
        (InstallError::InvalidBotToken, 400),
        (InstallError::CredentialsRejected, 400),
        (InstallError::CredentialsUnverifiable, 503),
        (InstallError::OwnedBySameWorkspace, 409),
        (InstallError::OwnedByArchivedAgent, 409),
        (InstallError::OwnedByAnotherWorkspace, 409),
        (InstallError::WebhookConfigured, 400),
        (InstallError::Seal, 500),
        (InstallError::Encode, 500),
        (
            InstallError::Store {
                message: "x".to_string(),
            },
            500,
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.http_status(), expected, "{error:?}");
    }
    assert_eq!(kind(), ChannelKind::Telegram);
    assert_eq!(CHANNEL_TYPE, ChannelKind::Telegram.storage_str());
}
