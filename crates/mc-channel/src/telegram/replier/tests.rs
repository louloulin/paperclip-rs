//! `telegram::replier` 的用例（写者 M7-5）。
//!
//! 上游 `replier_test.go` 的四条（群聊不发 bearer 链接、私聊发引用链接、命令与 issue 判决、
//! 只有被寻址的 `/issue` 才回）逐条移植，外加凭据面与纯函数的边界用例。
//! 全部注入端口替身 ⇒ 不需要库、不需要网络。

use std::sync::Mutex;

use super::*;
use crate::engine::resolvers::{DropReason, Outcome};
use crate::telegram::api::{ApiError, ApiResult, WebhookInfo};
use crate::telegram::inbound::{Chat, Message, Update, User};
use crate::telegram::resolvers::InstallationRow;
use crate::telegram::TYPE_TELEGRAM;

// ---------------------------------------------------------------------------
// 装置
// ---------------------------------------------------------------------------

/// 记下发出去的消息的 API 替身。
#[derive(Default)]
struct RecordingApi {
    sent: Mutex<Vec<SendMessage>>,
    fail: bool,
}

impl RecordingApi {
    fn failing() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    fn sent(&self) -> Vec<SendMessage> {
        self.sent.lock().expect("sent").clone()
    }
}

#[async_trait]
impl TelegramApi for RecordingApi {
    async fn get_me(&self, _bot_token: &str) -> ApiResult<User> {
        Err(ApiError::Malformed { method: "getMe" })
    }

    async fn get_webhook_info(&self, _bot_token: &str) -> ApiResult<WebhookInfo> {
        Err(ApiError::Malformed {
            method: "getWebhookInfo",
        })
    }

    async fn get_updates(&self, _bot_token: &str, _offset: i64) -> ApiResult<Vec<Update>> {
        Err(ApiError::Malformed {
            method: "getUpdates",
        })
    }

    async fn send_message(&self, _bot_token: &str, params: &SendMessage) -> ApiResult<Message> {
        self.sent.lock().expect("sent").push(params.clone());
        if self.fail {
            return Err(ApiError::Transport {
                method: "sendMessage",
            });
        }
        Ok(Message {
            message_id: 5,
            ..Message::default()
        })
    }

    async fn send_chat_action(
        &self,
        _bot_token: &str,
        _chat_id: i64,
        _message_thread_id: i64,
    ) -> ApiResult<()> {
        Ok(())
    }
}

/// 铸令牌替身（`None` = 没接绑定服务）。
#[derive(Default)]
struct StubMinter {
    fail: bool,
}

#[async_trait]
impl BindingMinter for StubMinter {
    async fn mint(
        &self,
        _workspace_id: Id,
        _installation_id: Id,
        channel_user_id: &str,
    ) -> Result<MintedBinding, String> {
        if self.fail {
            return Err("mint boom".to_string());
        }
        Ok(MintedBinding {
            raw: format!("token-for-{channel_user_id}"),
            expires_at: Utc::now(),
        })
    }
}

/// 一条**密文可解**的安装行（身份解密器把 base64 当明文）。
fn installation() -> InstallationRow {
    use base64::Engine as _;

    InstallationRow {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: serde_json::json!({
            "app_id": "999",
            "bot_username": "my_bot",
            "bot_token_encrypted": base64::engine::general_purpose::STANDARD.encode("123:token"),
        }),
    }
}

/// 把安装行挂成 `ResolvedInstallation`（不透明平台值）。
fn resolved(row: &InstallationRow) -> ResolvedInstallation {
    let mut installation = ResolvedInstallation::new(
        row.id,
        row.workspace_id,
        row.agent_id,
        row.installer_user_id,
        TYPE_TELEGRAM,
        true,
    );
    installation.platform = Some(Arc::new(row.clone()));
    installation
}

/// 一条入站消息（私聊或群聊，可带话题）。
fn message(chat_type: &str, thread_id: i64, message_id: i64) -> InboundMessage {
    let update = Update {
        update_id: 1,
        message: Some(Box::new(Message {
            message_id,
            from: Some(User {
                id: 111,
                first_name: "Ada".to_string(),
                ..User::default()
            }),
            chat: Chat {
                id: -100,
                chat_type: chat_type.to_string(),
            },
            text: "hello".to_string(),
            message_thread_id: thread_id,
            is_topic_message: thread_id != 0,
            ..Message::default()
        })),
    };
    crate::telegram::inbound::inbound_from_update(&update, 999, "my_bot").expect("accepted")
}

