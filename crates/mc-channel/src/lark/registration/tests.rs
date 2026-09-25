//! `lark::registration` 的用例（写者 M7-14）。
//!
//! 三件事在这里被钉住：
//!
//! 1. **协议客户端**：`begin` / `poll` 的信封解析、默认值、错误分流（换云两个方向）；
//! 2. **会话状态机**：`begin → 轮询 → 终态` 的**三态**（成功 / 超时 / 失败）——
//!    `DoD` 的专属验收之一；
//! 3. **会话表的语义**：workspace 收窄、过期出队、`mark_terminal` 首次写入胜。
//!
//! 装置（[`FakeApi`] / [`FakePoster`] / [`FakeRegistrationStore`]）是 `pub(crate)` 的：
//! `backfill` 的用例复用 [`FakeApi`]，于是"替身的行为"只有一个定义点。

use super::*;
use crate::lark::http_client::resource::{DownloadedResource, DownloadedResourceStream};
use crate::lark::installation::Installation;
use crate::lark::params::{
    AddReactionParams, BindingPromptParams, DeleteReactionParams, DownloadResourceParams,
    ListMessagesParams, PatchCardParams, SendCardParams, SendMarkdownCardParams, SendTextParams,
};
use crate::lark::types::{BotInfo, LarkMessage};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone as _, Utc};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::lark::client::{ApiClient, ApiError, TokenCacheInvalidator};
use crate::lark::installation::{InstallationService, PersistOutcome};
use crate::lark::params::InstallationCredentials;
use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;

const KEY_BYTES: [u8; 32] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32,
];
const CLIENT_SECRET: &str = "cli_secret-DO-NOT-LOG";

fn id(n: u8) -> Id {
    Id(uuid::Uuid::from_bytes([n; 16]))
}

fn boxed() -> SecretBox {
    SecretBox::new(&KEY_BYTES).expect("32 字节密钥")
}

// =====================================================================
// 装置
// =====================================================================

/// 假传输：按请求体里的 `action` 给出预设响应（`poll` 按调用顺序推进，用完后重复最后一个）。
#[derive(Default)]
pub struct FakePoster {
    begins: Mutex<Vec<String>>,
    polls: Mutex<Vec<String>>,
    poll_cursor: AtomicUsize,
    pub seen_urls: Mutex<Vec<String>>,
    pub calls: AtomicUsize,
}

impl FakePoster {
    /// 固定一串 `poll` 响应体。
    pub fn with_polls(bodies: Vec<&str>) -> Self {
        let poster = Self::default();
        *poster.polls.lock().expect("锁") = bodies.into_iter().map(str::to_string).collect();
        *poster.begins.lock().expect("锁") = vec![begin_body(
            "dc_test",
            "https://accounts.feishu.cn/oauth/v1/qrcode?code=abc",
            1,
            3600,
        )];
        poster
    }

    /// 用一段自定义的 `begin` 响应体（测错误路径与缺省值）。
    pub fn with_begin(body: &str) -> Self {
        let poster = Self::default();
        *poster.begins.lock().expect("锁") = vec![body.to_string()];
        poster
    }
}

#[async_trait]
impl FormPoster for FakePoster {
    async fn post_form(&self, endpoint: &str, body: String) -> Result<Vec<u8>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen_urls
            .lock()
            .expect("锁")
            .push(endpoint.to_string());
        if body.contains("action=begin") {
            let begins = self.begins.lock().expect("锁");
            let body = begins.first().cloned().unwrap_or_default();
            return Ok(body.into_bytes());
        }
        let polls = self.polls.lock().expect("锁");
        if polls.is_empty() {
            return Ok(br#"{"error":"authorization_pending"}"#.to_vec());
        }
        let index = self.poll_cursor.fetch_add(1, Ordering::SeqCst);
        let body = polls[index.min(polls.len() - 1)].clone();
        Ok(body.into_bytes())
    }
}

/// 装配一段 `begin` 响应体。
#[must_use]
pub fn begin_body(device_code: &str, qr: &str, interval: u64, expires_in: u64) -> String {
    format!(
        r#"{{"device_code":"{device_code}","verification_uri_complete":"{qr}","interval":{interval},"expires_in":{expires_in}}}"#
    )
}

