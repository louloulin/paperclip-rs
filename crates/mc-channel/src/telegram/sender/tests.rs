//! `telegram::sender` 的用例（写者 M7-6）。
//!
//! 上游 `telegram_test.go` 的 `TestChunkMessagePrefersNewlines` /
//! `TestChunkMessageCountsUTF16Units` / `TestSenderFallsBackOnlyForHTMLParseErrors` 逐条移植，
//! 外加**表驱动的**发送形态矩阵（分片 / 引用只挂第一片 / 429 一次重试 / 传输失败不重试 /
//! 错误路径不回显凭据）。
//!
//! 替身是**进程内**的记录式 [`TelegramApi`] 实现（不跑 HTTP）：本文件测的是**发送语义**
//! （顺序、回落、重试预算），wire 形状由 `api/tests.rs` 与端到端回路各测一次。
//! 这符合 `docs/60` §4.2 第 1 条 —— 只替平台 wire，且这里连 wire 都不替（没有 wire）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use mc_core::channel::message::OutboundMessage;

use super::*;
use crate::telegram::api::{ApiResult, EditMessageText, WebhookInfo};
use crate::telegram::inbound::{Message, Update, User};

/// 一次 `sendMessage` 调用留下的痕迹。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentCall {
    token: String,
    params: SendMessage,
}

/// 记录式 `TelegramApi` 替身：`sendMessage` 按脚本应答，其余方法只记账。
#[derive(Clone, Default)]
struct RecordingApi {
    state: Arc<Mutex<RecordingState>>,
}

#[derive(Default)]
struct RecordingState {
    sends: Vec<SentCall>,
    edits: Vec<(String, EditMessageText)>,
    /// `sendMessage` 的脚本化应答（用尽后一律成功，id 递增）。
    script: VecDeque<ApiResult<Message>>,
    next_id: i64,
}

impl RecordingApi {
    /// 预置 `sendMessage` 的应答序列。
    fn scripted(responses: Vec<ApiResult<Message>>) -> Self {
        let api = Self::default();
        api.state.lock().expect("lock").script = responses.into();
        api
    }

    /// 全部 `sendMessage` 调用。
    fn sends(&self) -> Vec<SentCall> {
        self.state.lock().expect("lock").sends.clone()
    }
}

#[async_trait::async_trait]
impl TelegramApi for RecordingApi {
    async fn get_me(&self, _bot_token: &str) -> ApiResult<User> {
        Ok(User::default())
    }

    async fn get_webhook_info(&self, _bot_token: &str) -> ApiResult<WebhookInfo> {
        Ok(WebhookInfo::default())
    }

    async fn get_updates(&self, _bot_token: &str, _offset: i64) -> ApiResult<Vec<Update>> {
        Ok(Vec::new())
    }