/// 一条只有判决的路由结论。
fn result(outcome: Outcome) -> RouteResult {
    RouteResult {
        outcome,
        sender: "111".to_string(),
        ..RouteResult::default()
    }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 上游 `TestReplyNeedsBindingDoesNotMintBearerLinkInGroup`：群聊只回一条指路，**不铸令牌**。
#[tokio::test]
async fn a_group_never_gets_a_bearer_link() {
    let api = Arc::new(RecordingApi::default());
    let replier = TelegramOutboundReplier::new(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
        Some(Arc::new(StubMinter::default())),
        "https://app.example",
        None,
    );
    let row = installation();
    replier
        .reply_now(
            &resolved(&row),
            &message("supergroup", 0, 7),
            &result(Outcome::NeedsBinding),
        )
        .await;
    let sent = api.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].text, BINDING_GROUP_HINT);
    assert!(
        !sent[0].text.contains("token="),
        "群里绝不出 token：{}",
        sent[0].text
    );
    assert_eq!(sent[0].chat_id, -100);
    assert_eq!(
        sent[0].reply_to_message_id, 7,
        "引用触发它的那条消息（允许被引用消息已删）"
    );
    assert!(sent[0].allow_sending_without_reply);
}

/// 上游 `TestReplyNeedsBindingInPrivateChatMintsQuotedLink`：私聊铸令牌 + 拼绑定 URL，
/// 且话题 / 引用参数照传。
#[tokio::test]
async fn a_private_chat_gets_the_redeem_link() {
    let api = Arc::new(RecordingApi::default());
    let replier = TelegramOutboundReplier::new(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
        Some(Arc::new(StubMinter::default())),
        "https://app.example/",
        None,
    );
    let row = installation();
    replier
        .reply_now(
            &resolved(&row),
            &message("private", 0, 7),
            &result(Outcome::NeedsBinding),
        )
        .await;
    let sent = api.sent();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0]
            .text
            .contains("https://app.example/telegram/bind?token=token-for-111",),
        "{}",
        sent[0].text
    );
    assert!(sent[0].text.contains(BINDING_LINK_TTL_HINT));
    assert_eq!(sent[0].parse_mode, "", "判决回复走纯文本");

    // 绑定面缺席 / 没配 app url ⇒ 该提示被跳过（其余告知照发）。
    let notices = TelegramOutboundReplier::notices_only(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
    );
    notices
        .reply_now(
            &resolved(&row),
            &message("private", 0, 7),
            &result(Outcome::NeedsBinding),
        )
        .await;
    assert_eq!(api.sent().len(), 1, "没有绑定面就不发绑定卡");

    // 铸令牌失败也只告警（回复器不在 ACK 路径上）。
    let failing = TelegramOutboundReplier::new(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
        Some(Arc::new(StubMinter { fail: true })),
        "https://app.example",
        None,
    );
    failing
        .reply_now(
            &resolved(&row),
            &message("private", 0, 7),
            &result(Outcome::NeedsBinding),
        )
        .await;
    assert_eq!(api.sent().len(), 1);
}

