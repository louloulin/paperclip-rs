//! `dingtalk::replier` 的用例（写者 M7-8）。
//!
//! 三段：
//!
//! 1. **判决 → 文案**逐条钉住（含"普通聊天消息保持沉默"与两条 `/issue` 拒绝）；
//! 2. **绑定卡**：四条前置失败各一条 + "链接只走私聊"（群里发会把令牌暴露给全群）；
//! 3. **issue 标识符**：有 slug 才成链、括号被百分号化、`ABC-42` / `#42` / UUID 三级回退。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::id::Id;

use super::*;
use crate::dingtalk::inbound::TYPE_DINGTALK;
use crate::dingtalk::outbound::{
    set_api_base, DingTalkApiError, OpenApiTransport, PATH_SEND_GROUP, PATH_SEND_P2P,
};
use crate::dingtalk::resolvers::InstallationRow;
use crate::dingtalk::stream::AppSecret;
use crate::engine::resolvers::{ChannelIssue, Outcome, RouteResult};

// =====================================================================
// 替身
// =====================================================================

#[derive(Default)]
struct RecordingTransport {
    posts: Mutex<Vec<(&'static str, serde_json::Value)>>,
    tokens: Mutex<Vec<String>>,
    fail: Mutex<bool>,
}

impl RecordingTransport {
    fn failing() -> Self {
        let this = Self::default();
        *this.fail.lock().expect("lock") = true;
        this
    }

    fn posts(&self) -> Vec<(&'static str, serde_json::Value)> {
        self.posts.lock().expect("lock").clone()
    }

    fn tokens(&self) -> Vec<String> {
        self.tokens.lock().expect("lock").clone()
    }
}

#[async_trait]
impl OpenApiTransport for RecordingTransport {
    async fn access_token(
        &self,
        _app_key: &str,
        _app_secret: &AppSecret,
    ) -> Result<String, DingTalkApiError> {
        self.tokens.lock().expect("lock").push("token".to_string());
        Ok("token".to_string())
    }

    fn invalidate(&self, _app_key: &str) {}

