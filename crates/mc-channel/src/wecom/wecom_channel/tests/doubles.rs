//! 端到端回路里的**端口替身**（**只替别的片的端口**，见 `round_trip.rs` 的模块文档表）。
//!
//! 拆出来是**门 ⑩**（800 行硬限）的要求：回路本体是断言链，替身是它们的脚手架，两者的读者
//! 也不同（一个读"断言了什么"，一个读"替身怎么造"）。
//!
//! 每一个替身都只替**本片写集之外**的那一面：会话 / 触发 / issue / 铸令牌 / 媒体。归一化、
//! 安装路由、身份绑定、去重、判决链、出站**全是真代码**。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::engine::resolvers::{
    AppendParams, AppendResult, Auditor, BindMediaParams, BindMediaResult, ChannelIssue,
    ChannelIssueOutcome, ChannelIssueParams, ChatRunParams, DropReason, EngineResult,
    EnsureSessionParams, IssueCreator, MediaResolver, ResolvedInstallation, RunTriggerer,
    SessionBinder, SessionReader, StartSessionParams, StartSessionResult, WorkspaceIdentity,
};
use crate::engine::session::DedupStore;
use crate::wecom::credentials::{
    CredentialsError, CredentialsResolver, InstallationCredentials, PlaintextSecret,
};
use crate::wecom::replier::{Binder, MintedBinding};
use crate::wecom::resolvers::{IdentityQueries, InstallationQueries};
use crate::wecom::types::Installation;
use async_trait::async_trait;
use mc_core::id::Id;

// =====================================================================

/// 一份明文的凭据解析器（真解封要 `secretbox`，而那条链在 `tests.rs` 里另有用例）。
pub struct StaticCredentials;

impl CredentialsResolver for StaticCredentials {
    fn credentials(
        &self,
        installation: &Installation,
    ) -> Result<InstallationCredentials, CredentialsError> {
        Ok(InstallationCredentials {
            bot_id: installation.bot_id.clone(),
            secret: PlaintextSecret::new("static-secret"),
        })
    }
}

/// 安装查询替身（一条固定安装）。
pub struct MemoryInstallations {
    pub installation: Installation,
}

#[async_trait]
impl InstallationQueries for MemoryInstallations {
    async fn find_active_by_bot_id(
        &self,
        bot_id: &str,
    ) -> Result<Option<Installation>, mc_repos::RepoError> {
        Ok((bot_id == self.installation.bot_id).then(|| self.installation.clone()))
    }
}

/// 身份查询替身：`bound` 决定"绑没绑"。
pub struct MemoryIdentities {
    pub bound: Option<Id>,
}

#[async_trait]
impl IdentityQueries for MemoryIdentities {
    async fn find_user_binding(
        &self,
        _installation_id: Id,
        _channel_user_id: &str,
    ) -> Result<Option<Id>, mc_repos::RepoError> {
        Ok(self.bound)
    }

    async fn is_workspace_member(
        &self,
        _workspace_id: Id,
        _user_id: Id,
    ) -> Result<bool, mc_repos::RepoError> {
        Ok(true)
    }
}

/// 两阶段幂等的**内存**接缝：`claim` 铸一枚令牌，`mark` 之后永远 `None`。
///
/// "命中不报错"这条语义的**真实**承担者是 [`ChannelDeduper`](crate::engine::session::ChannelDeduper)
/// —— 本结构只是它的表。
#[derive(Default)]
pub struct MemoryDedup {
    rows: Mutex<HashMap<String, (Id, bool)>>,
}

#[async_trait]
impl DedupStore for MemoryDedup {
    async fn claim(&self, _installation_id: Id, message_id: &str) -> EngineResult<Option<Id>> {
        let mut rows = self.rows.lock().expect("lock");
        // 已经有主（终态，或在飞）⇒ `None` ⇒ `ChannelDeduper` 翻成 `duplicate`。
        if rows.contains_key(message_id) {
            return Ok(None);
        }
        let token = Id::new();
        rows.insert(message_id.to_string(), (token, false));
        Ok(Some(token))
    }

