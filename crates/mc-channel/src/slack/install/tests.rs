//! `slack::install` 的用例（写者 M7-4）。
//!
//! 三条上游校验逐条钉住：**`xoxb-` 前缀**、**`xapp-` 里的 app id**、
//! **两个令牌同属一个 app**（`auth.test` → `bots.info` → 比对）。外加：
//! 明文**永不**入库（落库的是 `secretbox` 密文的 base64）、
//! 冲突三类各自映射到 409、以及「错误路径不回显凭据」（`docs/60` §2.3 判据 3）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;
use serde_json::Value;

use super::*;
use crate::slack::outbound::{ApiResult, SlackApiError};
use base64::Engine as _;

const KEY: [u8; 32] = [3u8; 32];

/// `xapp-` 形态的测试令牌（第三段是 app id）。
fn app_token(app_id: &str) -> String {
    format!("xapp-1-{app_id}-itest-not-a-real-token")
}

/// 一份"看起来像真的"的 bot token。
const BOT_TOKEN: &str = "xoxb-not-a-real-token-itest-only";

#[derive(Default)]
struct FakeApi {
    /// `auth.test` 的返回；`None` ⇒ 失败。
    auth: Mutex<Option<AuthTest>>,
    auth_error: Mutex<Option<SlackApiError>>,
    /// `bots.info` 的 app id。
    bot_app_id: Mutex<Option<String>>,
    /// `apps.connections.open` 是否成功。
    app_token_ok: Mutex<bool>,
    /// 收到的调用序列（断言顺序用）。
    calls: Mutex<Vec<&'static str>>,
}

impl FakeApi {
    fn consistent(app_id: &str) -> Self {
        Self {
            auth: Mutex::new(Some(AuthTest {
                team_id: "T1".to_string(),
                user_id: "UBOT".to_string(),
                bot_id: "B1".to_string(),
            })),
            bot_app_id: Mutex::new(Some(app_id.to_string())),
            app_token_ok: Mutex::new(true),
            ..Self::default()
        }
    }
}

#[async_trait]
impl InstallApi for FakeApi {
    async fn auth_test(&self, _bot_token: &str) -> ApiResult<AuthTest> {
        self.calls.lock().expect("lock").push("auth.test");
        if let Some(error) = self.auth_error.lock().expect("lock").clone() {
            return Err(error);
        }
        self.auth
            .lock()
            .expect("lock")
            .clone()
            .ok_or(SlackApiError::Malformed {
                method: "auth.test",
            })
    }

    async fn bot_app_id(&self, _bot_token: &str, _bot_id: &str) -> ApiResult<String> {
        self.calls.lock().expect("lock").push("bots.info");
        self.bot_app_id
            .lock()
            .expect("lock")
            .clone()
            .ok_or(SlackApiError::Malformed {
                method: "bots.info",
            })
    }

    async fn validate_app_token(&self, _app_token: &str) -> ApiResult<()> {
        self.calls
            .lock()
            .expect("lock")
            .push("apps.connections.open");
        if *self.app_token_ok.lock().expect("lock") {
            Ok(())
        } else {
            Err(SlackApiError::Refused {
                method: "apps.connections.open",
                code: "invalid_auth".to_string(),
            })
        }
    }
}

/// 内存存储替身（`persist` 的语义照上游：回收死主 → upsert → 冲突分类）。
#[derive(Default)]
struct FakeStore {
    rows: Mutex<Vec<InstallRecord>>,
    /// app id → 它是否已被**归档**的 agent 占着。
    archived: Mutex<Vec<String>>,
    fail: Mutex<bool>,
}

