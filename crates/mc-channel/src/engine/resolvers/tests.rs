use super::*;
use mc_core::channel::message::{ChatType, MessageKind, Source};

fn inbound() -> InboundMessage {
    InboundMessage {
        event_id: "Ev1".into(),
        message_id: "Msg1".into(),
        source: Source {
            channel_type: ChannelKind::Lark,
            chat_id: "C1".into(),
            chat_type: ChatType::P2p,
            sender_id: "U1".into(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Text,
        text: "hello".into(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: false,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::json!({}),
    }
}

/// 判决词表的 wire 取值**逐字**对齐上游（看板/日志靠它聚合）。
#[test]
fn outcome_and_drop_reason_wire_values_are_stable() {
    let outcomes = [
        (Outcome::Dropped, "dropped"),
        (Outcome::NeedsBinding, "needs_binding"),
        (Outcome::Ingested, "ingested"),
        (Outcome::FreshPending, "fresh_pending"),
        (Outcome::ChatStarted, "chat_started"),
        (Outcome::IssueUsage, "issue_usage"),
        (Outcome::AgentOffline, "agent_offline"),
        (Outcome::AgentArchived, "agent_archived"),
    ];
    assert_eq!(outcomes.len(), 8, "上游 8 个 Outcome 一个不多一个不少");
    for (outcome, wire) in outcomes {
        assert_eq!(outcome.as_str(), wire);
    }
    let reasons = [
        (DropReason::UnboundUser, "unbound_user"),
        (DropReason::NonWorkspaceMember, "non_workspace_member"),
        (DropReason::NotAddressedInGroup, "not_addressed_in_group"),
        (DropReason::Duplicate, "duplicate"),
        (DropReason::RevokedInstallation, "revoked_installation"),
        (DropReason::InvalidEvent, "invalid_event"),
    ];
    assert_eq!(reasons.len(), 6);
    for (reason, wire) in reasons {
        assert_eq!(reason.as_str(), wire);
    }
}

/// 哨兵 → 丢弃原因的映射（Router `match` 它，测试钉住别漂）。
#[test]
fn pipeline_errors_map_to_drop_reasons() {
    assert_eq!(
        PipelineError::InstallationNotFound.code(),
        "installation_not_found"
    );
    assert_eq!(
        PipelineError::InstallationNotFound.drop_reason(),
        Some(DropReason::InvalidEvent)
    );
    assert_eq!(
        PipelineError::SenderNotMember.drop_reason(),
        Some(DropReason::NonWorkspaceMember)
    );
    // 未绑定**不是**丢弃：它驱动绑定卡。
    assert_eq!(PipelineError::SenderUnbound.drop_reason(), None);
    assert_eq!(PipelineError::RouteChanged.drop_reason(), None);
    assert_eq!(
        PipelineError::ClaimLost.drop_reason(),
        Some(DropReason::Duplicate),
        "令牌被抢等价于重复"
    );
    assert_eq!(
        PipelineError::Duplicate.drop_reason(),
        Some(DropReason::Duplicate)
    );
}

/// `EngineError` 的三层映射：管道判决 / 基础设施 / 链路。
#[test]
fn engine_error_maps_back_to_channel_error() {
    let pipeline: EngineError = PipelineError::SenderUnbound.into();
    assert_eq!(
        pipeline.clone().into_channel_error().code(),
        "channel_storage_error"
    );
    assert!(EngineError::infra("db down")
        .to_string()
        .contains("db down"));
    let channel: EngineError = ChannelError::LeaseHeld.into();
    assert_eq!(channel.into_channel_error(), ChannelError::LeaseHeld);
    assert_eq!(EngineError::infra("x").code_hint(), "engine_infra_error");
}

/// 丢弃结论的构造与判定。
#[test]
fn route_result_dropped_helper() {
    let installation = Id::new();
    let dropped = RouteResult::dropped(DropReason::Duplicate, Some(installation));
    assert!(dropped.is_dropped());
    assert_eq!(dropped.drop_reason, Some(DropReason::Duplicate));
    assert_eq!(dropped.installation_id, Some(installation));
    assert_eq!(dropped.outcome.as_str(), "dropped");
    // 默认值是"丢弃 + 无原因"：`Default` 只用来给其它字段兜底。
    assert_eq!(RouteResult::default().outcome, Outcome::Dropped);
}

/// 安装上下文的 `Debug` **不**打印平台值（凭据纪律，`docs/60` §2.3）。
#[test]
fn resolved_installation_debug_hides_platform_value() {
    let mut installation = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        ChannelKind::Lark,
        true,
    );
    installation.platform = Some(Arc::new("APP-SECRET-PLAINTEXT".to_string()));
    let rendered = format!("{installation:?}");
    assert!(!rendered.contains("APP-SECRET-PLAINTEXT"), "回显了平台值");
    assert!(rendered.contains("<opaque>"));
    assert!(rendered.contains("Lark"));
    assert!(rendered.contains("true"));
}

/// 默认分类器**不认**任何命令（"没接命令面"的诚实形态）。
#[test]
fn no_commands_classifier_is_inert() {
    assert_eq!(NoCommands.classify("/issue fix it"), CommandIntent::None);
    assert_eq!(NoCommands.classify("/new"), CommandIntent::None);
    assert_eq!(NoCommands.classify("plain text"), CommandIntent::None);
}

/// `ResolverSet` 的构造与 `Debug`（只列存在性，不打印端口内部状态）。
#[test]
fn resolver_set_reports_optional_ports() {
    let (installation, identity, dedup, session, audit) = (
        Arc::new(StubInstallation) as Arc<dyn InstallationResolver>,
        Arc::new(StubIdentity) as Arc<dyn IdentityResolver>,
        Arc::new(StubDedup) as Arc<dyn Deduper>,
        Arc::new(StubSession) as Arc<dyn SessionBinder>,
        Arc::new(StubAudit) as Arc<dyn Auditor>,
    );
    let set = ResolverSet::new(installation, identity, dedup, session, audit, "lark_chat");
    assert_eq!(set.origin_type, "lark_chat");
    let rendered = format!("{set:?}");
    assert!(rendered.contains("media: false"));
    assert!(rendered.contains("replier: false"));
    assert!(rendered.contains("typing: false"));
    assert!(rendered.contains("<dyn InstallationResolver>"));
}

/// 端口的测试替身（其余模块复用它们；只实现会被调用的方法）。
struct StubInstallation;

#[async_trait]
impl InstallationResolver for StubInstallation {
    async fn resolve_installation(
        &self,
        _message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation> {
        Err(PipelineError::InstallationNotFound.into())
    }
}

struct StubIdentity;

#[async_trait]
impl IdentityResolver for StubIdentity {
    async fn resolve_sender(
        &self,
        _installation: &ResolvedInstallation,
        _message: &InboundMessage,
    ) -> EngineResult<ResolvedIdentity> {
        Ok(ResolvedIdentity { user_id: Id::new() })
    }
}

struct StubDedup;

#[async_trait]
impl Deduper for StubDedup {
    async fn claim(&self, _installation_id: Id, _message_id: &str) -> EngineResult<Id> {
        Ok(Id::new())
    }
    async fn mark(
        &self,
        _installation_id: Id,
        _message_id: &str,
        _claim_token: Id,
    ) -> EngineResult<()> {
        Ok(())
    }
    async fn release(
        &self,
        _installation_id: Id,
        _message_id: &str,
        _claim_token: Id,
    ) -> EngineResult<()> {
        Ok(())
    }
}

struct StubSession;

#[async_trait]
impl SessionBinder for StubSession {
    async fn ensure_session(&self, _params: EnsureSessionParams) -> EngineResult<Id> {
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
    async fn append_message(&self, _params: AppendParams) -> EngineResult<AppendResult> {
        Ok(AppendResult::default())
    }
    async fn bind_media(&self, _params: BindMediaParams) -> EngineResult<BindMediaResult> {
        Ok(BindMediaResult::default())
    }
}

struct StubAudit;

#[async_trait]
impl Auditor for StubAudit {
    async fn record_drop(
        &self,
        _installation_id: Option<Id>,
        _message: &InboundMessage,
        _reason: DropReason,
    ) -> EngineResult<()> {
        Ok(())
    }
}

/// 这条用例把"入站信封只读 `command_text`"的口径钉在类型层（分类器拿到的文本）。
#[test]
fn classifier_body_comes_from_command_source_text() {
    let mut message = inbound();
    message.text = "[引用]\n/issue fix".into();
    message.command_text = "/issue fix".into();
    assert_eq!(message.command_source_text(), "/issue fix");
    message.command_text.clear();
    assert_eq!(message.command_source_text(), "[引用]\n/issue fix");
}

/// `RouteResult::issue_usage_had_media` 与 `issue` 是两个独立字段（回复器要分开读）。
#[test]
fn route_result_carries_issue_and_media_flag_independently() {
    let result = RouteResult {
        outcome: Outcome::IssueUsage,
        issue_usage_had_media: true,
        ..RouteResult::default()
    };
    assert!(result.issue.is_none());
    assert!(result.issue_usage_had_media);
    assert_eq!(result.outcome.as_str(), "issue_usage");
}

/// 端口包的构造器不吞必填端口（`ResolverSet::new` 的全参数形态）。
#[test]
fn resolver_set_builder_keeps_origin_type() {
    let set = ResolverSet::new(
        Arc::new(StubInstallation),
        Arc::new(StubIdentity),
        Arc::new(StubDedup),
        Arc::new(StubSession),
        Arc::new(StubAudit),
        "wecom_chat",
    );
    assert_eq!(set.origin_type, "wecom_chat");
    assert!(set.media.is_none());
}
