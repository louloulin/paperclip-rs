//! WeCom BYO 安装的**真**凭据探针传输（M7-FU / `LUM-2136`；关闭 `docs/32` §31 的 **D9**）。
//!
//! # 这一片接上了什么
//!
//! M7-16（`LUM-1780`）落了探针的 **wire 一半**：`mc_channel::wecom::{ws_frame, ws_sender}`。
//! 但生产接线一直挂着一个 `PendingWsTransport` —— 它恒回
//! `TransportError::Failed { stage: "ws-transport-not-wired" }`，于是 BYO 安装**恒 503**
//! `wecom_credentials_unverifiable`（"凭据没被改动，稍后再试"）。
//! 本文件是那缺失的另一半：**真的拨一次、真的发一次 `aibot_subscribe`、真的读回 ack**。
//!
//! # 依赖方向（为什么放在 `mc-http` 侧而不是 `mc-channel`）
//!
//! `mc-http → mc-channel` 这条边**早已存在**（`mc-http/Cargo.toml:113`），而
//! `mc-channel` **不**依赖 `mc-http` ⇒ 把这个适配器放在 `mc-http` 侧，依赖方向恒为单向，
//! 永不成环，且**不需要**新增 crate、**不需要**动 `Cargo.lock`。
//! 真传输三件套（`ws_frame` / `ws_sender` / `wecom_channel::socket`）在 `mc-channel` 侧
//! **全部**已 `pub` 导出，所以这里只是委派。
//!
//! # 语义：一次连接、一次订阅、不注册、不发布
//!
//! 与 [`HandshakeProbe`] 的契约逐条对齐（上游 `credential_probe.go` 的整段注释就是讲为什么）：
//! 进程里**没有别人**知道这次探针发生过 —— 我们不注册安装、不起监管任务、订阅拿到 ack 就
//! 立刻关掉连接。
//!
//! # 判决的分工（**不要**在这里判"凭据对不对"）
//!
//! 本传输只负责**够不够得着**：拿到 ack 就把 `errcode` 原样交回
//! （`Ok(0)` = 订阅成功；`Ok(code)` = 平台**回了**一个码），够不着才回
//! [`TransportError`]。"这个码算拒绝还是算够不着"由
//! `mc_channel::wecom::credentials::classify_subscribe_ack` 统一判 ——
//! **两个读者共用那一个函数**（本探针与 M7-16 的重连握手），所以"限频，等等"不会在
//! 一处变成 `Unverifiable`、在另一处变成"去修这个安装"。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - 明文密钥只经 `PlaintextSecret` 出现，唯一的出口是 `subscribe_body(..).into_value()`；
//! - 错误**只带阶段名**（`stage`），不带 URL、帧内容、密文或明文
//!   （`tungstenite` 的 `Display` 会把完整 URL 与库的内部形态拼进去 ⇒ 一律映射成固定文案）。

use std::sync::Arc;
#[cfg(test)]
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use mc_channel::wecom::credentials::{PlaintextSecret, ProbeTransport, TransportError};
use mc_channel::wecom::wecom_channel::socket::{TungsteniteDialer, WsDialer};
use mc_channel::wecom::wecom_channel::{DEFAULT_WS_URL, SUBSCRIBE_TIMEOUT};
use mc_channel::wecom::ws_frame::subscribe_body;
use mc_channel::wecom::ws_sender::{SenderError, WsSender};

/// 生产探针传输（`tokio-tungstenite` 拨号 + M7-16 的帧/发送面）。
///
/// 三个可注入旋钮都只为**用例**存在，生产一律用默认值（与 `WeComDeps` 的
/// `with_dialer` / `with_ws_url` 同一手法）。
#[derive(Clone)]
pub struct WsProbeTransport {
    dialer: Arc<dyn WsDialer>,
    ws_url: String,
    subscribe_timeout: Duration,
}

impl std::fmt::Debug for WsProbeTransport {
    /// 手写：端口不可打印 ⇒ 只说**存在性**；URL 是公开端点（不含一次性票据）⇒ 报出来。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WsProbeTransport")
            .field("dialer", &"<dyn WsDialer>")
            .field("ws_url", &self.ws_url)
            .field("subscribe_timeout", &self.subscribe_timeout)
            .finish()
    }
}

