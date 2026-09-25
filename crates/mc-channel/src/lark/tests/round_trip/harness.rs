//! `round_trip.rs` 的**装置**：真解析器 + 内存端口 + 一条完整的回路装配。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - 拆出本文件是**门 ⑩**（单文件 800 行硬限）的要求，切点是「装置 ∥ 断言」——
//!   本文件里没有一条 `#[test]`（同 M7-11 的 `ws_connector/tests/harness.rs` 先例）。
//!
//! # 真到什么程度
//!
//! [`Harness`] 装的是**真** Router + **真** M7-12 解析器 + **真** M7-13 出站面；只有**别的片**
//! 的端口（去重 / 会话 / 触发 / 读侧 / issue）与平台 wire（[`FakeApi`]）是替身。于是用例钉住的
//! 是"判决 → 出站"这条链，不是替身之间的对话。
//!
//! [`decode_envelope`] 走的是 M7-11 的**真**解码器（不是手搓结构体）；WS **帧**层的分片重组是
//! M7-11 / M7-12 的替身范围（`ws_connector/tests/harness.rs`），本装置从"信封已完整"处接手。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::id::Id;
use serde_json::json;
use uuid::Uuid;

use crate::engine::resolvers::{
    AppendParams, AppendResult, Auditor, BindMediaParams, BindMediaResult, ChannelIssueOutcome,
    ChannelIssueParams, ChatRunParams, Deduper, DropReason, EngineError, EngineResult,
    EnsureSessionParams, IssueCreator, NoCommands, OutboundReplier, PipelineError, ResolverSet,
    RunTriggerer, SessionBinder, SessionReader, StartSessionParams, StartSessionResult,
    WorkspaceIdentity,
};
use crate::engine::router::{Router, RouterConfig};
use crate::lark::outbound::LarkOutboundDelivery;
use crate::lark::replier::{BindingTokenMinter, LarkOutcomeReplier};
use crate::lark::resolvers::{
    IdentityQueries, InstallationQueries, LarkIdentityResolver, LarkInstallation,
    LarkInstallationResolver,
};
use crate::lark::tests::support::{
    decrypter, delivery_row, installation, FakeApi, FakeMinter, MemoryStore,
};
use crate::lark::types::ChatType;
use crate::lark::typing::TypingIndicatorManager;
use crate::lark::ws_frame_decoder::{FrameDecoder, LarkInboundEvent, LarkJsonFrameDecoder};

/// 本装置用的固定 id（与 `outbound/tests.rs` 的常量**同值但不同命名空间**）。
pub(super) const INST: u128 = 0x3000;
pub(super) const SESSION: u128 = 0x2000;
pub(super) const TASK: u128 = 0x4000;
pub(super) const BINDING: u128 = 0x5000;
pub(super) const USER: u128 = 0x9100;

// =====================================================================
// 装置：真解析器 + 内存端口
// =====================================================================

/// M7-12 的安装查询端口（内存）。
pub(super) struct MemoryInstallations(Arc<MemoryStore>);

#[async_trait]
impl InstallationQueries for MemoryInstallations {
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<LarkInstallation>, mc_repos::RepoError> {
        Ok(self
            .0
            .installations
            .lock()
            .expect("poisoned")
            .values()
            .find(|row| row.app_id == app_id && row.status == "active")
            .map(LarkInstallation::from))
    }
}

/// M7-12 的身份查询端口（内存）：`bound` 决定发件人是否已绑定。
pub(super) struct MemoryIdentities {
    bound: bool,
    member: bool,
}

