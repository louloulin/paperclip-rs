//! `slack` 模块级用例的**共用件**：注册面最小依赖袋、本地平台替身、
//! engine 端口替身与回路装配（写者 M7-4）。
//!
//! 与 `tests.rs` 分开是**门 ⑩**（单文件 800 行硬限）的要求；切点是「装置 / 断言」
//! ——本文件只有装置，断言全在 `tests.rs`。
//!
//! # 两类替身，都只替**平台 wire**
//!
//! - `serve_socket_mode`：本地 **WebSocket** 服务端（替 Slack 的 Socket Mode 端点）；
//! - `serve_web_api`：本地 HTTP 服务端（替 `apps.connections.open` 与 `chat.postMessage`）。
//!
//! engine 的端口实现（安装 / 身份 / 去重 / 会话 / 审计 / 运行 / issue / 绑定 / 记账）是
//! **内存替身**，因为本 crate 没有 `sqlx` 依赖；**真库**形态由
//! `crates/mc-http/tests/channels/slack.rs` 用泛化渠道仓储覆盖（门 ⑥）。业务路径
//! （归一化 / 路由 / 判决 / 解密 / 文案 / mrkdwn / 分片 / 线程 / 元数据）在两边都是**真代码**。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::engine::resolvers::{
    AppendParams, AppendResult, Auditor, BindMediaParams, BindMediaResult, ChannelIssueOutcome,
    ChannelIssueParams, ChatRunParams, Deduper, DropReason, EngineError, EngineResult,
    EnsureSessionParams, IdentityResolver, InstallationResolver, IssueCreator, NoCommands,
    OutboundReplier, PipelineError, ResolvedIdentity, ResolvedInstallation, ResolverSet,
    RunTriggerer, SessionBinder, SessionReader, StartSessionParams, StartSessionResult,
    WorkspaceIdentity,
};
use crate::engine::router::{Router, RouterConfig};
use crate::message::SharedInboundHandler;
use crate::slack::config::Decrypter;
use crate::slack::outbound::Sender;
use crate::slack::replier::{
    BindingMinter, MintedBinding, OutboundLedger, OutboundRecord, SlackOutboundReplier,
};
use crate::slack::resolvers::InstallationRow;
use crate::slack::socket::{SlackChannel, TungsteniteTransport};

/// 标准 base64（BYO 落库的形态：先 `secretbox.seal`，再 base64）。
#[must_use]
pub(crate) fn base64_standard(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 进程全局的 API 基址是共享的 ⇒ 用到替身的用例串行。
pub(crate) static BASE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 一条 `events_api` 信封（Slack Socket Mode 的**真实**线形态）。
pub(crate) fn events_frame(envelope_id: &str, ts: &str) -> String {
    json!({
        "type": "events_api",
        "envelope_id": envelope_id,
        "payload": {
            "token": "verification-not-checked-by-socket-mode",
            "team_id": "T1",
            "api_app_id": "A1",
            "event": {
                "type": "app_mention",
                "channel": "C1",
                "channel_type": "channel",
                "user": "U1",
                "text": "<@UBOT> hello there",
                "ts": ts,
                "event_ts": ts,
            }
        }
    })
    .to_string()
}

/// 起一个本地 Socket Mode 替身：`script` 里的帧依次发出，收到的 ACK 原样回传。
pub(crate) async fn serve_socket_mode(
    script: Vec<String>,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ws");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        use futures_util::{SinkExt as _, StreamExt as _};

        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        if socket
            .send(tokio_tungstenite::tungstenite::Message::text(
                json!({"type": "hello"}).to_string(),
            ))
            .await
            .is_err()
        {
            return;
        }
        for frame in script {
            if socket
                .send(tokio_tungstenite::tungstenite::Message::text(frame))
                .await
                .is_err()
            {
                return;
            }
            // 等这一帧的 ACK。
            match socket.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                    let _ = tx.send(text);
                }
                _ => return,
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    (format!("ws://127.0.0.1:{port}"), rx)
}

/// 起一个本地 Slack Web API 替身（`apps.connections.open` + `chat.postMessage`）。
///
/// 返回 `(基址, 收到的 chat.postMessage 正文)`。手写 HTTP/1.1（不引入新依赖）。
pub(crate) async fn serve_web_api(
    ws_url: String,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind api");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let (ws_url, tx) = (ws_url.clone(), tx.clone());
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(read) = socket.read(&mut chunk).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    raw.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
                let path = head
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .to_string();
                let response = if path.ends_with("apps.connections.open") {
                    json!({"ok": true, "url": ws_url}).to_string()
                } else if path.ends_with("chat.postMessage") {
                    let _ = tx.send(body.to_string());
                    json!({"ok": true, "ts": "1700000000.000100"}).to_string()
                } else {
                    json!({"ok": false, "error": "unknown_method"}).to_string()
                };
                let payload = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response.len(),
                    response
                );
                let _ = socket.write_all(payload.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), payload_rx(rx))
}