/// `poll` 终态成功的响应体。
#[must_use]
pub fn poll_success_body(open_id: &str) -> String {
    format!(
        r#"{{"client_id":"cli_new","client_secret":"{CLIENT_SECRET}","user_info":{{"open_id":"{open_id}"}}}}"#
    )
}

/// 假 `ApiClient`：只有 `get_bot_info` 有行为，其余全部回 `NotConfigured`
/// （本片不测它们，但**不**用 `unimplemented!`：一个 panicking 的替身会把
/// "哪条路不该被走到"变成一次崩溃而不是一条断言）。
#[derive(Default)]
pub struct FakeApi {
    pub configured: bool,
    pub bot_info: Mutex<Option<Result<BotInfo, ApiError>>>,
    pub invalidations: Mutex<Vec<String>>,
    pub bot_info_calls: AtomicUsize,
}

impl FakeApi {
    /// 一个"够得着 Lark"的替身：`is_configured() == true` 且 `get_bot_info` 回固定值。
    pub fn serving(open_id: &str, union_id: &str) -> Self {
        Self {
            configured: true,
            bot_info: Mutex::new(Some(Ok(BotInfo {
                open_id: OpenId::new(open_id),
                union_id: union_id.to_string(),
            }))),
            invalidations: Mutex::new(Vec::new()),
            bot_info_calls: AtomicUsize::new(0),
        }
    }