impl WsProbeTransport {
    /// 生产形态：`tokio-tungstenite` + 默认端点 + M7-16 的订阅时限。
    #[must_use]
    pub fn production() -> Self {
        Self {
            dialer: Arc::new(TungsteniteDialer),
            ws_url: DEFAULT_WS_URL.to_string(),
            subscribe_timeout: SUBSCRIBE_TIMEOUT,
        }
    }

    /// 换拨号器（用例把真 socket 换成内存替身 —— 本仓的 `tokio-tungstenite` 没有
    /// `handshake` feature，**起不了真 WS 服务端**，替身是唯一可行的那条路）。
    ///
    /// `#[cfg(test)]`：本模块是 `wecom.rs` 的**私有**子模块，生产侧唯一的构造入口是
    /// [`WsProbeTransport::production`] ⇒ 这三个旋钮在非测试构建里没有调用点，
    /// 与 `WeComDeps::with_dialer` / `with_ws_url`（上游同样只给用例）同一手法。
    #[cfg(test)]
    #[must_use]
    pub fn with_dialer(mut self, dialer: Arc<dyn WsDialer>) -> Self {
        self.dialer = dialer;
        self
    }

    /// 覆写端点（用例）。
    #[cfg(test)]
    #[must_use]
    pub fn with_ws_url(mut self, ws_url: impl Into<String>) -> Self {
        self.ws_url = ws_url.into();
        self
    }

    /// 覆写订阅时限（用例钉住超时分支）。
    #[cfg(test)]
    #[must_use]
    pub fn with_subscribe_timeout(mut self, timeout: Duration) -> Self {
        self.subscribe_timeout = timeout;
        self
    }
}

#[async_trait::async_trait]
impl ProbeTransport for WsProbeTransport {
    /// 一次订阅的 ack `errcode`。
    ///
    /// # 形状（与 M7-16 的重连握手**逐条同款**）
    ///
    /// 1. 拨号（失败 ⇒ `TransportError::Failed { stage: "dial" }`）；
    /// 2. `WsSender` 写订阅帧，**同时**跑一个"喂 ack"的读 —— 握手阶段还没有读循环，
    ///    ack 账本得有人喂（上游也是在 `conn` 上直接 `ReadMessage` 等 ack）；
    /// 3. 订阅成功 ⇒ `Ok(0)`；平台回了非零码 ⇒ `Ok(code)`；其余失败 ⇒ `stage` 指到那一步。
    ///
    /// # Errors
    ///
    /// [`TransportError`] —— 只在**够不着**时返回（拨号 / 写 / 读 / 超时）。
    /// 平台**回了**的判决一律走 `Ok(errcode)`，由 `classify_subscribe_ack` 统一分类。
    async fn subscribe_ack(
        &self,
        bot_id: &str,
        secret: &PlaintextSecret,
    ) -> Result<i32, TransportError> {
        let connection = self
            .dialer
            .dial(&self.ws_url)
            .await
            .map_err(|_| TransportError::Failed { stage: "dial" })?;
        let mut reader = connection.reader;
        let sender = Arc::new(WsSender::new(connection.sink));

        let deadline = Instant::now() + self.subscribe_timeout;
        let subscribe = sender.subscribe(Some(deadline), subscribe_body(bot_id, secret));
        let outcome = {
            // 握手阶段的"喂 ack"读：大小/形态不对的帧在握手阶段被丢掉（上游的 `continue`）。
            let feeder = async {
                loop {
                    match reader.next_message().await {
                        Ok(Some(raw)) => {
                            let _ = sender.route_raw(&raw);
                        }
                        Ok(None) => {
                            return Err(TransportError::Failed {
                                stage: "link-closed-during-subscribe",
                            });
                        }
                        Err(_) => {
                            return Err(TransportError::Failed { stage: "read" });
                        }
                    }
                }
            };
            tokio::pin!(feeder);

            tokio::select! {
                outcome = subscribe => outcome,
                reason = &mut feeder => {
                    return Err(reason.unwrap_or(TransportError::Failed { stage: "read" }));
                }
            }
        };

        // 探针跑完即弃：显式关掉，不把连接留在那儿等超时。
        reader.close().await;
        match outcome {
            Ok(_) => Ok(0),
            // 平台**回了**一个码：交回 `classify_subscribe_ack` 去分类，不在这里判"凭据不对"。
            Err(SenderError::Api { code, .. }) => Ok(code),
            Err(SenderError::Sink(_) | SenderError::WriteAttempted { .. }) => {
                Err(TransportError::Failed {
                    stage: "subscribe-write",
                })
            }
            Err(
                SenderError::AckTimeout
                | SenderError::AckAbandoned { .. }
                | SenderError::NotAttempted,
            ) => Err(TransportError::Failed {
                stage: "subscribe-ack-timeout",
            }),
            Err(_) => Err(TransportError::Failed { stage: "subscribe" }),
        }
    }
}