/// 恒等转发（把通道类型写清楚，便于阅读）。
fn payload_rx(
    rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> tokio::sync::mpsc::UnboundedReceiver<String> {
    rx
}

// ---- engine 端口替身（业务路径全真，只有"背后是 DB"的口是内存的） ----

pub(crate) struct FakeInstallations(InstallationRow);

#[async_trait]
impl InstallationResolver for FakeInstallations {
    async fn resolve_installation(
        &self,
        _message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation> {
        Ok(ResolvedInstallation {
            id: self.0.id,
            workspace_id: self.0.workspace_id,
            agent_id: self.0.agent_id,
            installer_user_id: self.0.installer_user_id,
            active: self.0.is_active(),
            kind: ChannelKind::Slack,
            platform: Some(Arc::new(self.0.clone())),
        })
    }
}

/// 发件人**未绑定**（绑定卡那条路径）。
pub(crate) struct UnboundIdentity;

#[async_trait]
impl IdentityResolver for UnboundIdentity {
    async fn resolve_sender(
        &self,
        _installation: &ResolvedInstallation,
        _message: &InboundMessage,
    ) -> EngineResult<ResolvedIdentity> {
        Err(PipelineError::SenderUnbound.into())
    }
}

/// 已绑定（反例用：不会触发出站回复）。
pub(crate) struct BoundIdentity;

#[async_trait]
impl IdentityResolver for BoundIdentity {
    async fn resolve_sender(
        &self,
        _installation: &ResolvedInstallation,
        _message: &InboundMessage,
    ) -> EngineResult<ResolvedIdentity> {
        Ok(ResolvedIdentity { user_id: Id::new() })
    }
}

/// 内存去重（语义照上游：claim → mark / release）。
#[derive(Default)]
pub(crate) struct MemoryDedup {
    pub(crate) claims: Mutex<HashSet<String>>,
}

#[async_trait]
impl Deduper for MemoryDedup {
    async fn claim(&self, _installation_id: Id, message_id: &str) -> EngineResult<Id> {
        let mut claims = self.claims.lock().expect("lock");
        if !claims.insert(message_id.to_string()) {
            return Err(PipelineError::Duplicate.into());
        }
        Ok(Id::new())
    }

    async fn mark(&self, _installation_id: Id, _message_id: &str, _token: Id) -> EngineResult<()> {
        Ok(())
    }

    async fn release(
        &self,
        _installation_id: Id,
        message_id: &str,
        _token: Id,
    ) -> EngineResult<()> {
        self.claims.lock().expect("lock").remove(message_id);
        Ok(())
    }
}

/// 会话绑定替身（本节不落任何消息 ⇒ 全部 `EngineError::infra`，与"没接线"同形）。
struct UnusedSession;

#[async_trait]
impl SessionBinder for UnusedSession {
    async fn ensure_session(&self, _params: EnsureSessionParams) -> EngineResult<Id> {
        Err(EngineError::infra("unused"))
    }

    async fn start_session(&self, _params: StartSessionParams) -> EngineResult<StartSessionResult> {
        Err(EngineError::infra("unused"))
    }

    async fn mark_pending_fresh(&self, _session_id: Id, _message_id: &str) -> EngineResult<()> {
        Err(EngineError::infra("unused"))
    }

    async fn append_message(&self, _params: AppendParams) -> EngineResult<AppendResult> {
        Err(EngineError::infra("unused"))
    }

    async fn bind_media(&self, _params: BindMediaParams) -> EngineResult<BindMediaResult> {
        Err(EngineError::infra("unused"))
    }
}

/// 丢弃审计替身：记下每一条丢弃（反例断言用）。
#[derive(Default)]
pub(crate) struct RecordingAudit {
    pub(crate) drops: Mutex<Vec<DropReason>>,
}

#[async_trait]
impl Auditor for RecordingAudit {
    async fn record_drop(
        &self,
        _installation_id: Option<Id>,
        _message: &InboundMessage,
        reason: DropReason,
    ) -> EngineResult<()> {
        self.drops.lock().expect("lock").push(reason);
        Ok(())
    }
}

pub(crate) struct NoTrigger;

#[async_trait]
impl RunTriggerer for NoTrigger {
    async fn schedule_chat_run(&self, _params: ChatRunParams) -> EngineResult<()> {
        Ok(())
    }
    async fn drain(&self) -> EngineResult<()> {
        Ok(())
    }
}

pub(crate) struct NoReader;

#[async_trait]
impl SessionReader for NoReader {
    async fn workspace_identity(&self, _workspace_id: Id) -> EngineResult<WorkspaceIdentity> {
        Ok(WorkspaceIdentity::default())
    }
}

pub(crate) struct NoIssues;

#[async_trait]
impl IssueCreator for NoIssues {
    async fn create_issue(&self, _params: ChannelIssueParams) -> EngineResult<ChannelIssueOutcome> {
        Err(EngineError::infra("unused"))
    }
}

/// 绑定令牌铸造替身（真令牌的形态与哈希由 `binding/` 的用例覆盖）。
#[derive(Default)]
pub(crate) struct RecordingMinter {
    pub(crate) calls: Mutex<Vec<String>>,
}

#[async_trait]
impl BindingMinter for RecordingMinter {
    async fn mint(
        &self,
        _workspace_id: Id,
        _installation_id: Id,
        channel_user_id: &str,
    ) -> Result<MintedBinding, String> {
        self.calls
            .lock()
            .expect("lock")
            .push(channel_user_id.to_string());
        Ok(MintedBinding {
            raw: "e2e-token".to_string(),
            expires_at: chrono::Utc::now(),
        })
    }
}

/// 出站记账替身：断言 `kind` / `route_revision` / 消息 id 真的到了账本。
#[derive(Default)]
pub(crate) struct RecordingLedger {
    pub(crate) records: Mutex<Vec<OutboundRecord>>,
}

#[async_trait]
impl OutboundLedger for RecordingLedger {
    async fn record_outbound(&self, record: &OutboundRecord) -> Result<(), String> {
        self.records.lock().expect("lock").push(record.clone());
        Ok(())
    }
}

pub(crate) struct RoundTrip {
    pub(crate) router: Arc<Router>,
    pub(crate) minter: Arc<RecordingMinter>,
    pub(crate) ledger: Arc<RecordingLedger>,
    pub(crate) audit: Arc<RecordingAudit>,
    pub(crate) dedup: Arc<MemoryDedup>,
}

fn installation_row() -> InstallationRow {
    InstallationRow {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        // 身份解密器 ⇒ 这两个密文就是明文令牌。
        config: json!({
            "app_id": "A1",
            "team_id": "T1",
            "bot_user_id": "UBOT",
            "bot_token_encrypted": base64_standard(b"xoxb-e2e"),
            "app_token_encrypted": base64_standard(b"xapp-e2e"),
        }),
    }
}

/// 装配一条**完整的** Slack 回路（入站真代码 + 出站真代码 + 真 wire）。
pub(crate) fn round_trip(identity: Arc<dyn IdentityResolver>) -> RoundTrip {
    let row = installation_row();
    let minter = Arc::new(RecordingMinter::default());
    let ledger = Arc::new(RecordingLedger::default());
    let audit = Arc::new(RecordingAudit::default());
    let dedup = Arc::new(MemoryDedup::default());

    let replier: Arc<dyn OutboundReplier> = Arc::new(SlackOutboundReplier::new(
        Arc::new(Sender::http()),
        Decrypter::plaintext(),
        Some(Arc::clone(&minter) as Arc<dyn BindingMinter>),
        Some(Arc::clone(&ledger) as Arc<dyn OutboundLedger>),
        "https://app.example",
        None,
    ));

    let set = ResolverSet::new(
        Arc::new(FakeInstallations(row)),
        identity,
        Arc::clone(&dedup) as Arc<dyn Deduper>,
        Arc::new(UnusedSession),
        Arc::clone(&audit) as Arc<dyn Auditor>,
        "slack_chat",
    )
    .with_replier(replier);

    let router = Arc::new(Router::new(
        Arc::new(NoCommands),
        Arc::new(NoTrigger),
        Arc::new(NoReader),
        Arc::new(NoIssues),
        RouterConfig::default(),
    ));
    router.register(ChannelKind::Slack, set);
    RoundTrip {
        router,
        minter,
        ledger,
        audit,
        dedup,
    }
}

/// 一条接到真流水线上的 Slack 连接（`with_outbound` 是 M7-4 的接线点）。
pub(crate) fn channel(router: &Arc<Router>) -> SlackChannel {
    SlackChannel::new(
        "A1",
        "UBOT",
        "xapp-e2e",
        "xoxb-e2e",
        Some(Arc::clone(router) as SharedInboundHandler),
        Arc::new(TungsteniteTransport),
    )
    .with_outbound(Arc::new(Sender::http()))
}
