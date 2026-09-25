//! `slack::history` 的用例（写者 M7-4）。
//!
//! 覆盖四层：**窗口/游标**（纯函数）、**两道过滤**（纯函数）、**摊平与命名**（纯函数）、
//! **读面**（注入两个替身端口的端到端读）。真库 / 真 Slack 都不需要。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::id::Id;
use serde_json::json;

use super::flatten::{
    flatten_blocks, flatten_slack_text, resolve_user_names, slack_display_name, truncate_chars,
    HistoryLabeler,
};
use super::text::{
    clamp_history_limit, filter_context_generation, filter_route_generation,
    history_cursor_reached_start, history_next_cursor, history_window, slack_ts_less,
};
use super::*;
use crate::slack::config::Decrypter;
use crate::slack::outbound::ApiResult;

// =====================================================================
// 替身
// =====================================================================

fn message(ts: &str, user: &str, text: &str) -> SlackMessage {
    SlackMessage {
        ts: ts.to_string(),
        user: user.to_string(),
        text: text.to_string(),
        ..SlackMessage::default()
    }
}

fn target() -> SlackTarget {
    SlackTarget {
        bot_token: "xoxb-test".to_string(),
        binding_id: Id(uuid::Uuid::from_u128(7)),
        channel_id: "C1".to_string(),
        thread_root: "100.1".to_string(),
        bot_user_id: "UBOT".to_string(),
        history_start: String::new(),
        history_end: String::new(),
        boundary_pending: false,
        route_revision: 1,
    }
}

struct FakeApi {
    history: Vec<SlackMessage>,
    replies: Vec<SlackMessage>,
    users: Vec<SlackUser>,
    fail_users: bool,
}

#[async_trait]
impl HistoryApi for FakeApi {
    async fn conversation_history(
        &self,
        _token: &str,
        _channel: &str,
        _window: &HistoryWindow,
    ) -> ApiResult<Vec<SlackMessage>> {
        Ok(self.history.clone())
    }

    async fn conversation_replies(
        &self,
        _token: &str,
        _channel: &str,
        _window: &HistoryWindow,
    ) -> ApiResult<Vec<SlackMessage>> {
        Ok(self.replies.clone())
    }

    async fn users_info(&self, _token: &str, _ids: &[String]) -> ApiResult<Vec<SlackUser>> {
        if self.fail_users {
            return Err(crate::slack::outbound::SlackApiError::Refused {
                method: "users.info",
                code: "missing_scope".to_string(),
            });
        }
        Ok(self.users.clone())
    }
}

struct FakeStore {
    binding: Option<BindingSnapshot>,
    installation: Option<InstallationSnapshot>,
    outbound: Vec<OutboundRow>,
}

#[async_trait]
impl HistoryStore for FakeStore {
    async fn current_binding(&self, _session_id: Id) -> Result<Option<BindingSnapshot>, String> {
        Ok(self.binding.clone())
    }

    async fn installation(
        &self,
        _installation_id: Id,
    ) -> Result<Option<InstallationSnapshot>, String> {
        Ok(self.installation.clone())
    }

    async fn outbound_for_binding(
        &self,
        _binding_id: Id,
        _route_revision: i64,
    ) -> Result<Vec<OutboundRow>, String> {
        Ok(self.outbound.clone())
    }
}

/// 一条绑定（隔离键 `C1#100.1`）+ 活跃安装 + 明文令牌（身份解密器）。
fn wired(binding: Option<BindingSnapshot>, installation_active: bool) -> FakeStore {
    FakeStore {
        binding: binding.or(Some(BindingSnapshot {
            id: Id(uuid::Uuid::from_u128(7)),
            installation_id: Id(uuid::Uuid::from_u128(9)),
            channel_chat_id: "C1#100.1".to_string(),
            channel_id: "C1".to_string(),
            last_thread_id: "100.1".to_string(),
            ..BindingSnapshot::default()
        })),
        installation: Some(InstallationSnapshot {
            config: json!({
                "app_id": "A1",
                "bot_user_id": "UBOT",
                "bot_token_encrypted": "eG94Yi10ZXN0",
            }),
            active: installation_active,
        }),
        outbound: Vec::new(),
    }
}

fn history(store: FakeStore, api: FakeApi) -> History {
    History::new(Arc::new(store), Arc::new(api), Decrypter::plaintext())
}

// =====================================================================
// 窗口 / 游标（纯函数）
// =====================================================================