#[async_trait]
impl IdentityQueries for MemoryIdentities {
    async fn find_user_binding(
        &self,
        installation_id: Id,
        lark_open_id: &str,
    ) -> Result<Option<mc_repos::channel::binding::LarkUserBindingRow>, mc_repos::RepoError> {
        if !self.bound {
            return Ok(None);
        }
        Ok(Some(mc_repos::channel::binding::LarkUserBindingRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::from_u128(0x9000),
            multica_user_id: Uuid::from_u128(USER),
            installation_id: installation_id.0,
            lark_open_id: lark_open_id.to_string(),
            union_id: Some("on_sender".to_string()),
            bound_at: chrono::Utc::now(),
        }))
    }

    async fn is_workspace_member(
        &self,
        _workspace_id: Id,
        _user_id: Id,
    ) -> Result<bool, mc_repos::RepoError> {
        Ok(self.member)
    }
}

/// 去重端口（**别的片**的实现；本用例只钉"这条消息第一次见"）。
#[derive(Default)]
pub(super) struct MemoryDedup {
    #[allow(dead_code)]
    pub(super) claims: Mutex<Vec<String>>,
}

#[async_trait]
impl Deduper for MemoryDedup {
    async fn claim(&self, _installation_id: Id, message_id: &str) -> EngineResult<Id> {
        let mut claims = self.claims.lock().expect("poisoned");
        if claims.iter().any(|seen| seen == message_id) {
            return Err(PipelineError::Duplicate.into());
        }
        claims.push(message_id.to_string());
        Ok(Id(Uuid::new_v4()))
    }

    async fn mark(&self, _i: Id, _m: &str, _t: Id) -> EngineResult<()> {
        Ok(())
    }

    async fn release(&self, _i: Id, _m: &str, _t: Id) -> EngineResult<()> {
        Ok(())
    }
}

/// 审计端口（**别的片**的实现；本用例只记下来）。
#[derive(Default)]
pub(super) struct MemoryAudit {
    pub(super) dropped: Mutex<Vec<DropReason>>,
}

#[async_trait]
impl Auditor for MemoryAudit {
    async fn record_drop(
        &self,
        _installation_id: Option<Id>,
        _message: &InboundMessage,
        reason: DropReason,
    ) -> EngineResult<()> {
        self.dropped.lock().expect("poisoned").push(reason);
        Ok(())
    }
}

/// 会话端口（**别的片**的实现；只给出流水线下游要的字段）。
pub(super) struct MemorySession;

#[async_trait]
impl SessionBinder for MemorySession {
    async fn ensure_session(&self, _params: EnsureSessionParams) -> EngineResult<Id> {
        Ok(Id(Uuid::from_u128(SESSION)))
    }

    async fn start_session(&self, _params: StartSessionParams) -> EngineResult<StartSessionResult> {
        Ok(StartSessionResult {
            session_id: Id(Uuid::from_u128(SESSION)),
            binding_id: Some(Id(Uuid::from_u128(BINDING))),
            route_revision: 3,
            append: append_result(),
        })
    }

    async fn mark_pending_fresh(&self, _session_id: Id, _message_id: &str) -> EngineResult<()> {
        Ok(())
    }

    async fn append_message(&self, _params: AppendParams) -> EngineResult<AppendResult> {
        Ok(append_result())
    }

    async fn bind_media(&self, _params: BindMediaParams) -> EngineResult<BindMediaResult> {
        Ok(BindMediaResult::default())
    }
}

pub(super) fn append_result() -> AppendResult {
    AppendResult {
        message_id: Some(Id(Uuid::from_u128(0x6001))),
        context_revision: 1,
        binding_id: Some(Id(Uuid::from_u128(BINDING))),
        route_revision: 3,
        ..AppendResult::default()
    }
}

/// 触发端口（**别的片**的实现；记下每次排的 run）。
#[derive(Default)]
pub(super) struct MemoryTrigger {
    pub(super) runs: Mutex<Vec<ChatRunParams>>,
}

#[async_trait]
impl RunTriggerer for MemoryTrigger {
    async fn schedule_chat_run(&self, params: ChatRunParams) -> EngineResult<()> {
        self.runs.lock().expect("poisoned").push(params);
        Ok(())
    }