    async fn post_json(
        &self,
        path: &'static str,
        _access_token: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, DingTalkApiError> {
        if *self.fail.lock().expect("lock") {
            return Err(DingTalkApiError::Transport { path });
        }
        self.posts.lock().expect("lock").push((path, body));
        Ok(serde_json::json!({ "processQueryKey": "k" }))
    }
}

#[derive(Default)]
struct FakeMinter {
    calls: Mutex<Vec<(Id, Id, String)>>,
    fail: Mutex<bool>,
}

#[async_trait]
impl BindingMinter for FakeMinter {
    async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<MintedBinding, String> {
        self.calls.lock().expect("lock").push((
            workspace_id,
            installation_id,
            channel_user_id.to_string(),
        ));
        if *self.fail.lock().expect("lock") {
            return Err("token store down".to_string());
        }
        Ok(MintedBinding {
            raw: "RAW-TOKEN-abc".to_string(),
            workspace_id,
            installation_id,
        })
    }
}

fn resolved(config: serde_json::Value) -> ResolvedInstallation {
    let mut installation = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        TYPE_DINGTALK,
        true,
    );
    installation.platform = Some(Arc::new(InstallationRow {
        id: installation.id,
        workspace_id: installation.workspace_id,
        agent_id: installation.agent_id,
        installer_user_id: installation.installer_user_id,
        status: "active".to_string(),
        config,
    }));
    installation
}

fn configuration() -> serde_json::Value {
    serde_json::json!({ "app_id": "app-key", "app_secret": "SUPER-SECRET-VALUE" })
}

fn inbound(chat_type: ChatType) -> InboundMessage {
    InboundMessage {
        event_id: "ev".to_string(),
        message_id: "m1".to_string(),
        source: Source {
            channel_type: TYPE_DINGTALK,
            chat_id: "chat".to_string(),
            chat_type,
            sender_id: "staff-1".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Text,
        text: "hello".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::json!({ "app_id": "app-key" }),
    }
}

fn result(outcome: Outcome) -> RouteResult {
    RouteResult {
        outcome,
        ..RouteResult::default()
    }
}

fn build_replier(
    transport: &Arc<RecordingTransport>,
    minter: Option<Arc<dyn BindingMinter>>,
) -> DingTalkOutboundReplier {
    DingTalkOutboundReplier::new(OutboundReplierConfig {
        binding: minter,
        decrypt: Decrypter::fail_closed(),
        transport: Arc::clone(transport) as Arc<dyn OpenApiTransport>,
        app_url: "https://app.example.test".to_string(),
        binding_path: String::new(),
    })
}

fn body_text(post: &(&'static str, serde_json::Value)) -> String {
    let param: serde_json::Value = serde_json::from_str(
        post.1["msgParam"]
            .as_str()
            .expect("msgParam 是 JSON 字符串"),
    )
    .expect("msgParam");
    param["text"].as_str().expect("text").to_string()
}

/// 群回复的正文**带**引用块（上游 `targetFromMessage` 把可见引用拼进来）⇒
/// 断言文案本身时要先剥掉它。
fn answer_of(text: &str) -> String {
    text.split_once("\n\n---\n\n")
        .map_or_else(|| text.to_string(), |(_, answer)| answer.to_string())
}

// =====================================================================
// 判决 → 文案
// =====================================================================

/// 四条状态告知各一条；`IssueUsage` 按"有没有媒体"换文案。
#[tokio::test]
async fn the_status_notices_match_upstream() {
    for (outcome, want) in [
        (Outcome::AgentOffline, AGENT_OFFLINE_TEXT),
        (Outcome::AgentArchived, AGENT_ARCHIVED_TEXT),
        (Outcome::FreshPending, FRESH_PENDING_TEXT),
        (Outcome::ChatStarted, CHAT_STARTED_TEXT),
        (Outcome::IssueUsage, ISSUE_USAGE_TEXT),
    ] {
        let transport = Arc::new(RecordingTransport::default());
        let replier = build_replier(&transport, None);
        replier
            .reply_now(
                &resolved(configuration()),
                &inbound(ChatType::Group),
                &result(outcome),
            )
            .await;
        let posts = transport.posts();
        assert_eq!(posts.len(), 1, "{outcome:?}");
        assert_eq!(posts[0].0, PATH_SEND_GROUP);
        assert_eq!(answer_of(&body_text(&posts[0])), want, "{outcome:?}");
    }

    // `/issue` 缺标题 + 带了图片 ⇒ 另一条文案。
    let transport = Arc::new(RecordingTransport::default());
    let replier = build_replier(&transport, None);
    let mut usage = result(Outcome::IssueUsage);
    usage.issue_usage_had_media = true;
    replier
        .reply_now(
            &resolved(configuration()),
            &inbound(ChatType::Group),
            &usage,
        )
        .await;
    assert_eq!(
        answer_of(&body_text(&transport.posts()[0])),
        ISSUE_USAGE_WITH_MEDIA_TEXT
    );
}

/// 普通聊天消息（`Ingested` 但没有 issue）**保持沉默**；带 issue 才回。
#[tokio::test]
async fn a_plain_ingested_turn_stays_silent() {
    let transport = Arc::new(RecordingTransport::default());
    let replier = build_replier(&transport, None);
    replier
        .reply_now(
            &resolved(configuration()),
            &inbound(ChatType::Group),
            &result(Outcome::Ingested),
        )
        .await;
    assert!(transport.posts().is_empty(), "普通聊天不回复");

    let mut created = result(Outcome::Ingested);
    created.issue = Some(ChannelIssue {
        id: Id::new(),
        number: 42,
        title: "Fix the thing".to_string(),
    });
    created.issue_identifier = "ABC-42".to_string();
    created.issue_workspace_slug = "acme".to_string();
    replier
        .reply_now(
            &resolved(configuration()),
            &inbound(ChatType::Group),
            &created,
        )
        .await;
    let posts = transport.posts();
    assert_eq!(posts.len(), 1);
    let text = answer_of(&body_text(&posts[0]));
    assert!(text.starts_with("✅ Created ["), "{text}");
    assert!(text.contains("Fix the thing"), "{text}");

    // 重复守卫 ⇒ 另一条文案。
    let mut duplicate = created.clone();
    duplicate.issue_duplicate = true;
    replier
        .reply_now(
            &resolved(configuration()),
            &inbound(ChatType::Group),
            &duplicate,
        )
        .await;
    let posts = transport.posts();
    assert_eq!(posts.len(), 2);
    assert!(answer_of(&body_text(&posts[1])).starts_with("⚠️ Not created — active issue"));
}

/// `Dropped`：只有**被寻址的** `/issue` 被拒时才回；重复 / 群聊闲聊保持沉默。
#[tokio::test]
async fn dropped_only_replies_for_an_addressed_issue_command() {
    let transport = Arc::new(RecordingTransport::default());
    let replier = build_replier(&transport, None);

    let mut duplicate = result(Outcome::Dropped);
    duplicate.drop_reason = Some(crate::engine::DropReason::Duplicate);
    let mut addressed = inbound(ChatType::Group);
    addressed.command_text = "/issue fix the thing".to_string();
    replier
        .reply_now(&resolved(configuration()), &addressed, &duplicate)
        .await;
    assert!(transport.posts().is_empty(), "重复丢弃保持沉默");

    let mut not_member = duplicate.clone();
    not_member.drop_reason = Some(crate::engine::DropReason::NonWorkspaceMember);
    replier
        .reply_now(&resolved(configuration()), &addressed, &not_member)
        .await;
    let posts = transport.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(answer_of(&body_text(&posts[0])), ISSUE_NOT_MEMBER_TEXT);

    let mut revoked = duplicate;
    revoked.drop_reason = Some(crate::engine::DropReason::RevokedInstallation);
    replier
        .reply_now(&resolved(configuration()), &addressed, &revoked)
        .await;
    assert_eq!(
        answer_of(&body_text(&transport.posts()[1])),
        ISSUE_DISABLED_TEXT
    );

    // 没有寻址 ⇒ 即使原因成立也不回。
    let mut unaddressed = addressed.clone();
    unaddressed.addressed_to_bot = false;
    replier
        .reply_now(&resolved(configuration()), &unaddressed, &revoked)
        .await;
    assert_eq!(transport.posts().len(), 2, "没寻址就不回");
}

/// `/issue` 命令判定：去掉命令前缀后仍要认得出（`command_text` 优先于 `text`）。
#[test]
fn the_addressed_issue_predicate_reads_the_command_text() {
    let mut message = inbound(ChatType::Group);
    assert!(!is_addressed_issue_command(&message), "普通文本不是命令");
    message.text = "/issue fix it".to_string();
    assert!(is_addressed_issue_command(&message));
    message.addressed_to_bot = false;
    assert!(!is_addressed_issue_command(&message), "没寻址不算");
    message.addressed_to_bot = true;
    message.command_text = "just chatting".to_string();
    assert!(!is_addressed_issue_command(&message), "命令文本优先");
}

// =====================================================================
// 绑定卡
// =====================================================================

/// 私聊绑定卡：令牌进 URL、目标走 `oToMessages/batchSend`、收件人是发件人。
#[tokio::test]
async fn the_binding_prompt_goes_private_with_the_token_in_the_url() {
    let transport = Arc::new(RecordingTransport::default());
    let minter = Arc::new(FakeMinter::default());
    let replier = build_replier(
        &transport,
        Some(Arc::clone(&minter) as Arc<dyn BindingMinter>),
    );
    let installation = resolved(configuration());
    let message = inbound(ChatType::Group);
    let mut needs_binding = result(Outcome::NeedsBinding);
    needs_binding.sender = "staff-9".to_string();
    replier
        .reply_now(&installation, &message, &needs_binding)
        .await;

    let calls = minter.calls.lock().expect("lock").clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, installation.workspace_id);
    assert_eq!(calls[0].1, installation.id);
    assert_eq!(calls[0].2, "staff-9");

    let posts = transport.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, PATH_SEND_P2P, "绑定卡**只**私聊发");
    assert_eq!(posts[0].1["userIds"], serde_json::json!(["staff-9"]));
    let text = body_text(&posts[0]);
    assert!(
        text.contains("https://app.example.test/dingtalk/bind?token=RAW-TOKEN-abc"),
        "{text}"
    );
    assert!(text.contains(BINDING_LINK_TTL_HINT), "{text}");
    assert!(!text.contains("\n\n---\n\n"), "私聊不引用：{text}");
}

/// 四条前置失败：缺发件人 / 没接绑定服务 / 没配 app url / 铸令牌失败。
#[tokio::test]
async fn the_binding_prompt_has_four_fail_closed_preconditions() {
    let installation = resolved(configuration());
    let message = inbound(ChatType::Group);

    // ① 缺发件人（`result.sender` 与消息都没有）。
    let transport = Arc::new(RecordingTransport::default());
    let replier = build_replier(&transport, None);
    let mut no_sender = message.clone();
    no_sender.source.sender_id = String::new();
    let error = replier
        .send_binding_prompt(&installation, &no_sender, &result(Outcome::NeedsBinding))
        .await
        .expect_err("缺发件人");
    assert_eq!(error, ReplierError::MissingSender);

    // ② 没接绑定服务。
    let error = replier
        .send_binding_prompt(&installation, &message, &result(Outcome::NeedsBinding))
        .await
        .expect_err("没接绑定服务");
    assert_eq!(error, ReplierError::BindingUnavailable);

    // ③ 没配 app url。
    let replier = DingTalkOutboundReplier::new(OutboundReplierConfig {
        binding: Some(Arc::new(FakeMinter::default()) as Arc<dyn BindingMinter>),
        decrypt: Decrypter::fail_closed(),
        transport: Arc::clone(&transport) as Arc<dyn OpenApiTransport>,
        app_url: String::new(),
        binding_path: DEFAULT_BINDING_PATH.to_string(),
    });
    let error = replier
        .send_binding_prompt(&installation, &message, &result(Outcome::NeedsBinding))
        .await
        .expect_err("没配 app url");
    assert_eq!(error, ReplierError::AppUrlUnavailable);

    // ④ 铸令牌失败。
    let minter = Arc::new(FakeMinter::default());
    *minter.fail.lock().expect("lock") = true;
    let replier = build_replier(
        &transport,
        Some(Arc::clone(&minter) as Arc<dyn BindingMinter>),
    );
    let error = replier
        .send_binding_prompt(&installation, &message, &result(Outcome::NeedsBinding))
        .await
        .expect_err("铸令牌失败");
    assert_eq!(error, ReplierError::Mint);
    assert!(transport.posts().is_empty(), "失败不该发出任何帧");
}

/// 凭据面：令牌明文**不**出现在任何 `Debug` / 错误文案里。
#[test]
fn the_plaintext_token_never_reaches_debug_or_errors() {
    let _minter = FakeMinter::default();
    let minted = MintedBinding {
        raw: "RAW-TOKEN-abc".to_string(),
        workspace_id: Id::new(),
        installation_id: Id::new(),
    };
    let rendered = format!("{minted:?}");
    assert!(!rendered.contains("RAW-TOKEN-abc"), "{rendered}");
    assert!(rendered.contains("<redacted>"));

    for error in [
        ReplierError::MissingSender,
        ReplierError::BindingUnavailable,
        ReplierError::AppUrlUnavailable,
        ReplierError::CredentialsUnavailable,
        ReplierError::Mint,
        ReplierError::Send { code: "transport" },
    ] {
        let text = format!("{error} {error:?}");
        assert!(!text.contains("RAW-TOKEN-abc"), "{text}");
        assert!(!text.contains("SUPER-SECRET-VALUE"), "{text}");
        assert!(!text.contains("token="), "{text}");
    }

    // 令牌与明文 secret 都不在回复器的 `Debug` 里。
    let replier = build_replier(
        &Arc::new(RecordingTransport::default()),
        Some(Arc::new(FakeMinter::default()) as Arc<dyn BindingMinter>),
    );
    let rendered = format!("{replier:?}");
    assert!(!rendered.contains("RAW-TOKEN-abc"), "{rendered}");
    assert!(!rendered.contains("SUPER-SECRET-VALUE"), "{rendered}");
}

/// 发送失败 ⇒ 只记 warn（不 panic、不把错误抛回 engine）。
#[tokio::test]
async fn a_transport_failure_only_logs() {
    let transport = Arc::new(RecordingTransport::failing());
    let replier = build_replier(&transport, None);
    replier
        .reply_now(
            &resolved(configuration()),
            &inbound(ChatType::Group),
            &result(Outcome::AgentOffline),
        )
        .await;
    assert!(transport.posts().is_empty());
    assert_eq!(transport.tokens().len(), 1, "试过一次（拿到令牌后再失败）");
}

/// 凭据解不开（密文 + 无解密器）⇒ 一条错误，不发帧。
#[tokio::test]
async fn undecodable_credentials_surface_a_clean_error() {
    let transport = Arc::new(RecordingTransport::default());
    let replier = build_replier(&transport, None);
    let encrypted = resolved(serde_json::json!({
        "app_id": "app-key",
        "app_secret_encrypted": "CIPHERTEXT-B64",
    }));
    let error = replier
        .post(&encrypted, &inbound(ChatType::Group), "hi")
        .await
        .expect_err("解不开");
    assert_eq!(error, ReplierError::CredentialsUnavailable);
    let text = format!("{error} {error:?}");
    assert!(!text.contains("CIPHERTEXT-B64"), "{text}");
    assert!(transport.posts().is_empty());
}

// =====================================================================
// 纯函数
// =====================================================================

/// issue 标识符：有 slug 才成链；括号百分号化；三级回退。
#[test]
fn the_issue_identifier_links_through_the_workspace_slug() {
    let mut result_ = result(Outcome::Ingested);
    result_.issue = Some(ChannelIssue {
        id: Id::new(),
        number: 42,
        title: "t".to_string(),
    });
    // 没有 slug ⇒ 不发链接（裸标识符）。
    assert_eq!(
        issue_markdown_identifier(&result_, "https://app.example.test"),
        "#42"
    );

    result_.issue_identifier = "ABC-42".to_string();
    assert_eq!(
        issue_markdown_identifier(&result_, "https://app.example.test"),
        "ABC-42"
    );

    result_.issue_workspace_slug = "acme".to_string();
    let linked = issue_markdown_identifier(&result_, "https://app.example.test");
    let id = result_.issue.as_ref().expect("issue").id;
    assert_eq!(
        linked,
        format!("[ABC\\-42](https://app.example.test/acme/issues/{id})"),
        "标识符按普通 Markdown 转义（`-` 也在表里，上游 `escapeMarkdownText`）"
    );

    // 基址带 userinfo / 查询串 / 片段 ⇒ 不发链接。
    for bad in [
        "https://user:pw@app.example.test",
        "https://app.example.test?x=1",
        "https://app.example.test#frag",
        "not a url",
    ] {
        assert_eq!(issue_markdown_identifier(&result_, bad), "ABC-42", "{bad}");
    }

    // 基址路径里的括号被百分号化（否则会终止 Markdown 链接目标）。
    let linked = issue_markdown_identifier(&result_, "https://app.example.test/a(b)");
    assert!(linked.contains("%28b%29"), "{linked}");

    // 标识符按普通 Markdown 转义（`*` 一类）。
    result_.issue_identifier = "A*B".to_string();
    result_.issue_workspace_slug = "acme".to_string();
    let linked = issue_markdown_identifier(&result_, "https://app.example.test");
    assert!(linked.starts_with(r"[A\*B]("), "{linked}");

    // 没有 identifier ⇒ `#number`；没有 number ⇒ UUID。
    result_.issue_identifier = String::new();
    assert_eq!(issue_result_identifier(&result_), "#42");
    let mut zero = result_.clone();
    zero.issue = Some(ChannelIssue {
        id,
        number: 0,
        title: String::new(),
    });
    assert_eq!(issue_result_identifier(&zero), id.to_string());
}

/// `percent_encode`：unreserved 原样、空格变 `+`、其余 `%XX`（上游 `url.QueryEscape`）。
#[test]
fn percent_encoding_matches_query_escape() {
    assert_eq!(percent_encode("abc-_.~"), "abc-_.~");
    assert_eq!(percent_encode("a b"), "a+b");
    assert_eq!(percent_encode("a/b+c"), "a%2Fb%2Bc");
    assert_eq!(percent_encode("界"), "%E7%95%8C");
    assert_eq!(
        binding_url("https://app.example.test/", "/dingtalk/bind", "a b"),
        "https://app.example.test/dingtalk/bind?token=a+b"
    );
}

/// `binding_path` 归一化：零值取默认、缺前导 `/` 补上。
#[test]
fn the_binding_path_is_normalized() {
    let transport = Arc::new(RecordingTransport::default());
    for (given, want) in [
        (String::new(), DEFAULT_BINDING_PATH),
        ("dingtalk/bind".to_string(), "/dingtalk/bind"),
        ("/custom".to_string(), "/custom"),
    ] {
        let replier = DingTalkOutboundReplier::new(OutboundReplierConfig {
            binding: None,
            decrypt: Decrypter::fail_closed(),
            transport: Arc::clone(&transport) as Arc<dyn OpenApiTransport>,
            app_url: "https://app.example.test/".to_string(),
            binding_path: given.clone(),
        });
        let rendered = format!("{replier:?}");
        assert!(rendered.contains(want), "{given} ⇒ {rendered}");
    }
}

/// 未配置的基址不会被本文件静默改动（`set_api_base` 只给真 HTTP 的用例用）。
#[test]
fn the_api_base_seam_stays_default_here() {
    set_api_base("http://127.0.0.1:1");
    assert!(crate::dingtalk::outbound::api_base().starts_with("http://127.0.0.1:1"));
    crate::dingtalk::outbound::reset_api_base();
    assert_eq!(
        crate::dingtalk::outbound::api_base(),
        crate::dingtalk::outbound::DEFAULT_API_BASE
    );
}