#[test]
fn clamp_history_limit_uses_the_documented_default_and_cap() {
    assert_eq!(clamp_history_limit(0), DEFAULT_HISTORY_LIMIT);
    assert_eq!(clamp_history_limit(-3), DEFAULT_HISTORY_LIMIT);
    assert_eq!(clamp_history_limit(7), 7);
    assert_eq!(clamp_history_limit(999), MAX_HISTORY_LIMIT);
}

#[test]
fn slack_timestamps_compare_numerically_not_lexically() {
    // 字典序会说 "9.9" > "100.1"，数值序不会 —— 这正是上游 `parseSlackTS` 的理由。
    assert!(slack_ts_less("9.9", "100.1"));
    assert!(!slack_ts_less("100.1", "9.9"));
    assert!(!slack_ts_less("bad", "bad"));
}

#[test]
fn history_bounds_tightens_the_caller_window_with_the_binding() {
    let mut target = target();
    target.history_start = "50.0".to_string();
    target.history_end = "200.0".to_string();
    let (start, end) = history_bounds(
        &HistoryOptions {
            after: "10.0".to_string(),
            until: "900.0".to_string(),
            ..HistoryOptions::default()
        },
        &target,
    );
    assert_eq!(start, "50.0", "绑定的下界更晚 ⇒ 取它");
    assert_eq!(end, "200.0", "绑定的上界更早 ⇒ 取它");
    // 调用方的窗口更紧时不被放宽。
    let (start, end) = history_bounds(
        &HistoryOptions {
            after: "60.0".to_string(),
            until: "150.0".to_string(),
            ..HistoryOptions::default()
        },
        &target,
    );
    assert_eq!((start.as_str(), end.as_str()), ("60.0", "150.0"));
}

#[test]
fn history_window_switches_between_back_paging_and_window_reading() {
    let back_paging = history_window("300.0", "50.0", "200.0", "C1", 20);
    assert_eq!(back_paging.latest, "200.0");
    assert!(back_paging.oldest.is_empty());
    assert!(!back_paging.inclusive);
    let first_page = history_window("", "50.0", "200.0", "C1", 20);
    assert_eq!(first_page.oldest, "50.0");
    assert!(first_page.inclusive, "有边界时第一次读是闭区间");
}

#[test]
fn cursor_stops_at_the_history_start_and_on_a_short_page() {
    let full: Vec<SlackMessage> = (0..20)
        .map(|index| message(&format!("{index}.0"), "U1", "x"))
        .collect();
    assert_eq!(history_next_cursor(&full, 20, ""), "0.0");
    // 已到历史下界 ⇒ 不再给游标。
    assert!(history_next_cursor(&full, 20, "0.0").is_empty());
    // 不满一页 ⇒ 不再给游标。
    assert!(history_next_cursor(&full[..5], 20, "").is_empty());
    assert!(history_cursor_reached_start("10.0", "20.0"));
    assert!(!history_cursor_reached_start("30.0", "20.0"));
}

// =====================================================================
// 两道过滤（纯函数）
// =====================================================================

#[test]
fn route_generation_drops_control_acks_and_other_bindings() {
    let target = target();
    let own_binding = target.binding_id.to_string();
    let mut control_ack = message("10.0", "UBOT", "⏳ on it");
    control_ack.metadata_event_type = OUTBOUND_METADATA_EVENT.to_string();
    control_ack.metadata_event_payload =
        json!({ "kind": "control_ack", "binding_id": own_binding.clone() });
    let mut foreign = message("11.0", "UBOT", "other session reply");
    foreign.metadata_event_type = OUTBOUND_METADATA_EVENT.to_string();
    foreign.metadata_event_payload = json!({ "kind": "task_reply", "binding_id": "999" });
    let mut ours = message("12.0", "UBOT", "our reply");
    ours.metadata_event_type = OUTBOUND_METADATA_EVENT.to_string();
    ours.metadata_event_payload = json!({ "kind": "task_reply", "binding_id": own_binding });
    let human = message("13.0", "U1", "hello");

    let kept = filter_route_generation(
        vec![control_ack, foreign, ours, human],
        &target,
        "",
        "",
        &HashMap::new(),
    );
    let texts: Vec<&str> = kept.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, vec!["our reply", "hello"]);
}