/// 上游 `TestReplyCoversCommandAndIssueOutcomes`：每个判决各自的文案；普通聊天**沉默**。
#[tokio::test]
async fn every_outcome_maps_to_its_text_and_plain_chat_stays_silent() {
    let api = Arc::new(RecordingApi::default());
    let replier = TelegramOutboundReplier::notices_only(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
    );
    let row = installation();
    let installation = resolved(&row);

    for (outcome, wanted) in [
        (Outcome::AgentOffline, AGENT_OFFLINE_TEXT),
        (Outcome::AgentArchived, AGENT_ARCHIVED_TEXT),
        (Outcome::FreshPending, FRESH_PENDING_TEXT),
        (Outcome::ChatStarted, CHAT_STARTED_TEXT),
        (Outcome::IssueUsage, ISSUE_USAGE_TEXT),
    ] {
        replier
            .reply_now(&installation, &message("private", 0, 7), &result(outcome))
            .await;
        assert_eq!(api.sent().last().expect("sent").text, wanted);
    }

    let before = api.sent().len();
    replier
        .reply_now(
            &installation,
            &message("private", 0, 7),
            &result(Outcome::Ingested),
        )
        .await;
    assert_eq!(api.sent().len(), before, "普通聊天消息保持沉默");

    // 带 issue 的 Ingested ⇒ 创建 / 重复两条文案。
    let issue = ChannelIssue {
        id: Id::new(),
        number: 12,
        title: "fix login".to_string(),
    };
    let created = RouteResult {
        outcome: Outcome::Ingested,
        issue: Some(issue.clone()),
        issue_identifier: "ABC-12".to_string(),
        ..RouteResult::default()
    };
    replier
        .reply_now(&installation, &message("private", 0, 7), &created)
        .await;
    assert_eq!(
        api.sent().last().expect("sent").text,
        "✅ Created ABC-12 — fix login"
    );

    let duplicate = RouteResult {
        issue_duplicate: true,
        ..created
    };
    replier
        .reply_now(&installation, &message("private", 0, 7), &duplicate)
        .await;
    assert_eq!(
        api.sent().last().expect("sent").text,
        "⚠️ Not created — active issue ABC-12 already exists: fix login"
    );

    // 没有 identifier ⇒ 降到 `#<number>`；没有标题 ⇒ 只回标识符。
    let bare = RouteResult {
        outcome: Outcome::Ingested,
        issue: Some(ChannelIssue {
            id: Id::new(),
            number: 12,
            title: "  ".to_string(),
        }),
        issue_identifier: String::new(),
        ..RouteResult::default()
    };
    replier
        .reply_now(&installation, &message("private", 0, 7), &bare)
        .await;
    assert_eq!(api.sent().last().expect("sent").text, "✅ Created #12");
}

/// 上游 `TestDroppedReplyOnlyAnswersAddressedIssueCommands`：只有**被寻址的 `/issue`**
/// 被拒时才回，且只对两条原因。
#[tokio::test]
async fn dropped_replies_only_cover_addressed_issue_commands() {
    let api = Arc::new(RecordingApi::default());
    let replier = TelegramOutboundReplier::notices_only(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
    );
    let row = installation();
    let installation = resolved(&row);

    // 未被寻址 ⇒ 不回（哪怕它长得像 /issue）。
    let plain = crate::telegram::inbound::inbound_from_update(
        &Update {
            update_id: 1,
            message: Some(Box::new(Message {
                message_id: 3,
                from: Some(User {
                    id: 111,
                    first_name: "U".to_string(),
                    ..User::default()
                }),
                chat: Chat {
                    id: -100,
                    chat_type: "supergroup".to_string(),
                },
                text: "/issue something".to_string(),
                ..Message::default()
            })),
        },
        999,
        "my_bot",
    )
    .expect("accepted");
    assert!(!is_addressed_issue_command(&plain));
    replier
        .reply_now(
            &installation,
            &plain,
            &RouteResult::dropped(DropReason::NonWorkspaceMember, None),
        )
        .await;
    assert!(api.sent().is_empty(), "未被寻址的 /issue 被拒不回");

    // 被寻址 + 非成员 / 已撤销 ⇒ 各回一条。
    let addressed = crate::telegram::inbound::inbound_from_update(
        &Update {
            update_id: 1,
            message: Some(Box::new(Message {
                message_id: 4,
                from: Some(User {
                    id: 111,
                    first_name: "U".to_string(),
                    ..User::default()
                }),
                chat: Chat {
                    id: -100,
                    chat_type: "private".to_string(),
                },
                text: "/issue something".to_string(),
                ..Message::default()
            })),
        },
        999,
        "my_bot",
    )
    .expect("accepted");
    assert!(is_addressed_issue_command(&addressed));

    for (reason, wanted) in [
        (DropReason::NonWorkspaceMember, ISSUE_NOT_MEMBER_TEXT),
        (DropReason::RevokedInstallation, ISSUE_DISABLED_TEXT),
    ] {
        replier
            .reply_now(
                &installation,
                &addressed,
                &RouteResult::dropped(reason, Some(installation.id)),
            )
            .await;
        assert_eq!(api.sent().last().expect("sent").text, wanted);
    }

    // 别的丢弃原因（例如重复）⇒ 沉默。
    let before = api.sent().len();
    replier
        .reply_now(
            &installation,
            &addressed,
            &RouteResult::dropped(DropReason::Duplicate, Some(installation.id)),
        )
        .await;
    assert_eq!(api.sent().len(), before);
    assert_eq!(
        dropped_reply_text(
            &RouteResult::dropped(DropReason::NonWorkspaceMember, None),
            &plain
        ),
        "",
        "未被寻址 ⇒ 空文案"
    );
}

