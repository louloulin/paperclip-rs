//! 表情契约的用例（`emotion.rs` 的 `#[cfg(test)] mod tests;`）。
//!
//! 钉住三件事：平台枚举名与两条路径（wire 面）、四条守卫的**顺序**、以及
//! "401 ⇒ 作废令牌缓存并重试**一次**"这条规则的每一步。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use super::{
    emotion_path, emotion_request, set_emoji_reaction, Emotion, EmotionError, EmotionRequest,
    EmotionTransport, PATH_RECALL_EMOTION, PATH_REPLY_EMOTION,
};

/// 脚本化的传输替身：按序吐结果，并记录每次调用与作废次数。
#[derive(Default)]
struct FakeTransport {
    results: Mutex<VecDeque<Result<(), EmotionError>>>,
    calls: Mutex<Vec<(String, EmotionRequest)>>,
    invalidations: AtomicUsize,
}

impl FakeTransport {
    fn scripted(results: Vec<Result<(), EmotionError>>) -> Self {
        Self {
            results: Mutex::new(results.into()),
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<(String, EmotionRequest)> {
        self.calls.lock().expect("lock").clone()
    }

    fn invalidations(&self) -> usize {
        self.invalidations.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EmotionTransport for FakeTransport {
    async fn post_emotion(&self, path: &str, request: &EmotionRequest) -> Result<(), EmotionError> {
        self.calls
            .lock()
            .expect("lock")
            .push((path.to_string(), request.clone()));
        self.results
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or(Ok(()))
    }

    fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

/// 两个内置表情的平台名（它们是 `OpenAPI` 的 enum 值，**不是**产品文案）。
#[test]
fn the_two_platform_emotion_names_are_stable() {
    assert_eq!(Emotion::Acknowledged.platform_name(), "收到");
    assert_eq!(Emotion::Done.platform_name(), "Done");
    for emotion in Emotion::ALL {
        assert_eq!(
            Emotion::from_platform_name(emotion.platform_name()),
            Some(emotion)
        );
    }
    // 别的名字解不回来（绝不把任意字符串当表情发给平台）。
    assert_eq!(Emotion::from_platform_name("emotion_167"), None);
    assert_eq!(Emotion::from_platform_name(""), None);
    assert_eq!(Emotion::ALL.len(), 2);
}

/// 两条路径：贴 / 撤。
#[test]
fn the_two_paths_are_selected_by_recall() {
    assert_eq!(emotion_path(false), PATH_REPLY_EMOTION);
    assert_eq!(emotion_path(true), PATH_RECALL_EMOTION);
    assert_eq!(emotion_path(false), "/v1.0/robot/emotion/reply");
    assert_eq!(emotion_path(true), "/v1.0/robot/emotion/recall");
}

/// 四条守卫的**顺序**（会话 id → 消息 id → robot code → 名字）与请求体的逐字形态。
#[test]
fn request_validation_order_and_body_shape() {
    assert_eq!(
        emotion_request("", "", "m", Emotion::Done),
        Err(EmotionError::MissingConversationId)
    );
    assert_eq!(
        emotion_request("", "c", "", Emotion::Done),
        Err(EmotionError::MissingMessageId)
    );
    assert_eq!(
        emotion_request("", "c", "m", Emotion::Done),
        Err(EmotionError::MissingRobotCode)
    );

    let request =
        emotion_request("rank", "cid", "mid", Emotion::Acknowledged).expect("所有字段都在");
    assert_eq!(request.emotion_type, 1);
    assert_eq!(
        serde_json::to_value(&request).expect("序列化"),
        serde_json::json!({
            "robotCode": "rank",
            "openConversationId": "cid",
            "openMsgId": "mid",
            "emotionType": 1,
            "emotionName": "收到",
        })
    );
    // 请求体里没有凭据字段（令牌在 `Authorization` 头，由实现加）。
    let rendered = format!("{request:?}");
    assert!(!rendered.contains("token"));
    assert!(rendered.contains("rank"));
}

/// 第一次就成功：一次调用、零作废。
#[tokio::test]
async fn a_successful_reaction_posts_once_and_never_invalidates() {
    let transport = FakeTransport::scripted(vec![]);
    let outcome = set_emoji_reaction(
        &transport,
        "rank",
        "cid",
        "mid",
        Emotion::Acknowledged,
        false,
    )
    .await;
    assert_eq!(outcome, Ok(()));
    assert_eq!(transport.calls().len(), 1);
    assert_eq!(transport.calls()[0].0, PATH_REPLY_EMOTION);
    assert_eq!(transport.invalidations(), 0);
}

/// 401 ⇒ 作废令牌缓存并**重试一次**（第二次成功 ⇒ `Ok`）。
#[tokio::test]
async fn a_401_invalidates_the_token_and_retries_exactly_once() {
    let transport = FakeTransport::scripted(vec![Err(EmotionError::Unauthorized)]);
    let outcome = set_emoji_reaction(&transport, "rank", "cid", "mid", Emotion::Done, true).await;
    assert_eq!(outcome, Ok(()));
    let calls = transport.calls();
    assert_eq!(calls.len(), 2);
    // 两次都在**撤**的那条路径上（重试不改语义）。
    assert!(calls.iter().all(|(path, _)| path == PATH_RECALL_EMOTION));
    assert!(calls.iter().all(|(_, body)| body.emotion_name == "Done"));
    assert_eq!(transport.invalidations(), 1);
}

/// 两次都 401 ⇒ 原样返回 401（调用方自己决定怎么处置）。
#[tokio::test]
async fn two_401s_surface_unauthorized() {
    let transport = FakeTransport::scripted(vec![
        Err(EmotionError::Unauthorized),
        Err(EmotionError::Unauthorized),
    ]);
    let outcome = set_emoji_reaction(&transport, "rank", "cid", "mid", Emotion::Done, false).await;
    assert_eq!(outcome, Err(EmotionError::Unauthorized));
    assert_eq!(transport.calls().len(), 2);
    assert_eq!(transport.invalidations(), 1, "只作废一次");
}

/// 平台拒绝（`success=false`）与链路失败都**不**重试。
#[tokio::test]
async fn rejected_and_transport_failures_are_not_retried() {
    let rejected = FakeTransport::scripted(vec![Err(EmotionError::Rejected)]);
    assert_eq!(
        set_emoji_reaction(&rejected, "rank", "cid", "mid", Emotion::Done, false).await,
        Err(EmotionError::Rejected)
    );
    assert_eq!(rejected.calls().len(), 1);
    assert_eq!(rejected.invalidations(), 0);

    let broken = FakeTransport::scripted(vec![Err(EmotionError::Transport {
        message: "connection reset".to_string(),
    })]);
    assert!(matches!(
        set_emoji_reaction(&broken, "rank", "cid", "mid", Emotion::Done, false).await,
        Err(EmotionError::Transport { .. })
    ));
    assert_eq!(broken.calls().len(), 1);
    assert_eq!(broken.invalidations(), 0);
}

/// 校验不通过时**一次都不发**（守卫在循环之前）。
#[tokio::test]
async fn validation_happens_before_any_request() {
    let transport = FakeTransport::scripted(vec![]);
    let outcome = set_emoji_reaction(&transport, "rank", "", "mid", Emotion::Done, false).await;
    assert_eq!(outcome, Err(EmotionError::MissingConversationId));
    assert!(transport.calls().is_empty());
}