    /// 够得着 Lark，但 `get_bot_info` 失败。
    pub fn failing() -> Self {
        Self {
            configured: true,
            bot_info: Mutex::new(Some(Err(ApiError::Transport { op: "get_bot_info" }))),
            invalidations: Mutex::new(Vec::new()),
            bot_info_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ApiClient for FakeApi {
    fn is_configured(&self) -> bool {
        self.configured
    }

    async fn send_interactive_card(&self, _params: SendCardParams) -> Result<String, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn patch_interactive_card(&self, _params: PatchCardParams) -> Result<(), ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn send_text_message(&self, _params: SendTextParams) -> Result<String, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn send_markdown_card(
        &self,
        _params: SendMarkdownCardParams,
    ) -> Result<String, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn send_binding_prompt_card(&self, _params: BindingPromptParams) -> Result<(), ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn get_bot_info(
        &self,
        _credentials: InstallationCredentials,
    ) -> Result<BotInfo, ApiError> {
        self.bot_info_calls.fetch_add(1, Ordering::SeqCst);
        self.bot_info
            .lock()
            .expect("锁")
            .clone()
            .unwrap_or(Err(ApiError::NotConfigured))
    }

    async fn get_message(
        &self,
        _credentials: InstallationCredentials,
        _message_id: &str,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn list_chat_messages(
        &self,
        _credentials: InstallationCredentials,
        _params: ListMessagesParams,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn download_message_resource(
        &self,
        _credentials: InstallationCredentials,
        _params: DownloadResourceParams,
    ) -> Result<DownloadedResource, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn download_message_resource_stream(
        &self,
        _credentials: InstallationCredentials,
        _params: DownloadResourceParams,
    ) -> Result<DownloadedResourceStream, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn batch_get_users(
        &self,
        _credentials: InstallationCredentials,
        _open_ids: Vec<String>,
    ) -> Result<HashMap<String, String>, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn add_message_reaction(&self, _params: AddReactionParams) -> Result<String, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn delete_message_reaction(&self, _params: DeleteReactionParams) -> Result<(), ApiError> {
        Err(ApiError::NotConfigured)
    }
}

impl TokenCacheInvalidator for FakeApi {
    fn invalidate_token_cache(&self, app_id: &str) {
        self.invalidations
            .lock()
            .expect("锁")
            .push(app_id.to_string());
    }
}

/// 假落库口：记账 `commit_install` 的入参，判决可预设。
#[derive(Default)]
pub struct FakeRegistrationStore {
    pub agent_name: Option<String>,
    /// 预设判决（`None` = 走默认的 `Stored`）。
    pub verdict: Mutex<Option<PersistVerdict>>,
    pub commits: Mutex<Vec<CommitInstall>>,
}

/// 预设判决的形状（`PersistOutcome` 里带 `Box<Installation>` ⇒ 不 `Clone`）。
pub enum PersistVerdict {
    /// 冲突（带分类）。
    Conflict(crate::lark::installation::InstallError),
    /// 存储层失败。
    Failing(String),
}

impl FakeRegistrationStore {
    pub fn with_agent(name: &str) -> Self {
        Self {
            agent_name: Some(name.to_string()),
            ..Self::default()
        }
    }

    pub fn stored_row() -> Installation {
        Installation {
            id: id(7),
            workspace_id: id(2),
            agent_id: id(3),
            app_id: "cli_new".to_string(),
            app_secret_encrypted: vec![0xBB; 30],
            tenant_key: None,
            bot_open_id: OpenId::new("ou_bot"),
            bot_union_id: Some("on_bot".to_string()),
            region: Region::Feishu,
            installer_user_id: id(4),
            status: "active".to_string(),
            installed_at: Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间"),
            created_at: Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间"),
            updated_at: Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间"),
        }
    }
}

#[async_trait]
impl RegistrationStore for FakeRegistrationStore {
    async fn agent_name_in_workspace(
        &self,
        _workspace_id: Id,
        _agent_id: Id,
    ) -> Result<Option<String>, String> {
        Ok(self.agent_name.clone())
    }

    async fn commit_install(&self, params: &CommitInstall) -> Result<PersistOutcome, String> {
        self.commits.lock().expect("锁").push(params.clone());
        match self.verdict.lock().expect("锁").take() {
            None => Ok(PersistOutcome::Stored(Box::new(Self::stored_row()))),
            Some(PersistVerdict::Conflict(error)) => Ok(PersistOutcome::Conflict(error)),
            Some(PersistVerdict::Failing(message)) => Err(message),
        }
    }
}

/// 可推进的假时钟。
#[derive(Default)]
pub struct FakeClock {
    offset: AtomicI64,
}

impl FakeClock {
    pub fn arc(self) -> Arc<dyn Fn() -> DateTime<Utc> + Send + Sync> {
        let base = Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间");
        let offset = Arc::new(self.offset);
        Arc::new(move || base + chrono::Duration::seconds(offset.load(Ordering::SeqCst)))
    }
}

/// 一个装了假端口的服务 + 它的部件（用例要分别断言每一件）。
struct Harness {
    service: Arc<RegistrationService>,
    poster: Arc<FakePoster>,
    api: Arc<FakeApi>,
    store: Arc<FakeRegistrationStore>,
    sessions: Arc<MemoryInstallSessionStore>,
}

fn harness(poster: FakePoster, api: FakeApi, store: FakeRegistrationStore) -> Harness {
    harness_with(
        poster,
        api,
        store,
        RegistrationServiceConfig {
            now: FakeClock::default().arc(),
            ..RegistrationServiceConfig::default()
        },
    )
}

fn harness_with(
    poster: FakePoster,
    api: FakeApi,
    store: FakeRegistrationStore,
    config: RegistrationServiceConfig,
) -> Harness {
    let poster = Arc::new(poster);
    let api = Arc::new(api);
    let store = Arc::new(store);
    let sessions = Arc::new(MemoryInstallSessionStore::with_clock(config.now.clone()));
    let client = RegistrationClient::new(RegistrationConfig::default(), poster.clone());
    let installs = Arc::new(InstallationService::new(
        Arc::new(crate::lark::installation::tests::MemoryStore::default()),
        boxed(),
    ));
    let service = Arc::new(RegistrationService::new(
        config,
        client,
        api.clone(),
        Some(Arc::clone(&api) as Arc<dyn TokenCacheInvalidator + Send + Sync>),
        store.clone(),
        installs,
        sessions.clone(),
    ));
    Harness {
        service,
        poster,
        api,
        store,
        sessions,
    }
}

// =====================================================================
// 子模块（门 ⑩ 的单文件 800 行硬限）
// =====================================================================

/// 协议客户端 / 会话表 / 三态状态机的断言。
mod state_machine;