    async fn drain(&self) -> EngineResult<()> {
        Ok(())
    }
}

/// 读侧端口（**别的片**的实现）。
pub(super) struct MemoryReader;

#[async_trait]
impl SessionReader for MemoryReader {
    async fn workspace_identity(&self, _workspace_id: Id) -> EngineResult<WorkspaceIdentity> {
        Ok(WorkspaceIdentity {
            issue_prefix: "ABC".to_string(),
            slug: "acme".to_string(),
        })
    }
}

/// issue 端口（**别的片**的实现；本用例不走 `/issue`）。
pub(super) struct MemoryIssues;

#[async_trait]
impl IssueCreator for MemoryIssues {
    async fn create_issue(&self, _params: ChannelIssueParams) -> EngineResult<ChannelIssueOutcome> {
        Err(EngineError::infra("not wired in this test"))
    }
}

/// 一条**真**的解码产物：v2 信封的 JSON 直接喂给 M7-11 的解码器（不是手搓结构体）。
///
/// WS **帧**层（`ws_frame.rs` 的分片重组）是 M7-11 / M7-12 的替身范围；本用例从"信封已完整"
/// 处接手 —— 解码器本身仍是真代码。
pub(super) fn decode_envelope(
    event_id: &str,
    message_id: &str,
    app_id: &str,
    chat_id: &str,
    chat_type: &str,
    text: &str,
    mentions: &serde_json::Value,
) -> LarkInboundEvent {
    let envelope = json!({
        "schema": "2.0",
        "header": {
            "event_id": event_id,
            "event_type": "im.message.receive_v1",
            "app_id": app_id,
            "tenant_key": "tk",
            "create_time": "1700000000000",
        },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_sender", "union_id": "on_sender" } },
            "message": {
                "message_id": message_id,
                "chat_id": chat_id,
                "chat_type": chat_type,
                "message_type": "text",
                "content": json!({"text": text}).to_string(),
                "mentions": mentions,
                "thread_id": "",
                "create_time": "1700000000000",
            },
        }
    });
    let payload = serde_json::to_vec(&envelope).expect("serializes");
    match LarkJsonFrameDecoder::new()
        .decode(&payload)
        .expect("frame decodes")
    {
        crate::lark::ws_frame_decoder::DecodeOutcome::Message(event) => *event,
        crate::lark::ws_frame_decoder::DecodeOutcome::Ignored => {
            panic!("expected a message outcome, got Ignored")
        }
    }
}

/// 把一条帧变成归一化后的入站信封（**真**归一化：`from_event` + `to_inbound_message`）。
pub(super) fn normalize(event: LarkInboundEvent, platform: &LarkInstallation) -> InboundMessage {
    crate::lark::feishu_channel::LarkInboundMessage::from_event(event, platform)
        .to_inbound_message()
        .expect("normalizes")
}

/// 装一条完整的回路（真 Router + 真 lark 解析器 + 真出站面）。
pub(super) struct Harness {
    pub(super) router: Router,
    pub(super) api: Arc<FakeApi>,
    pub(super) store: Arc<MemoryStore>,
    pub(super) trigger: Arc<MemoryTrigger>,
    pub(super) audit: Arc<MemoryAudit>,
    pub(super) platform: LarkInstallation,
    pub(super) typing: Arc<TypingIndicatorManager>,
    pub(super) delivery: LarkOutboundDelivery,
}