#[test]
fn route_generation_also_filters_through_the_outbound_ledger_and_strips_new_prefix() {
    let target = target();
    let ledger_owned = message("10.0", "UBOT", "recorded control ack");
    let mut owners = HashMap::new();
    owners.insert(
        "10.0".to_string(),
        OutboundRow {
            channel_message_id: "10.0".to_string(),
            outbound_kind: "control_ack".to_string(),
            binding_id: target.binding_id,
        },
    );
    let boundary = message("20.0", "U1", "<@UBOT> /new hello there");

    let mut target_with_start = target.clone();
    target_with_start.history_start = "20.0".to_string();
    let kept = filter_route_generation(
        vec![ledger_owned, boundary],
        &target_with_start,
        "",
        "",
        &owners,
    );
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].text, "hello there", "/new 被剥成正文");
}

#[test]
fn context_generation_fails_closed_for_unknown_bot_output() {
    let bot = message("10.0", "UBOT", "untrusted old reply");
    let known = message("11.0", "UBOT", "this generation's reply");
    let human = message("12.0", "U1", "human");
    let allowed: HashSet<String> = ["11.0".to_string()].into_iter().collect();
    let opts = HistoryOptions {
        context_revision: 2,
        ..HistoryOptions::default()
    };
    let kept = filter_context_generation(vec![bot, known, human], &opts, "", "", "UBOT", &allowed);
    let texts: Vec<&str> = kept.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, vec!["this generation's reply", "human"]);

    // 代际 1 不做白名单（上游：第一代没有可信来源可依）。
    let opts = HistoryOptions {
        context_revision: 1,
        ..HistoryOptions::default()
    };
    let kept = filter_context_generation(
        vec![message("10.0", "UBOT", "old")],
        &opts,
        "",
        "",
        "UBOT",
        &HashSet::new(),
    );
    assert_eq!(kept.len(), 1);
}

// =====================================================================
// 摊平与命名（纯函数）
// =====================================================================

#[test]
fn alert_cards_are_read_from_attachments_when_the_top_level_text_is_empty() {
    let mut alert = message("10.0", "B1", "");
    alert.attachments = vec![json!({
        "title": "Grafana alert",
        "text": "disk almost full",
        "fields": [{"title": "host", "value": "web-1"}],
        "fallback": "ignored when text exists"
    })];
    let text = flatten_slack_text(&alert);
    assert!(text.contains("Grafana alert"));
    assert!(text.contains("disk almost full"));
    assert!(text.contains("host web-1"));
    assert!(!text.contains("ignored"), "fallback 是最后手段");
}

#[test]
fn a_join_marker_flattens_to_nothing() {
    assert!(flatten_slack_text(&message("10.0", "U1", "")).is_empty());
    assert!(flatten_slack_text(&message("10.0", "U1", "   ")).is_empty());
}

#[test]
fn rich_text_blocks_flatten_to_plain_lines() {
    let blocks = vec![json!({
        "type": "rich_text",
        "elements": [
            {"type": "rich_text_section", "elements": [
                {"type": "text", "text": "line one "},
                {"type": "link", "url": "https://x", "text": "link label"}
            ]},
            {"type": "rich_text_list", "elements": [
                {"type": "rich_text_section", "elements": [{"type": "text", "text": "item"}]}
            ]}
        ]
    })];
    assert_eq!(flatten_blocks(&blocks), "line one link label\nitem");
}

#[test]
fn truncate_chars_counts_characters_and_appends_an_ellipsis() {
    assert_eq!(truncate_chars("中文测试", 4), "中文测试");
    assert_eq!(truncate_chars("中文测试超", 4), "中文测试…");
}

#[test]
fn labeler_prefers_display_name_then_falls_back_to_positional_users() {
    let mut names = HashMap::new();
    names.insert("U1".to_string(), "Alice".to_string());
    let mut labeler = HistoryLabeler::new(names);
    assert_eq!(labeler.label(&message("1.0", "UBOT", ""), true), "Bot");
    assert_eq!(labeler.label(&message("2.0", "U1", ""), false), "Alice");
    assert_eq!(labeler.label(&message("3.0", "U2", ""), false), "User 1");
    // 同一个人在同一页里标签稳定。
    assert_eq!(labeler.label(&message("4.0", "U1", ""), false), "Alice");
    assert_eq!(labeler.label(&message("5.0", "U2", ""), false), "User 1");
}

#[test]
fn display_name_prefers_display_name_then_real_name_then_handle() {
    let user = SlackUser {
        id: "U1".to_string(),
        display_name: "d".to_string(),
        real_name: "r".to_string(),
        name: "h".to_string(),
    };
    assert_eq!(slack_display_name(&user), "d");
    let user = SlackUser {
        display_name: String::new(),
        ..user
    };
    assert_eq!(slack_display_name(&user), "r");
    let user = SlackUser {
        real_name: String::new(),
        ..user
    };
    assert_eq!(slack_display_name(&user), "h");
}