    async fn send_message(&self, bot_token: &str, params: &SendMessage) -> ApiResult<Message> {
        let mut state = self.state.lock().expect("lock");
        state.sends.push(SentCall {
            token: bot_token.to_string(),
            params: params.clone(),
        });
        if let Some(scripted) = state.script.pop_front() {
            return scripted;
        }
        state.next_id += 1;
        Ok(Message {
            message_id: state.next_id,
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

    async fn edit_message_text(&self, bot_token: &str, params: &EditMessageText) -> ApiResult<()> {
        self.state
            .lock()
            .expect("lock")
            .edits
            .push((bot_token.to_string(), params.clone()));
        Ok(())
    }
}

/// 造一条出站消息。
fn outbound(chat_id: &str, text: &str) -> OutboundMessage {
    OutboundMessage {
        chat_id: chat_id.to_string(),
        text: text.to_string(),
        thread_id: String::new(),
        reply_to: String::new(),
    }
}

/// bot token 的形态（**不是**真令牌；断言它不出现在错误文案里）。
const TOKEN: &str = "123456:not-a-real-bot-token-itest-only";

// ---------------------------------------------------------------------------
// 分片（上游两条用例逐字）
// ---------------------------------------------------------------------------

/// 上游 `TestChunkMessagePrefersNewlines`。
#[test]
fn chunk_message_prefers_newlines() {
    let text = format!("{}\n{}", "a".repeat(60), "b".repeat(60));
    let chunks = chunk_message(&text, 100);
    assert_eq!(chunks.len(), 2, "want 2 chunks, got {chunks:?}");
    assert!(
        chunks[0].ends_with('a'),
        "split should land on the newline: {chunks:?}"
    );
    assert!(
        chunks[1].starts_with('b'),
        "split should land on the newline: {chunks:?}"
    );
}

/// 上游 `TestChunkMessageCountsUTF16Units`。
#[test]
fn chunk_message_counts_utf16_units() {
    let chunks = chunk_message("😀a😀", 3);
    assert_eq!(chunks, vec!["😀a".to_string(), "😀".to_string()]);
    for chunk in &chunks {
        assert!(utf16_units(chunk) <= 3, "chunk {chunk:?} over the cap");
    }
}

/// 分片的三个边界：超上限才切、上限 0 不切、星面字符不被切开。
#[test]
fn chunk_message_boundaries_are_table_driven() {
    let cases: &[(&str, usize, &[&str])] = &[
        ("short", 3500, &["short"]),
        ("", 3500, &[""]),
        // 上限 0 ⇒ 不切（上游 `maxUnits <= 0`）。
        ("anything", 0, &["anything"]),
        // 上限 4，两个星面字符 + 一个 ASCII ⇒ 第一片装不下第二个 emoji。
        ("😀😀a", 4, &["😀😀", "a"]),
        // 没有换行可依 ⇒ 硬切在 rune 边界上。
        ("abcdef", 3, &["abc", "def"]),
    ];
    for (text, max, want) in cases {
        assert_eq!(
            &chunk_message(text, *max),
            want,
            "chunk_message({text:?}, {max})"
        );
    }
}

// ---------------------------------------------------------------------------
// 发送形态
// ---------------------------------------------------------------------------

/// 单条短回复：一次 `sendMessage`，HTML parse mode，复合键 `"chat:message"`。
#[tokio::test]
async fn a_short_reply_is_one_html_send() {
    let api = RecordingApi::default();
    let sender = Sender::new(Arc::new(api.clone()));

    let frame = sender
        .send(TOKEN, &outbound("42", "# Title\n**bold** and *it*"))
        .await
        .expect("send");
    assert_eq!(frame.message_ids, vec!["42:1".to_string()]);
    assert_eq!(frame.to_send_result().message_id, "42:1");
    assert!(
        frame.to_send_result().message_ids.is_empty(),
        "上游 telegram 不填 MessageIDs"
    );
    let sends = api.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].params.parse_mode, "HTML");
    assert_eq!(
        sends[0].params.text,
        "<b>Title</b>\n<b>bold</b> and <i>it</i>"
    );
    assert!(!frame.is_empty());
}

/// 表驱动的发送形态矩阵。
#[tokio::test]
async fn send_shapes_are_table_driven() {
    // 1) HTML 被拒 ⇒ 回落纯文本（同一片发两次），第二片不带 parse_mode。
    let api = RecordingApi::scripted(vec![Err(ApiError::Api {
        method: "sendMessage",
        code: 400,
        description: "Bad Request: can't parse entities: unsupported start tag".to_string(),
        retry_after: None,
    })]);
    let sender = Sender::new(Arc::new(api.clone()));
    let frame = sender
        .send(TOKEN, &outbound("42", "**bold**"))
        .await
        .expect("fallback send");
    assert_eq!(frame.message_ids, vec!["42:1".to_string()]);
    let sends = api.sends();
    assert_eq!(sends.len(), 2, "回落只多发一次：{sends:#?}");
    assert_eq!(sends[0].params.parse_mode, "HTML");
    assert_eq!(sends[1].params.parse_mode, "", "回落不带 parse mode");
    assert_eq!(sends[1].params.text, "**bold**", "回落发原始 Markdown");
    assert_eq!(sends[0].params.text, "<b>bold</b>");

    // 2) 传输失败**不**重试（响应丢了 = 可能已经投递成功，重发即重复）。
    let api = RecordingApi::scripted(vec![Err(ApiError::Transport {
        method: "sendMessage",
    })]);
    let sender = Sender::new(Arc::new(api.clone()));
    let error = sender
        .send(TOKEN, &outbound("42", "hi"))
        .await
        .expect_err("fails");
    assert_eq!(api.sends().len(), 1, "传输失败不得重试");
    // 错误路径不回显凭据（§2.3 第 2 条）：文案里不得出现 token / URL / chat。
    let text = error.to_string();
    assert!(!text.contains(TOKEN), "error leaks the token: {text}");
    assert!(
        !text.contains("api.telegram.org"),
        "error leaks the URL: {text}"
    );

    // 3) 其它 400（不是实体解析错误）**不**回落 —— 回落只会把真问题藏起来。
    let api = RecordingApi::scripted(vec![Err(ApiError::Api {
        method: "sendMessage",
        code: 400,
        description: "Bad Request: chat not found".to_string(),
        retry_after: None,
    })]);
    let sender = Sender::new(Arc::new(api.clone()));
    assert!(sender.send(TOKEN, &outbound("42", "hi")).await.is_err());
    assert_eq!(api.sends().len(), 1, "非解析错误不得回落");
}

/// 429：按 Telegram 强制的退避**重试一次**（唯一的真实等待，1 秒是协议下界）。
#[tokio::test]
async fn a_rate_limit_is_retried_exactly_once() {
    let api = RecordingApi::scripted(vec![
        Err(ApiError::Api {
            method: "sendMessage",
            code: 429,
            description: "Too Many Requests: retry after 1".to_string(),
            retry_after: Some(1),
        }),
        Err(ApiError::Api {
            method: "sendMessage",
            code: 429,
            description: "Too Many Requests: retry after 1".to_string(),
            retry_after: Some(1),
        }),
    ]);
    let sender = Sender::new(Arc::new(api.clone()));
    let started = std::time::Instant::now();
    let error = sender
        .send(TOKEN, &outbound("42", "hi"))
        .await
        .expect_err("second 429 is final");
    assert_eq!(api.sends().len(), 2, "只重试一次");
    assert_eq!(error.to_string().matches("429").count(), 1);
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(1),
        "退避不得被跳过"
    );
}