    async fn mark(
        &self,
        _installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool> {
        let mut rows = self.rows.lock().expect("lock");
        match rows.get_mut(message_id) {
            Some(row) if row.0 == claim_token => {
                row.1 = true;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn release(
        &self,
        _installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool> {
        let mut rows = self.rows.lock().expect("lock");
        if rows.get(message_id).is_some_and(|row| row.0 == claim_token) {
            rows.remove(message_id);
            return Ok(true);
        }
        Ok(false)
    }
}

/// 会话绑定替身（记下每一次调用）。
#[derive(Default)]
pub struct MemorySession {
    pub ensured: Mutex<Vec<String>>,
    pub appended: Mutex<Vec<String>>,
}

#[async_trait]
impl SessionBinder for MemorySession {
    async fn ensure_session(&self, params: EnsureSessionParams) -> EngineResult<Id> {
        self.ensured
            .lock()
            .expect("lock")
            .push(params.message.source.chat_id.clone());
        Ok(Id::new())
    }

    async fn start_session(&self, _params: StartSessionParams) -> EngineResult<StartSessionResult> {
        Ok(StartSessionResult {
            session_id: Id::new(),
            binding_id: None,
            route_revision: 1,
            append: AppendResult::default(),
        })
    }

    async fn mark_pending_fresh(&self, _session_id: Id, _message_id: &str) -> EngineResult<()> {
        Ok(())
    }

    async fn append_message(&self, params: AppendParams) -> EngineResult<AppendResult> {
        self.appended
            .lock()
            .expect("lock")
            .push(params.message.command_source_text().to_string());
        Ok(AppendResult::default())
    }

    async fn bind_media(&self, _params: BindMediaParams) -> EngineResult<BindMediaResult> {
        Ok(BindMediaResult::default())
    }
}

/// 审计替身：记下**原因**（"丢弃不报错"那条断言的另一面）。
#[derive(Default)]
pub struct MemoryAudit {
    pub drops: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl Auditor for MemoryAudit {
    async fn record_drop(
        &self,
        _installation_id: Option<Id>,
        message: &mc_core::channel::message::InboundMessage,
        reason: DropReason,
    ) -> EngineResult<()> {
        self.drops
            .lock()
            .expect("lock")
            .push((reason.as_str().to_string(), message.message_id.clone()));
        Ok(())
    }
}

/// 运行触发替身：数次数。
#[derive(Default)]
pub struct CountingTrigger {
    pub runs: AtomicUsize,
}

#[async_trait]
impl RunTriggerer for CountingTrigger {
    async fn schedule_chat_run(&self, _params: ChatRunParams) -> EngineResult<()> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn drain(&self) -> EngineResult<()> {
        Ok(())
    }
}

pub struct NoReader;

#[async_trait]
impl SessionReader for NoReader {
    async fn workspace_identity(&self, _workspace_id: Id) -> EngineResult<WorkspaceIdentity> {
        Ok(WorkspaceIdentity::default())
    }
}

/// `/issue` 的替身：每次都回同一个 issue（`number = 7`）。
pub struct FixedIssues;

#[async_trait]
impl IssueCreator for FixedIssues {
    async fn create_issue(&self, _params: ChannelIssueParams) -> EngineResult<ChannelIssueOutcome> {
        Ok(ChannelIssueOutcome {
            issue: ChannelIssue {
                id: Id::new(),
                number: 7,
                title: "登录坏了".to_string(),
            },
            duplicate: false,
            assigned_task_id: None,
        })
    }
}

/// 铸令牌的替身：一枚固定明文令牌。
pub struct FixedBinder;

#[async_trait]
impl Binder for FixedBinder {
    async fn mint(
        &self,
        _workspace_id: Id,
        _installation_id: Id,
        _channel_user_id: &str,
    ) -> Result<MintedBinding, String> {
        Ok(MintedBinding {
            raw: "bind-token-xyz".to_string(),
            reused: false,
        })
    }
}

/// 媒体解析的替身：本回路里断言的是"没有媒体时不碰它"，所以它只记次数。
#[derive(Default)]
pub struct NeverMedia {
    pub calls: AtomicUsize,
}

impl MediaResolver for NeverMedia {
    fn has_media(&self, _message: &mc_core::channel::message::InboundMessage) -> bool {
        false
    }
    fn resolve_media(
        &self,
        _installation: &ResolvedInstallation,
        _sender: &crate::engine::ResolvedIdentity,
        _session_id: Id,
        _chat_message_id: Option<Id>,
        message: &mc_core::channel::message::InboundMessage,
    ) -> mc_core::channel::message::InboundMessage {
        self.calls.fetch_add(1, Ordering::SeqCst);
        message.clone()
    }
}