#[test]
fn user_name_resolution_failure_is_not_an_error() {
    let tokens = tokio::runtime::Runtime::new().expect("runtime");
    let api = FakeApi {
        history: Vec::new(),
        replies: Vec::new(),
        users: Vec::new(),
        fail_users: true,
    };
    let names = tokens.block_on(resolve_user_names(
        &api,
        "xoxb-test",
        &[message("1.0", "U1", "hi")],
        "UBOT",
    ));
    assert!(names.is_empty());
}

// =====================================================================
// 读面（替身端口）
// =====================================================================

#[tokio::test]
async fn a_session_without_a_slack_binding_is_an_empty_read() {
    let history = history(
        FakeStore {
            binding: None,
            installation: None,
            outbound: Vec::new(),
        },
        FakeApi {
            history: Vec::new(),
            replies: Vec::new(),
            users: Vec::new(),
            fail_users: false,
        },
    );
    let error = history
        .channel_overview(Id::new(), &HistoryOptions::default())
        .await
        .expect_err("没有绑定");
    assert!(error.is_no_slack_session(), "空读语义：{error}");
}

#[tokio::test]
async fn a_revoked_installation_reads_nothing() {
    let history = history(
        wired(None, false),
        FakeApi {
            history: vec![message("10.0", "U1", "hi")],
            replies: Vec::new(),
            users: Vec::new(),
            fail_users: false,
        },
    );
    let page = history
        .thread(Id::new(), "", &HistoryOptions::default())
        .await;
    assert!(page.is_err(), "撤销的安装没有可读的东西");
}

#[tokio::test]
async fn channel_overview_tags_threads_and_uses_the_real_channel_id() {
    let mut root = message("10.0", "U1", "thread root");
    root.reply_count = 3;
    root.latest_reply = "12.0".to_string();
    let history = history(
        wired(None, true),
        FakeApi {
            history: vec![root, message("11.0", "U2", "second")],
            replies: Vec::new(),
            users: vec![SlackUser {
                id: "U1".to_string(),
                display_name: "Alice".to_string(),
                ..SlackUser::default()
            }],
            fail_users: false,
        },
    );
    let page = history
        .channel_overview(Id::new(), &HistoryOptions::default())
        .await
        .expect("overview");
    assert_eq!(page.channel_type, "slack");
    assert_eq!(page.messages.len(), 2, "最旧在前、系统标记被丢掉");
    assert_eq!(page.messages[0].author, "Alice");
    assert_eq!(page.messages[0].thread_id, "10.0");
    assert_eq!(page.messages[0].reply_count, 3);
    assert_eq!(page.messages[1].author, "User 1");
}

#[tokio::test]
async fn a_boundary_pending_binding_short_circuits_to_an_empty_page() {
    let mut binding = wired(None, true);
    if let Some(row) = binding.binding.as_mut() {
        row.history_boundary_pending = true;
    }
    let history = history(
        binding,
        FakeApi {
            history: vec![message("10.0", "U1", "should not be read")],
            replies: Vec::new(),
            users: Vec::new(),
            fail_users: false,
        },
    );
    let page = history
        .channel_overview(Id::new(), &HistoryOptions::default())
        .await
        .expect("empty");
    assert!(page.messages.is_empty());
    assert_eq!(page.channel_type, "slack");
}

#[tokio::test]
async fn thread_reads_the_sessions_own_thread_and_strips_the_clear_prefix() {
    let mut boundary = message("10.0", "U1", "<@UBOT> /clear keep this");
    boundary.ts = "10.0".to_string();
    let history = history(
        wired(None, true),
        FakeApi {
            history: Vec::new(),
            replies: vec![boundary, message("11.0", "UBOT", "reply")],
            users: Vec::new(),
            fail_users: false,
        },
    );
    let opts = HistoryOptions {
        after: "10.0".to_string(),
        ..HistoryOptions::default()
    };
    let page = history.thread(Id::new(), "", &opts).await.expect("thread");
    assert_eq!(page.thread_id, "100.1", "空 thread_id ⇒ 会话自己的线程");
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.messages[0].text, "keep this", "/clear 前缀被剥掉");
    assert_eq!(page.messages[1].role, HistoryRole::Assistant);
}