/// 目标解析失败是**产品性**错误，不是 API 错误（上游 `bad chat id %q`）。
#[tokio::test]
async fn a_non_numeric_chat_id_is_a_bad_target() {
    let api = RecordingApi::default();
    let sender = Sender::new(Arc::new(api.clone()));
    let error = sender
        .send(TOKEN, &outbound("not-a-chat", "hi"))
        .await
        .expect_err("bad chat id");
    assert_eq!(
        error,
        SendError::BadChatId {
            value: "not-a-chat".to_string()
        }
    );
    assert!(api.sends().is_empty(), "解析失败不该发起任何请求");
}

/// 分片的多条回复：**只有第一片**引用触发消息，最终 id 是**末片**。
#[tokio::test]
async fn only_the_first_chunk_quotes_and_the_last_id_wins() {
    let api = RecordingApi::default();
    // 上限压到 4，逼出三片。
    let sender = Sender::new(Arc::new(api.clone())).with_max_units(4);
    let out = OutboundMessage {
        chat_id: "42".to_string(),
        text: "aaaa\nbbbb\ncccc".to_string(),
        thread_id: "7".to_string(),
        reply_to: "42:99".to_string(),
    };
    let frame = sender.send(TOKEN, &out).await.expect("send");
    let sends = api.sends();
    assert!(sends.len() >= 2, "1,2 号形态：应当分片 {sends:#?}");
    assert_eq!(sends[0].params.reply_to_message_id, 99, "第一片引用");
    assert!(sends[0].params.allow_sending_without_reply);
    for call in &sends[1..] {
        assert_eq!(call.params.reply_to_message_id, 0, "后续片不引用");
    }
    for call in &sends {
        assert_eq!(call.params.message_thread_id, 7, "话题路由每一片都要带");
        assert_eq!(call.token, TOKEN);
    }
    assert_eq!(
        frame.to_send_result().message_id,
        format!("42:{}", sends.len()),
        "末片 id 胜出"
    );
}

/// 复合引用键（`"chat:message"`）与裸 id 都要能用（上游 `parseMessageRef`）。
#[tokio::test]
async fn the_reply_reference_accepts_both_shapes() {
    for reference in ["42:99", "99"] {
        let api = RecordingApi::default();
        let sender = Sender::new(Arc::new(api.clone()));
        let out = OutboundMessage {
            chat_id: "42".to_string(),
            text: "hi".to_string(),
            thread_id: String::new(),
            reply_to: reference.to_string(),
        };
        sender.send(TOKEN, &out).await.expect("send");
        let sends = api.sends();
        assert_eq!(sends[0].params.reply_to_message_id, 99, "ref = {reference}");
    }
}

/// `is_html_parse_error` 只认三种描述，且必须是 400。
#[test]
fn html_parse_error_detection_is_table_driven() {
    let api_error = |code: u16, description: &str| ApiError::Api {
        method: "sendMessage",
        code,
        description: description.to_string(),
        retry_after: None,
    };
    let cases: &[(ApiError, bool)] = &[
        (
            api_error(
                400,
                "Bad Request: can't parse entities: Unexpected character",
            ),
            true,
        ),
        (
            api_error(
                400,
                "Bad Request: unsupported start tag \"x\" at byte offset 0",
            ),
            true,
        ),
        (
            api_error(
                400,
                "Bad Request: can't find end tag corresponding to \"b\"",
            ),
            true,
        ),
        (
            api_error(400, "Bad Request: message is not modified"),
            false,
        ),
        (api_error(400, "Bad Request: chat not found"), false),
        (api_error(429, "can't parse entities"), false),
        (
            ApiError::Transport {
                method: "sendMessage",
            },
            false,
        ),
        (
            ApiError::Malformed {
                method: "sendMessage",
            },
            false,
        ),
        (ApiError::Conflict, false),
    ];
    for (error, want) in cases {
        assert_eq!(is_html_parse_error(error), *want, "error = {error:?}");
    }
}