/// 路由用例的传输覆写（**单槽**，全模块共用一个）。
///
/// 只在 `cfg(test)` 下定义 ⇒ 生产二进制**不存在**这个符号（这不是运行时开关，
/// 也不需要任何 feature flag 关它）。
#[cfg(test)]
static OVERRIDE: OnceLock<Arc<dyn ProbeTransport>> = OnceLock::new();

/// 用例装上传输替身。
///
/// 单槽的代价与化解：同一进程内的路由用例共享一个替身，而替身**按 `bot_id` 分支**
/// （接受 / 够不着）⇒ “201 + 落行”与“503 + 一行不落”两条都能在它上面断言，
/// 且两条用例用**不同的 `bot_id`** ⇒ 互不干扰（同进程并行也安全）。
#[cfg(test)]
pub(crate) fn install_test_override(transport: Arc<dyn ProbeTransport>) {
    let _ = OVERRIDE.set(transport);
}

/// 解析该给 `install_service` 的传输：有用例覆写就用它，否则用**生产**传输。
pub(crate) fn resolve_transport() -> Arc<dyn ProbeTransport> {
    #[cfg(test)]
    if let Some(transport) = OVERRIDE.get() {
        return Arc::clone(transport);
    }
    Arc::new(WsProbeTransport::production())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_channel::channel::{ChannelError, ChannelResult};
    use mc_channel::wecom::credentials::{classify_subscribe_ack, ProbeError};
    use mc_channel::wecom::wecom_channel::socket::{DialedConnection, WsReader};
    use mc_channel::wecom::ws_frame::{decode_frame, Frame};
    use mc_channel::wecom::ws_sender::{SinkError, WsSink};
    use std::sync::Mutex;
    use std::time::Instant;

    const KEY: [u8; 32] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    fn secret() -> PlaintextSecret {
        PlaintextSecret::new(String::from_utf8(KEY.to_vec()).expect("utf-8"))
    }

    /// 把写出的帧原样记下来的 sink（探针的断言只需要"订阅帧确实出去了"）。
    struct RecordingSink {
        written: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait::async_trait]
    impl WsSink for RecordingSink {
        async fn write_text(
            &mut self,
            payload: &[u8],
            _deadline: Instant,
        ) -> Result<(), SinkError> {
            let value: serde_json::Value = serde_json::from_slice(payload)
                .map_err(|_| SinkError::before_write("subscribe frame was not json"))?;
            self.written.lock().expect("写账锁").push(value);
            Ok(())
        }

        async fn close(&mut self) -> Result<(), SinkError> {
            Ok(())
        }
    }

    /// 按脚本回一帧 ack 的读侧。
    ///
    /// ⚠️ ack 的 `req_id` **从写出的订阅帧里取**（服务端逐字回显 `req_id`，客户端据此配对）
    /// —— 写死一个假 `req_id` 会让 ack 配不上，`subscribe` 只会等到超时。
    struct ScriptedReader {
        /// `Some(errcode)` = 回一个带该码的 ack；`None` = 从头到尾不回帧。
        ack_errcode: Option<i32>,
        written: Arc<Mutex<Vec<serde_json::Value>>>,
        sent: bool,
        /// 脚本用完后是否**真的断链**（`Ok(None)`）。
        ///
        /// `false`（正常探针）时脚本用完就永远挂起：ack 一旦被喂进账本，订阅 future 就该
        /// 判决；若此时报"链关闭"，那是我们自己的替身抢跑了，不是被测行为。
        close_after_frames: bool,
        closed: Arc<Mutex<bool>>,
    }

    #[async_trait::async_trait]
    impl WsReader for ScriptedReader {
        async fn next_message(&mut self) -> ChannelResult<Option<Vec<u8>>> {
            if self.sent {
                if self.close_after_frames {
                    return Ok(None);
                }
                std::future::pending::<()>().await;
            }
            let Some(errcode) = self.ack_errcode else {
                // 从头不回帧 ⇒ 订阅只会等到超时（`subscribe-ack-timeout`）。
                std::future::pending::<()>().await;
                unreachable!("pending 永不返回")
            };
            // 订阅帧可能还在写（`written` 为空）⇒ 稍等一拍再取。
            let req_id = loop {
                let frame = self.written.lock().expect("写账锁").last().cloned();
                if let Some(frame) = frame {
                    break frame["headers"]["req_id"]
                        .as_str()
                        .expect("订阅帧带 req_id")
                        .to_string();
                }
                tokio::task::yield_now().await;
            };
            self.sent = true;
            let ack = serde_json::json!({
                "cmd": "",
                "headers": { "req_id": req_id },
                "errcode": errcode,
                "errmsg": "",
            });
            Ok(Some(ack.to_string().into_bytes()))
        }

        async fn close(&mut self) {
            *self.closed.lock().expect("关锁") = true;
        }
    }

    struct ScriptedDialer {
        ack_errcode: Option<i32>,
        dial_error: bool,
        close_after_frames: bool,
        closed: Arc<Mutex<bool>>,
        written: Arc<Mutex<Vec<serde_json::Value>>>,
        dialled_url: Arc<Mutex<Option<String>>>,
    }

    #[async_trait::async_trait]
    impl WsDialer for ScriptedDialer {
        async fn dial(&self, url: &str) -> ChannelResult<DialedConnection> {
            *self.dialled_url.lock().expect("URL 锁") = Some(url.to_string());
            if self.dial_error {
                return Err(ChannelError::Transport {
                    message: "scripted dial failure".to_string(),
                });
            }
            Ok(DialedConnection {
                sink: Box::new(RecordingSink {
                    written: Arc::clone(&self.written),
                }),
                reader: Box::new(ScriptedReader {
                    ack_errcode: self.ack_errcode,
                    written: Arc::clone(&self.written),
                    sent: false,
                    close_after_frames: self.close_after_frames,
                    closed: Arc::clone(&self.closed),
                }),
            })
        }
    }

    /// 造一个被脚本化的探针 + 它共享的三本账（写帧 / 关连接 / 拨过的 URL）。
    type Rig = (
        WsProbeTransport,
        Arc<Mutex<Vec<serde_json::Value>>>,
        Arc<Mutex<bool>>,
        Arc<Mutex<Option<String>>>,
    );

    fn transport(ack_errcode: Option<i32>, dial_error: bool) -> Rig {
        let written = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(false));
        let dialled_url = Arc::new(Mutex::new(None));
        let dialer = ScriptedDialer {
            ack_errcode,
            dial_error,
            close_after_frames: false,
            closed: Arc::clone(&closed),
            written: Arc::clone(&written),
            dialled_url: Arc::clone(&dialled_url),
        };
        (
            WsProbeTransport::production()
                .with_dialer(Arc::new(dialer))
                .with_subscribe_timeout(Duration::from_millis(500)),
            written,
            closed,
            dialled_url,
        )
    }

    /// 订阅被接受（`errcode 0`）⇒ `Ok(0)`，`classify_subscribe_ack` 判**通过**。
    #[tokio::test]
    async fn an_accepted_subscribe_reports_zero() {
        let (probe, written, closed, dialled_url) = transport(Some(0), false);
        let errcode = probe
            .subscribe_ack("bot-1", &secret())
            .await
            .expect("够得着");
        assert_eq!(errcode, 0);
        assert!(classify_subscribe_ack(errcode).is_ok());

        // 订阅帧**真的出去了**（`aibot_subscribe`），且帧里带 bot_id。
        let written = written.lock().expect("写账锁");
        assert_eq!(written.len(), 1);
        assert_eq!(written[0]["cmd"], "aibot_subscribe");
        assert_eq!(written[0]["body"]["bot_id"], "bot-1");
        drop(written);
        // 拨的是默认端点（aibot 的公开 WS 地址，不是用例地址）。
        assert_eq!(
            dialled_url.lock().expect("URL 锁").as_deref(),
            Some(DEFAULT_WS_URL)
        );
        // 跑完即弃：连接被显式关掉。
        assert!(*closed.lock().expect("关锁"), "探针跑完必须关连接");
    }

    /// 平台回了白名单内的码（`40001`）⇒ **交回那个码**，由 `classify_subscribe_ack` 判拒绝。
    ///
    /// ⚠️ 这条是本片的核心语义差别：传输**不**自己判"凭据不对" —— 够得着就回码。
    #[tokio::test]
    async fn a_rejection_errcode_is_handed_back_not_swallowed() {
        let (probe, ..) = transport(Some(40001), false);
        let errcode = probe
            .subscribe_ack("bot-1", &secret())
            .await
            .expect("够得着");
        assert_eq!(errcode, 40001);
        assert_eq!(
            classify_subscribe_ack(errcode),
            Err(ProbeError::Rejected { errcode: 40001 })
        );
    }

    /// 限频（`45009`）⇒ 不认识的码 ⇒ fail-closed 的 `Unverifiable`，**不是** `Rejected`。
    #[tokio::test]
    async fn a_throttle_code_is_unverifiable_not_rejected() {
        let (probe, ..) = transport(Some(45009), false);
        let errcode = probe
            .subscribe_ack("bot-1", &secret())
            .await
            .expect("够得着");
        assert_eq!(
            classify_subscribe_ack(errcode),
            Err(ProbeError::Unverifiable { errcode: 45009 })
        );
    }

    /// 拨号失败 ⇒ `TransportError`（⇒ 探针判 `Unverifiable { errcode: 0 }`，**不**怀疑密钥）。
    #[tokio::test]
    async fn a_dial_failure_is_a_transport_error() {
        let (probe, ..) = transport(None, true);
        let error = probe
            .subscribe_ack("bot-1", &secret())
            .await
            .expect_err("够不着");
        assert_eq!(error, TransportError::Failed { stage: "dial" });
    }

    /// 链接在 ack 到达前被关掉 ⇒ `TransportError`（不是"凭据被拒"）。
    #[tokio::test]
    async fn a_closed_link_is_a_transport_error() {
        // 这次脚本**真的断链**（`Ok(None)`）且从无 ack。
        let written = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(false));
        let dialer = ScriptedDialer {
            ack_errcode: None,
            dial_error: false,
            close_after_frames: true,
            closed: Arc::clone(&closed),
            written: Arc::clone(&written),
            dialled_url: Arc::new(Mutex::new(None)),
        };
        let probe = WsProbeTransport::production()
            .with_dialer(Arc::new(dialer))
            .with_subscribe_timeout(Duration::from_millis(500));
        let error = probe
            .subscribe_ack("bot-1", &secret())
            .await
            .expect_err("够不着");
        assert!(matches!(error, TransportError::Failed { .. }));
    }

    /// 写出的订阅帧是**合法 JSON 且带 `bot_id`**，读回的 ack 能被 M7-16 的解码器解开
    /// （**两个读者共用同一份帧契约**）。
    ///
    /// ⚠️ 只解 ack：`decode_frame` 逐字设计成只解**入站**帧，解出站请求会得
    /// `FrameError::NotInbound`（那是它的职责边界，不是缺陷）。
    #[tokio::test]
    async fn the_ack_frame_round_trips_through_the_decoder() {
        let (probe, written, ..) = transport(Some(40001), false);
        assert_eq!(
            probe
                .subscribe_ack("bot-1", &secret())
                .await
                .expect("够得着"),
            40001
        );
        let written = written.lock().expect("写账锁");
        assert_eq!(written[0]["cmd"], "aibot_subscribe");

        let ack = serde_json::json!({
            "cmd": "", "headers": { "req_id": "r1" }, "errcode": 40001, "errmsg": "",
        });
        let envelope = decode_frame(&ack.to_string().into_bytes()).expect("ack 可解码");
        match envelope {
            Frame::Response(envelope) => assert_eq!(envelope.errcode, 40001),
            other => panic!("ack 应解成 Response，实得 {other:?}"),
        }
    }

    /// `with_ws_url` 真的换掉了拨号地址（用例唯一的另一个旋钮）。
    #[tokio::test]
    async fn the_endpoint_can_be_overridden() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(false));
        let dialled_url = Arc::new(Mutex::new(None));
        let dialer = ScriptedDialer {
            ack_errcode: Some(0),
            dial_error: false,
            close_after_frames: false,
            closed: Arc::clone(&closed),
            written: Arc::clone(&written),
            dialled_url: Arc::clone(&dialled_url),
        };
        let probe = WsProbeTransport::production()
            .with_dialer(Arc::new(dialer))
            .with_ws_url("wss://example.invalid/socket")
            .with_subscribe_timeout(Duration::from_millis(500));
        assert_eq!(
            probe
                .subscribe_ack("bot-1", &secret())
                .await
                .expect("够得着"),
            0
        );
        assert_eq!(
            dialled_url.lock().expect("URL 锁").as_deref(),
            Some("wss://example.invalid/socket")
        );
    }
}