/// 发送失败只告警（回复器不在 ACK 路径上）；话题与引用参数照传。
#[tokio::test]
async fn a_failed_send_only_warns_and_topics_are_preserved() {
    let api = Arc::new(RecordingApi::failing());
    let replier = TelegramOutboundReplier::notices_only(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
    );
    let row = installation();
    replier
        .reply_now(
            &resolved(&row),
            &message("supergroup", 77, 9),
            &result(Outcome::AgentOffline),
        )
        .await;
    let sent = api.sent();
    assert_eq!(sent.len(), 1, "失败也照样尝试过一次");
    assert_eq!(sent[0].message_thread_id, 77);
    assert_eq!(sent[0].reply_to_message_id, 9);

    // 解不开密文 ⇒ 失败关闭（不把密文当令牌发出去）。
    let fail_closed = TelegramOutboundReplier::notices_only(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::fail_closed(),
    );
    fail_closed
        .reply_now(
            &resolved(&row),
            &message("private", 0, 9),
            &result(Outcome::AgentOffline),
        )
        .await;
    assert_eq!(api.sent().len(), 1, "解密失败必须失败关闭");

    // 没有不透明平台行 ⇒ 也失败关闭。
    let bare = ResolvedInstallation::new(
        row.id,
        row.workspace_id,
        row.agent_id,
        row.installer_user_id,
        TYPE_TELEGRAM,
        true,
    );
    fail_closed
        .reply_now(
            &bare,
            &message("private", 0, 9),
            &result(Outcome::AgentOffline),
        )
        .await;
    assert_eq!(api.sent().len(), 1);
}

/// 同步接缝把工作推给脱离任务（engine 的调用点不阻塞）。
#[tokio::test]
async fn the_sync_seam_spawns_a_detached_reply() {
    let api = Arc::new(RecordingApi::default());
    let replier = Arc::new(TelegramOutboundReplier::notices_only(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
    ));
    let row = installation();
    OutboundReplier::reply(
        replier.as_ref(),
        &resolved(&row),
        &message("private", 0, 9),
        &result(Outcome::AgentOffline),
    );
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert_eq!(api.sent().len(), 1, "脱离任务最终会发出去");
}

/// 纯函数：`url_encode` 的字符集、issue 文案的标识符回落、`Debug` 不含令牌。
#[test]
fn helpers_and_debug_are_credential_safe() {
    assert_eq!(url_encode("abc-def_ghi"), "abc-def_ghi");
    assert_eq!(url_encode("a b"), "a+b");
    assert_eq!(url_encode("a/b?c"), "a%2Fb%3Fc");
    assert_eq!(url_encode("é"), "%C3%A9");

    let issue = ChannelIssue {
        id: Id::new(),
        number: 7,
        title: " x ".to_string(),
    };
    assert_eq!(issue_created_text(&issue, "ABC-7"), "✅ Created ABC-7 — x");
    assert_eq!(
        issue_duplicate_text(&issue, ""),
        "⚠️ Not created — active issue #7 already exists: x"
    );
    assert_eq!(issue_result_identifier(&issue, "ABC-7"), "ABC-7");
    assert_eq!(issue_result_identifier(&issue, ""), "#7");

    let api: Arc<dyn TelegramApi> = Arc::new(RecordingApi::default());
    let replier = TelegramOutboundReplier::new(
        api,
        Decrypter::plaintext(),
        Some(Arc::new(StubMinter::default())),
        "https://app.example",
        Some("telegram/bind"),
    );
    let rendered = format!("{replier:?}");
    assert!(!rendered.contains("token"), "{rendered}");
    assert!(rendered.contains("/telegram/bind"));
    assert!(rendered.contains("<dyn TelegramApi>"));
    assert_eq!(DEFAULT_BINDING_PATH, "/telegram/bind");
}