#[async_trait]
impl InstallStore for FakeStore {
    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<InstallRecord>, String> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|row| row.workspace_id == workspace_id)
            .cloned()
            .collect())
    }

    async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Option<InstallRecord>, String> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.id == installation_id && row.workspace_id == workspace_id)
            .cloned())
    }

    async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String> {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows
            .iter_mut()
            .find(|row| row.id == installation_id && row.workspace_id == workspace_id)
        else {
            return Ok(false);
        };
        if row.status == "revoked" {
            return Ok(false);
        }
        row.status = "revoked".to_string();
        Ok(true)
    }

    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String> {
        if *self.fail.lock().expect("lock") {
            return Err("store down".to_string());
        }
        let mut rows = self.rows.lock().expect("lock");
        // 冲突：(channel_type, app_id) 已被**别的**行占着。
        if let Some(existing) = rows.iter().find(|row| {
            row.config.get("app_id").and_then(Value::as_str) == Some(params.app_id.as_str())
                && !(row.workspace_id == params.workspace_id && row.agent_id == params.agent_id)
        }) {
            if existing.workspace_id != params.workspace_id {
                return Ok(PersistOutcome::OwnedByAnotherWorkspace);
            }
            if self.archived.lock().expect("lock").contains(&params.app_id) {
                return Ok(PersistOutcome::OwnedByArchivedAgent);
            }
            return Ok(PersistOutcome::OwnedBySameWorkspace);
        }
        // 同一个 (workspace, agent) ⇒ 原地更新。
        if let Some(existing) = rows
            .iter_mut()
            .find(|row| row.workspace_id == params.workspace_id && row.agent_id == params.agent_id)
        {
            existing.config = params.config.clone();
            existing.status = "active".to_string();
            return Ok(PersistOutcome::Stored(Box::new(existing.clone())));
        }
        let record = InstallRecord {
            id: Id::new(),
            workspace_id: params.workspace_id,
            agent_id: params.agent_id,
            installer_user_id: params.installer_user_id,
            status: "active".to_string(),
            config: params.config.clone(),
            installed_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        rows.push(record.clone());
        Ok(PersistOutcome::Stored(Box::new(record)))
    }
}

fn service(store: Arc<FakeStore>, api: Arc<FakeApi>) -> InstallService {
    InstallService::new(store, api, SecretBox::new(&KEY).expect("32 字节密钥"))
}

fn params(workspace_id: Id, agent_id: Id, app_id: &str) -> RegisterByoParams {
    RegisterByoParams::new(
        workspace_id,
        agent_id,
        Id::new(),
        BOT_TOKEN,
        app_token(app_id),
    )
}

/// `parseSlackAppID` 逐条（上游 `TestParseSlackAppID` 的等价物）。
#[test]
fn app_id_is_the_third_dash_segment_and_must_start_with_a_capital_a() {
    assert_eq!(
        parse_slack_app_id("xapp-1-A0BCXGVCS7R-itest-abc").expect("ok"),
        "A0BCXGVCS7R"
    );
    for bad in [
        "",              // 空
        "xoxb-1-A1-2-3", // 前缀不对
        "xapp-1-T1-2-3", // 第三段不是 app id（不以 A 开头）
        "xapp-1--2-3",   // 第三段空
        "xapp-1",        // 段数不足
    ] {
        assert_eq!(
            parse_slack_app_id(bad).expect_err(bad),
            InstallError::InvalidAppToken,
            "{bad}"
        );
    }
}

/// bot token 前缀不对 ⇒ 立刻 400，**不**打任何 Slack API。
#[test]
fn a_malformed_bot_token_never_reaches_slack() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::consistent("A1"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let error = runtime
        .block_on(svc.register_byo(&RegisterByoParams::new(
            Id::new(),
            Id::new(),
            Id::new(),
            "not-a-bot-token",
            app_token("A1"),
        )))
        .expect_err("拒绝");
    assert_eq!(error, InstallError::InvalidBotToken);
    assert_eq!(error.http_status(), 400);
    assert!(api.calls.lock().expect("lock").is_empty());
}