impl Harness {
    pub(super) fn new(bound: bool, member: bool) -> Self {
        let api = FakeApi::new();
        let store = MemoryStore::new();
        let platform = installation(INST, "cli_a");
        store.put_installation(&platform);
        store.put_agent_name(platform.agent_id, "Bot");

        let bridged = store.store();
        let trigger = Arc::new(MemoryTrigger::default());
        let audit = Arc::new(MemoryAudit::default());

        let typing = Arc::new(
            TypingIndicatorManager::new(
                Arc::clone(&api) as Arc<dyn crate::lark::client::ApiClient>,
                decrypter(),
                Arc::clone(&bridged) as Arc<dyn crate::lark::typing::TypingIndicatorQueries>,
            )
            .with_clock(Arc::new(crate::lark::typing::ManualWallClock::new(
                1_700_000_000_000,
            ))),
        );
        let minter = FakeMinter::new();
        let mut set = ResolverSet::new(
            Arc::new(LarkInstallationResolver::new(Arc::new(
                MemoryInstallations(Arc::clone(&store)),
            ))),
            Arc::new(LarkIdentityResolver::new(Arc::new(MemoryIdentities {
                bound,
                member,
            }))),
            Arc::new(MemoryDedup::default()),
            Arc::new(MemorySession),
            Arc::clone(&audit) as Arc<dyn Auditor>,
            crate::lark::resolvers::ORIGIN_LARK_CHAT,
        )
        .with_typing(Arc::clone(&typing) as Arc<dyn crate::engine::resolvers::TypingNotifier>)
        .with_replier(Arc::new(LarkOutcomeReplier::new(
            Arc::clone(&api) as Arc<dyn crate::lark::client::ApiClient>,
            Arc::clone(&minter) as Arc<dyn BindingTokenMinter>,
            decrypter(),
            Arc::clone(&bridged) as Arc<dyn crate::lark::replier::OutcomeReplierQueries>,
            "https://app.example",
            "",
        )) as Arc<dyn OutboundReplier>);
        // 媒体面不在本片（M7-12 落的）⇒ `None` 是"该部署不做媒体"，不是 todo。
        set = set.with_media({
            // 一个"没有媒体"的端口：`has_media` 恒 false ⇒ 流水线走普通入库路径。
            struct NoMedia;
            impl crate::engine::resolvers::MediaResolver for NoMedia {
                fn has_media(&self, _message: &InboundMessage) -> bool {
                    false
                }

                fn resolve_media(
                    &self,
                    _installation: &crate::engine::resolvers::ResolvedInstallation,
                    _sender: &crate::engine::resolvers::ResolvedIdentity,
                    _session_id: Id,
                    _chat_message_id: Option<Id>,
                    message: &InboundMessage,
                ) -> InboundMessage {
                    message.clone()
                }
            }
            Arc::new(NoMedia)
        });

        let router = Router::new(
            Arc::new(NoCommands),
            Arc::clone(&trigger) as Arc<dyn RunTriggerer>,
            Arc::new(MemoryReader),
            Arc::new(MemoryIssues),
            RouterConfig::default(),
        );
        router.register(crate::lark::resolvers::TYPE_LARK, set);

        let delivery = LarkOutboundDelivery::new(
            Arc::clone(&bridged) as Arc<dyn crate::lark::outbound::PatcherQueries>,
            Arc::clone(&api) as Arc<dyn crate::lark::client::ApiClient>,
        )
        .with_decrypter(decrypter())
        .with_typing(Arc::clone(&typing));

        Self {
            router,
            api,
            store,
            trigger,
            audit,
            platform,
            typing,
            delivery,
        }
    }

    /// 一条投递行 + 一条遗留会话绑定行（出站面要它们）。
    pub(super) fn arm_delivery(&self, chat_id: &str, chat_type: ChatType, message_id: &str) {
        self.store.put_delivery(delivery_row(
            Id(Uuid::from_u128(TASK)),
            Id(Uuid::from_u128(BINDING)),
            Id(Uuid::from_u128(INST)),
            chat_id,
            chat_type,
            Some(message_id),
            None,
            json!({"sender_id": "ou_sender"}),
        ));
        self.store
            .put_legacy_binding(crate::lark::tests::support::legacy_binding_row(
                Id(Uuid::from_u128(SESSION)),
                Id(Uuid::from_u128(INST)),
                chat_id,
                chat_type,
            ));
    }
}
