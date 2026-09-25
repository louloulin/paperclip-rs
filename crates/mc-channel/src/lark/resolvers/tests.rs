//! [`super`]（解析器集合：安装 / 身份 / 会话 / 判决映射）的用例。
//!
//! 五组：安装行投影 / 安装路由 / 身份解析 / 会话隔离键 / 判决→出站值的映射。
//! 全部用**替身**（不是真库）：每个解析器只依赖一个小接口，所以"未绑定 ⇒ 回绑定卡"、
//! "非成员 ⇒ 丢弃"这些判决不需要 PostgreSQL 就能钉住。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use mc_core::channel::message::{ChatType, InboundMessage, Source};
use mc_core::id::Id;
use mc_repos::channel::binding::LarkUserBindingRow;
use mc_repos::channel::installation::LarkInstallationRow;
use mc_repos::RepoError;
use uuid::Uuid;

use super::*;
use crate::engine::resolvers::{
    EngineError, IdentityResolver, InstallationResolver, PipelineError, RouteResult,
};

// =====================================================================
// 夹具
// =====================================================================

fn row() -> LarkInstallationRow {
    LarkInstallationRow {
        id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        app_id: "cli_test".to_string(),
        app_secret_encrypted: vec![1, 2, 3],
        tenant_key: Some("tenant-1".to_string()),
        bot_open_id: "ou_bot".to_string(),
        bot_union_id: Some("on_bot".to_string()),
        region: "lark".to_string(),
        installer_user_id: Uuid::new_v4(),
        status: "active".to_string(),
        ws_lease_token: None,
        ws_lease_expires_at: None,
        installed_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn installation() -> LarkInstallation {
    LarkInstallation::from(&row())
}

/// 一条归一化入站消息（`raw` 是本 adapter 自己的载荷形状）。
fn message(app_id: &str, chat_id: &str, thread_id: &str, sender: &str) -> InboundMessage {
    InboundMessage {
        event_id: "ev-1".to_string(),
        message_id: "om-1".to_string(),
        source: Source {
            channel_type: TYPE_LARK,
            chat_id: chat_id.to_string(),
            chat_type: if thread_id.is_empty() {
                ChatType::P2p
            } else {
                ChatType::Group
            },
            sender_id: sender.to_string(),
            sender_stable_id: String::new(),
            thread_id: thread_id.to_string(),
        },
        kind: mc_core::channel::message::MessageKind::Text,
        text: "hi".to_string(),
        command_text: "hi".to_string(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: false,
        force_fresh: false,
        skip_agent_run: false,
        // `raw` 必须是本 adapter 自己的**完整**载荷（`decode_raw` 会整份反序列化）。
        raw: serde_json::to_value(super::LarkInboundMessage {
            event_type: "im.message.receive_v1".to_string(),
            event_id: "ev-1".to_string(),
            app_id: app_id.to_string(),
            tenant_key: "tenant-1".to_string(),
            chat_id: super::super::types::ChatId::new(chat_id),
            chat_type: if thread_id.is_empty() {
                ChatType::P2p
            } else {
                ChatType::Group
            },
            message_id: "om-1".to_string(),
            sender_open_id: super::super::types::OpenId::new(sender),
            sender_union_id: String::new(),
            message_type: "text".to_string(),
            content: r#"{"text":"hi"}"#.to_string(),
            mentions: Vec::new(),
            create_time: "1700000000000".to_string(),
            parent_id: String::new(),
            root_id: String::new(),
            thread_id: thread_id.to_string(),
            envelope: serde_json::Value::Null,
            body: "hi".to_string(),
            command_body: "hi".to_string(),
            addressed_to_bot: false,
            force_fresh_session: false,
            has_selected_context: false,
        })
        .expect("载荷应当可序列化"),
    }
}

// =====================================================================
// 一、安装行投影
// =====================================================================

/// 遗留行的投影：密文原样、空 `bot_union_id` 变 `None`、region 归一、`active` 判据。
#[test]
fn installation_projection_maps_every_field() {
    let raw = row();
    let projection = LarkInstallation::from(&raw);
    assert_eq!(projection.id, Id(raw.id));
    assert_eq!(projection.workspace_id, Id(raw.workspace_id));
    assert_eq!(projection.agent_id, Id(raw.agent_id));
    assert_eq!(projection.app_id, "cli_test");
    assert_eq!(projection.app_secret_encrypted, vec![1, 2, 3]);
    assert_eq!(projection.tenant_key.as_deref(), Some("tenant-1"));
    assert_eq!(projection.bot_open_id.as_str(), "ou_bot");
    assert_eq!(projection.bot_union_id.as_deref(), Some("on_bot"));
    assert_eq!(projection.bot_union_id_or_empty(), "on_bot");
    assert_eq!(projection.region, Region::Lark);
    assert!(projection.is_active());

    // 回填之前 / 已撤销。
    let mut legacy = raw.clone();
    legacy.bot_union_id = None;
    let mut revoked = projection.clone();
    revoked.status = "revoked".to_string();
    let legacy_projection = LarkInstallation::from(&legacy);
    assert_eq!(legacy_projection.bot_union_id, None);
    assert_eq!(legacy_projection.bot_union_id_or_empty(), "");
    assert!(!revoked.is_active());
    // 认不出的 region 回落飞书。
    let mut weird = raw;
    weird.region = "new-cloud".to_string();
    assert_eq!(LarkInstallation::from(&weird).region, Region::Feishu);
}

/// `decode_raw`：空 / 形状不对都是**基础设施**失败（`raw` 是本 adapter 自己写的）。
#[test]
fn decode_raw_rejects_empty_and_malformed_payloads() {
    let mut empty = message("cli_test", "oc-1", "", "ou_a");
    empty.raw = serde_json::Value::Null;
    assert!(matches!(decode_raw(&empty), Err(EngineError::Infra { .. })));

    let mut broken = message("cli_test", "oc-1", "", "ou_a");
    broken.raw = serde_json::json!({"totally": "different"});
    assert!(matches!(
        decode_raw(&broken),
        Err(EngineError::Infra { .. })
    ));
}

// =====================================================================
// 二、安装路由
// =====================================================================

struct FakeInstallationQueries {
    found: Option<LarkInstallation>,
    /// `true` ⇒ 查询报基础设施错（`RepoError` **没有** `Clone` ⇒ 用一个开关而不是存一份错误）。
    fail: bool,
    seen_app_id: Mutex<Vec<String>>,
}

#[async_trait]
impl InstallationQueries for FakeInstallationQueries {
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<LarkInstallation>, RepoError> {
        self.seen_app_id
            .lock()
            .expect("lock")
            .push(app_id.to_string());
        if self.fail {
            return Err(RepoError::Db("pool exhausted".to_string()));
        }
        Ok(self.found.clone())
    }
}

/// 命中 ⇒ 完整的 `ResolvedInstallation`（`platform` 里带着本 adapter 的投影）。
#[tokio::test]
async fn installation_resolver_returns_the_platform_payload() {
    let queries = Arc::new(FakeInstallationQueries {
        found: Some(installation()),
        fail: false,
        seen_app_id: Mutex::new(Vec::new()),
    });
    let resolver =
        LarkInstallationResolver::new(Arc::clone(&queries) as Arc<dyn InstallationQueries>);
    let answer = resolver
        .resolve_installation(&message("cli_test", "oc-1", "", "ou_a"))
        .await
        .expect("应当命中");
    assert_eq!(answer.kind, TYPE_LARK);
    assert!(answer.active);
    assert_eq!(
        queries.seen_app_id.lock().expect("lock").as_slice(),
        ["cli_test"]
    );
    let platform = platform_installation(&answer).expect("应当带平台投影");
    assert_eq!(platform.app_id, "cli_test");
}

/// 认不出 `app_id` ⇒ **产品性**丢弃（`installation_not_found`），不是错误。
#[tokio::test]
async fn unknown_app_id_is_a_product_decision() {
    let queries = Arc::new(FakeInstallationQueries {
        found: None,
        fail: false,
        seen_app_id: Mutex::new(Vec::new()),
    });
    let resolver =
        LarkInstallationResolver::new(Arc::clone(&queries) as Arc<dyn InstallationQueries>);
    let error = resolver
        .resolve_installation(&message("cli_unknown", "oc-1", "", "ou_a"))
        .await
        .expect_err("应当判丢弃");
    assert_eq!(
        error,
        EngineError::Pipeline(PipelineError::InstallationNotFound)
    );
    assert_eq!(
        PipelineError::InstallationNotFound.drop_reason(),
        Some(crate::engine::resolvers::DropReason::InvalidEvent)
    );
}

/// 已撤销的安装 ⇒ 仍然返回 `ResolvedInstallation`，但 `active = false`
/// （Router 据此记 `revoked_installation` 丢弃）。
#[tokio::test]
async fn revoked_installation_resolves_inactive() {
    let mut revoked = installation();
    revoked.status = "revoked".to_string();
    let queries = Arc::new(FakeInstallationQueries {
        found: Some(revoked),
        fail: false,
        seen_app_id: Mutex::new(Vec::new()),
    });
    let resolver =
        LarkInstallationResolver::new(Arc::clone(&queries) as Arc<dyn InstallationQueries>);
    let answer = resolver
        .resolve_installation(&message("cli_test", "oc-1", "", "ou_a"))
        .await
        .expect("应当解析出来");
    assert!(!answer.active);
}

/// 仓储报错 ⇒ 基础设施失败（不是产品判决）。
#[tokio::test]
async fn repository_failure_is_an_infrastructure_error() {
    let queries = Arc::new(FakeInstallationQueries {
        found: None,
        fail: true,
        seen_app_id: Mutex::new(Vec::new()),
    });
    let resolver =
        LarkInstallationResolver::new(Arc::clone(&queries) as Arc<dyn InstallationQueries>);
    let error = resolver
        .resolve_installation(&message("cli_test", "oc-1", "", "ou_a"))
        .await
        .expect_err("应当报基础设施错");
    assert!(matches!(error, EngineError::Infra { .. }), "{error:?}");
}

// =====================================================================
// 三、身份解析
// =====================================================================

struct FakeIdentityQueries {
    binding: Option<LarkUserBindingRow>,
    member: bool,
    sees: Mutex<Vec<(Id, String)>>,
}

#[async_trait]
impl IdentityQueries for FakeIdentityQueries {
    async fn find_user_binding(
        &self,
        installation_id: Id,
        lark_open_id: &str,
    ) -> Result<Option<LarkUserBindingRow>, RepoError> {
        self.sees
            .lock()
            .expect("lock")
            .push((installation_id, lark_open_id.to_string()));
        Ok(self.binding.clone())
    }

    async fn is_workspace_member(
        &self,
        _workspace_id: Id,
        _user_id: Id,
    ) -> Result<bool, RepoError> {
        Ok(self.member)
    }
}

fn binding(user_id: Uuid) -> LarkUserBindingRow {
    LarkUserBindingRow {
        id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        multica_user_id: user_id,
        installation_id: Uuid::new_v4(),
        lark_open_id: "ou_a".to_string(),
        union_id: None,
        bound_at: Utc::now(),
    }
}

/// 已绑定的 workspace 成员 ⇒ 解析出 Multica 用户。
#[tokio::test]
async fn bound_member_resolves() {
    let user_id = Uuid::new_v4();
    let queries = Arc::new(FakeIdentityQueries {
        binding: Some(binding(user_id)),
        member: true,
        sees: Mutex::new(Vec::new()),
    });
    let resolver = LarkIdentityResolver::new(Arc::clone(&queries) as Arc<dyn IdentityQueries>);
    let answer = resolver
        .resolve_sender(
            &installation_row(),
            &message("cli_test", "oc-1", "", "ou_a"),
        )
        .await
        .expect("应当解析出来");
    assert_eq!(answer.user_id, Id(user_id));
    // 查绑定用的是**安装 + open_id** 两个键。
    let sees = queries.sees.lock().expect("lock");
    assert_eq!(sees.len(), 1);
    assert_eq!(sees[0].1, "ou_a");
}

/// 没绑定 ⇒ `SenderUnbound`（**不是**错误；Router 据此回绑定卡）。
#[tokio::test]
async fn unbound_sender_is_a_product_decision() {
    let queries = Arc::new(FakeIdentityQueries {
        binding: None,
        member: true,
        sees: Mutex::new(Vec::new()),
    });
    let resolver = LarkIdentityResolver::new(Arc::clone(&queries) as Arc<dyn IdentityQueries>);
    let error = resolver
        .resolve_sender(
            &installation_row(),
            &message("cli_test", "oc-1", "", "ou_a"),
        )
        .await
        .expect_err("应当判未绑定");
    assert_eq!(error, EngineError::Pipeline(PipelineError::SenderUnbound));
    assert_eq!(
        PipelineError::SenderUnbound.drop_reason(),
        None,
        "未绑定是 `needs_binding` 判决，不是丢弃"
    );
}

/// 已绑定但**不是** workspace 成员 ⇒ `SenderNotMember` ⇒ `non_workspace_member` 丢弃。
#[tokio::test]
async fn bound_non_member_is_dropped() {
    let queries = Arc::new(FakeIdentityQueries {
        binding: Some(binding(Uuid::new_v4())),
        member: false,
        sees: Mutex::new(Vec::new()),
    });
    let resolver = LarkIdentityResolver::new(Arc::clone(&queries) as Arc<dyn IdentityQueries>);
    let error = resolver
        .resolve_sender(
            &installation_row(),
            &message("cli_test", "oc-1", "", "ou_a"),
        )
        .await
        .expect_err("应当判非成员");
    assert_eq!(error, EngineError::Pipeline(PipelineError::SenderNotMember));
    assert_eq!(
        PipelineError::SenderNotMember.drop_reason(),
        Some(crate::engine::resolvers::DropReason::NonWorkspaceMember)
    );
}

/// 测试用的 `ResolvedInstallation`（只需要 id / workspace）。
fn installation_row() -> crate::engine::resolvers::ResolvedInstallation {
    let installed = installation();
    crate::engine::resolvers::ResolvedInstallation {
        id: installed.id,
        workspace_id: installed.workspace_id,
        agent_id: installed.agent_id,
        installer_user_id: installed.installer_user_id,
        active: true,
        kind: TYPE_LARK,
        platform: Some(Arc::new(installed)),
    }
}

// =====================================================================
// 四、会话隔离键
// =====================================================================

/// 上游 `larkSessionRouting`：p2p / 顶层群消息 = chat id；话题里的消息 = `chat:话题`。
#[test]
fn session_routing_isolates_by_topic_only() {
    let direct = session_routing(&message("cli_test", "oc-1", "", "ou_a"));
    assert_eq!(direct.binding_key, "oc-1");

    let mut group_top = message("cli_test", "oc-1", "", "ou_a");
    group_top.source.chat_type = ChatType::Group;
    assert_eq!(session_routing(&group_top).binding_key, "oc-1");

    let mut group_topic = message("cli_test", "oc-1", "omt-1", "ou_a");
    group_topic.source.chat_type = ChatType::Group;
    assert_eq!(session_routing(&group_topic).binding_key, "oc-1:omt-1");

    // p2p 里带 `thread_id` 不隔离（1:1 只有一个会话）。
    let mut direct_threaded = message("cli_test", "oc-1", "omt-1", "ou_a");
    direct_threaded.source.chat_type = ChatType::P2p;
    assert_eq!(session_routing(&direct_threaded).binding_key, "oc-1");
}

// =====================================================================
// 五、判决 → 出站值
// =====================================================================

/// `engine::Outcome` → 本 adapter 的 `Outcome` 逐条 1:1。
#[test]
fn outcome_mapping_is_one_to_one() {
    use crate::engine::resolvers::Outcome as EngineOutcome;
    let cases = [
        (EngineOutcome::Dropped, Outcome::Dropped),
        (EngineOutcome::NeedsBinding, Outcome::NeedsBinding),
        (EngineOutcome::Ingested, Outcome::Ingested),
        (EngineOutcome::FreshPending, Outcome::FreshPending),
        (EngineOutcome::ChatStarted, Outcome::ChatStarted),
        (EngineOutcome::IssueUsage, Outcome::IssueUsage),
        (EngineOutcome::AgentOffline, Outcome::AgentOffline),
        (EngineOutcome::AgentArchived, Outcome::AgentArchived),
    ];
    for (engine, expected) in cases {
        assert_eq!(Outcome::from_engine(&engine), expected, "{engine:?}");
    }
    assert_eq!(Outcome::default(), Outcome::Dropped, "默认判决不触发回复");
}

/// `dispatch_result_from_engine`：字段逐个搬（含 `/issue` 的标题与标识符）。
#[test]
fn dispatch_result_carries_the_issue_fields() {
    let installation_id = Id::new();
    let session_id = Id::new();
    let issue_id = Id::new();
    let mut route = RouteResult {
        outcome: crate::engine::resolvers::Outcome::Ingested,
        installation_id: Some(installation_id),
        chat_session_id: Some(session_id),
        sender: "ou_a".to_string(),
        issue_identifier: "MUL-42".to_string(),
        issue_workspace_slug: "acme".to_string(),
        issue: Some(crate::engine::resolvers::ChannelIssue {
            id: issue_id,
            number: 42,
            title: "标题".to_string(),
        }),
        ..RouteResult::default()
    };
    route.drop_reason = None;

    let dispatch = dispatch_result_from_engine(&route);
    assert_eq!(dispatch.outcome, Outcome::Ingested);
    assert_eq!(dispatch.installation_id, Some(installation_id));
    assert_eq!(dispatch.chat_session_id, Some(session_id));
    assert_eq!(dispatch.sender_open_id, "ou_a");
    assert_eq!(dispatch.issue_id, Some(issue_id));
    assert_eq!(dispatch.issue_number, 42);
    assert_eq!(dispatch.issue_identifier, "MUL-42");
    assert_eq!(dispatch.issue_workspace_slug, "acme");
    assert_eq!(dispatch.issue_title, "标题");
    assert!(!dispatch.issue_duplicate);
    assert!(!dispatch.issue_usage_had_media);

    // 没有 issue 的判决 ⇒ issue 字段留零值（不是 `None` 崩）。
    let dropped = dispatch_result_from_engine(&RouteResult::dropped(
        crate::engine::resolvers::DropReason::Duplicate,
        None,
    ));
    assert_eq!(dropped.outcome, Outcome::Dropped);
    assert_eq!(
        dropped.drop_reason,
        Some(crate::engine::resolvers::DropReason::Duplicate)
    );
    assert_eq!(dropped.issue_id, None);
    assert_eq!(dropped.issue_number, 0);
    assert_eq!(dropped.issue_title, "");
}

/// `/issue` 的 `origin_type` 逐字 `lark_chat`（上游：不随 cutover 改名）。
#[test]
fn origin_type_is_verbatim() {
    assert_eq!(origin_type(), "lark_chat");
    assert_eq!(ORIGIN_LARK_CHAT, "lark_chat");
    assert_eq!(kind(), mc_core::channel::ChannelKind::Lark);
    assert_eq!(TYPE_LARK.storage_str(), "feishu", "存库口径是 feishu");
}