/// 两个令牌属于**不同** app ⇒ 拒（上游 Niko review 那条）。
#[test]
fn mismatched_bot_and_app_tokens_are_refused() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    // app token 说是 A1，而 bots.info 说 bot 属于 A2。
    let api = Arc::new(FakeApi::consistent("A2"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let error = runtime
        .block_on(svc.register_byo(&params(Id::new(), Id::new(), "A1")))
        .expect_err("拒绝");
    assert_eq!(error, InstallError::TokenAppMismatch);
    assert_eq!(error.http_status(), 400);
    assert_eq!(
        api.calls.lock().expect("lock").as_slice(),
        ["auth.test", "bots.info"],
        "比对在 validate_app_token 之前"
    );
    assert!(store.rows.lock().expect("lock").is_empty());
}

/// 成功的 BYO：令牌**只以密文**落库，且解回来等于原文。
#[test]
fn a_successful_byo_stores_ciphertext_never_plaintext() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::consistent("A1"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let record = runtime
        .block_on(svc.register_byo(&params(Id::new(), Id::new(), "A1")))
        .expect("装上");
    assert!(record.is_active());

    let serialized = serde_json::to_string(&record.config).expect("serialize");
    assert!(
        !serialized.contains(BOT_TOKEN),
        "明文 bot token 绝不入库 / 绝不进 config"
    );
    assert!(!serialized.contains("xapp-"), "明文 app token 绝不入库");

    // 解回来等于原文（`secretbox` + base64 往返）。
    let sealed_bot = record
        .config
        .get("bot_token_encrypted")
        .and_then(Value::as_str)
        .expect("ciphertext column");
    let opened = SecretBox::new(&KEY)
        .expect("key")
        .open(
            &base64::engine::general_purpose::STANDARD
                .decode(sealed_bot)
                .expect("base64"),
        )
        .expect("open");
    assert_eq!(String::from_utf8(opened).expect("utf8"), BOT_TOKEN);

    // 上游的落库字段。
    assert_eq!(
        record.config.get("app_id").and_then(Value::as_str),
        Some("A1")
    );
    assert_eq!(
        record.config.get("team_id").and_then(Value::as_str),
        Some("T1")
    );
    assert_eq!(
        record.config.get("bot_user_id").and_then(Value::as_str),
        Some("UBOT")
    );
}

/// `InstallRecord` 的 `Debug` 不回显 config。
#[test]
fn the_record_debug_never_prints_the_encrypted_config() {
    let record = InstallRecord {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: serde_json::json!({
            "app_id": "A1",
            "bot_token_encrypted": "c2VjcmV0LWNpcGhlcnRleHQ=",
        }),
        installed_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let printed = format!("{record:?}");
    assert!(!printed.contains("c2VjcmV0LWNpcGhlcnRleHQ="));
    assert!(printed.contains("<redacted>"));
}

/// 三类冲突各自映射到 409（上游用三条不同的提示语）。
#[test]
fn ownership_conflicts_are_classified_not_blamed_on_the_wrong_party() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::consistent("A1"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let workspace = Id::new();
    let agent = Id::new();
    runtime
        .block_on(svc.register_byo(&params(workspace, agent, "A1")))
        .expect("第一行");

    // 同一个 workspace 的**另一个** agent。
    let error = runtime
        .block_on(svc.register_byo(&params(workspace, Id::new(), "A1")))
        .expect_err("冲突");
    assert_eq!(error, InstallError::OwnedBySameWorkspace);
    assert_eq!(error.http_status(), 409);

    // 归档的 agent。
    store.archived.lock().expect("lock").push("A1".to_string());
    let error = runtime
        .block_on(svc.register_byo(&params(workspace, Id::new(), "A1")))
        .expect_err("冲突");
    assert_eq!(error, InstallError::OwnedByArchivedAgent);
    assert_eq!(error.http_status(), 409);

    // 别的 workspace。
    let error = runtime
        .block_on(svc.register_byo(&params(Id::new(), Id::new(), "A1")))
        .expect_err("冲突");
    assert_eq!(error, InstallError::OwnedByAnotherWorkspace);
    assert_eq!(error.http_status(), 409);
}

/// 同一个 `(workspace, agent)` 重连 ⇒ **原地更新**，不新增行（上游 `persistInstall`）。
#[test]
fn reconnecting_the_same_agent_updates_in_place() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::consistent("A1"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let workspace = Id::new();
    let agent = Id::new();
    let first = runtime
        .block_on(svc.register_byo(&params(workspace, agent, "A1")))
        .expect("第一次");
    // 先撤销，再换一个新 app 重连（上游注释逐字：swapping it to a NEW Slack app）。
    assert!(runtime
        .block_on(svc.revoke(workspace, first.id))
        .expect("revoke"));
    let api2 = Arc::new(FakeApi::consistent("A2"));
    let svc2 = service(Arc::clone(&store), api2);
    let second = runtime
        .block_on(svc2.register_byo(&params(workspace, agent, "A2")))
        .expect("重连");
    assert_eq!(second.id, first.id, "行原地更新（id 不变）");
    assert!(second.is_active());
    assert_eq!(store.rows.lock().expect("lock").len(), 1);
}

/// 撤销语义：只有 `active → revoked` 那一次算"改了一行"。
#[test]
fn revoke_is_idempotent_and_preserves_the_row() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::consistent("A1"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let workspace = Id::new();
    let record = runtime
        .block_on(svc.register_byo(&params(workspace, Id::new(), "A1")))
        .expect("装上");
    assert!(runtime
        .block_on(svc.revoke(workspace, record.id))
        .expect("revoke"));
    assert!(!runtime
        .block_on(svc.revoke(workspace, record.id))
        .expect("no-op"));
    let after = runtime
        .block_on(svc.get_in_workspace(record.id, workspace))
        .expect("行仍在");
    assert_eq!(after.status, "revoked");
    assert!(!after.is_active());
}

/// 跨 workspace 读 = 与不存在同一结果（越权不泄露存在性）。
#[test]
fn reading_across_workspaces_is_not_found() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::consistent("A1"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let workspace = Id::new();
    let record = runtime
        .block_on(svc.register_byo(&params(workspace, Id::new(), "A1")))
        .expect("装上");
    let error = runtime
        .block_on(svc.get_in_workspace(record.id, Id::new()))
        .expect_err("越权");
    assert_eq!(error, InstallError::NotFound);
    assert_eq!(error.http_status(), 404);
}

/// 列表含 revoked（上游 `ListChannelInstallationsByWorkspace`）。
#[test]
fn the_workspace_list_includes_revoked_rows() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi::consistent("A1"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let workspace = Id::new();
    let record = runtime
        .block_on(svc.register_byo(&params(workspace, Id::new(), "A1")))
        .expect("装上");
    runtime
        .block_on(svc.revoke(workspace, record.id))
        .expect("revoke");
    let rows = runtime.block_on(svc.list(workspace)).expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "revoked");
}

/// 错误路径**不回显**任何凭据（`docs/60` §2.3 判据 3）。
#[test]
fn api_errors_never_echo_the_pasted_tokens() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi {
        auth_error: Mutex::new(Some(SlackApiError::Refused {
            method: "auth.test",
            code: "invalid_auth".to_string(),
        })),
        ..FakeApi::consistent("A1")
    });
    let svc = service(Arc::clone(&store), api);
    let params = params(Id::new(), Id::new(), "A1");
    let printed = format!("{params:?}");
    assert!(!printed.contains(BOT_TOKEN), "Debug 脱敏");
    assert!(!printed.contains("xapp-"), "Debug 脱敏");
    let error = runtime
        .block_on(svc.register_byo(&params))
        .expect_err("拒绝");
    let text = format!("{error} {error:?}");
    assert!(!text.contains(BOT_TOKEN));
    assert!(!text.contains("xapp-"));
    assert!(text.contains("invalid_auth"), "只带 Slack 自己的错误码");
}

/// 存储故障 ⇒ 不透明错误 + 500。
#[test]
fn store_failures_are_opaque_and_map_to_500() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    *store.fail.lock().expect("lock") = true;
    let api = Arc::new(FakeApi::consistent("A1"));
    let svc = service(Arc::clone(&store), Arc::clone(&api));
    let error = runtime
        .block_on(svc.register_byo(&params(Id::new(), Id::new(), "A1")))
        .expect_err("失败");
    assert_eq!(error.code(), "slack_store_error");
    assert_eq!(error.http_status(), 500);
}

/// app token 开不了连接 ⇒ 拒（不留一个"永远收不到事件"的行）。
#[test]
fn an_app_token_that_cannot_open_a_socket_is_refused() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let api = Arc::new(FakeApi {
        app_token_ok: Mutex::new(false),
        ..FakeApi::consistent("A1")
    });
    let svc = service(Arc::clone(&store), api);
    let error = runtime
        .block_on(svc.register_byo(&params(Id::new(), Id::new(), "A1")))
        .expect_err("拒绝");
    assert!(matches!(
        error,
        InstallError::Api {
            step: "apps.connections.open",
            ..
        }
    ));
    assert!(store.rows.lock().expect("lock").is_empty());
}
