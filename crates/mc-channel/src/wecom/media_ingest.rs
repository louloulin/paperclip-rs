//! `media_ingest.go`（480 行）的本地落点：`WeCom` 的 `engine.MediaResolver`。
//!
//! - **写者**：M7-18（`LUM-1783` / `docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §35 的 D1）。
//! - **上游定位**（文件头逐字）：形态是 `lark/media_ingest.go` 立下、而 `Router` 依赖的那个：
//!   [`MediaResolver::has_media`] 是**纯内存**看一眼我们已经拿到的载荷，`resolve_media` 脱离
//!   连接器的 ACK 路径运行，每一次上传都由一条**在 PUT 之前**写下的意图账本行覆盖，而**没有
//!   任何东西**在线上被删掉 —— 任何一处失败都把那行留给对账器、把消息的占位文本原样留下。
//!
//!   `WeCom` 专有的是中间那一段：它交出一把**预签名的 COS URL** 与一把**每 URL 现铸的密钥**，
//!   而不是一个用租户令牌去取的资源 id ⇒ 这里**没有 API 客户端、也没有凭据** —— 只有一次
//!   HTTP GET 与一次解密（[`super::media_download`] / [`super::media_crypt`]）。回调体除了那两根
//!   字符串之外**什么也没说**关于这个文件的事，所以名字来自下载的 `Content-Disposition`、
//!   而类型由名字推出来、或者从解密后的字节里嗅出来。
//!
//! # 三处接缝（本片不实现，逐条交接）
//!
//! | 接缝 | 上游是什么 | 本仓为什么是接缝 | 归谁 |
//! | --- | --- | --- | --- |
//! | [`decode_wecom_inbound`] | `wecom_resolvers.go` 的 `wecomMsgFromRaw` | 那个文件属 **M7-19**（`docs/fixtures/m7-slice-upstream-files.tsv`） | 交接 H1：M7-19 落地时**收敛**成一份 |
//! | [`MediaNotifier`] | `sendersRegistry` 上那条活连接 | `senders_registry.go` 属 **M7-20** | 交接 H2 |
//! | [`MediaStorage`] / [`MediaStreamStorage`] | `storage.Storage` / `storage.S3Storage` 的流式那一半 | 与 `dingtalk::media::MediaStorage` 同款：**adapter 之间零互相依赖** ⇒ 端口在本文件重新声明（收敛归 M7-21） | 宿主装配 |
//!
//! # 与上游的两点形态差异（登记 `docs/32` §35 的 D10）
//!
//! 1. **同步端口 → 一次桥**：本仓的 [`MediaResolver::resolve_media`] 是**同步**签名（M7-1 定的
//!    契约），而下载与意图账本都是 `async`。落法是**独立线程 + current-thread 运行时**跑整段
//!    摄入（与 `dingtalk::media::block_on_engine` 同款），**一次**桥住整条消息而不是每个附件一次。
//! 2. **`errors.Is(err, errMediaTooLarge)` 的三分之一 + 块闸那一格**：上游把"太大"与"被拒"分成
//!    两个类别（`mediaFailureTooLarge` / `mediaFailureBlocked`）好让日志与用户文案各说各的。
//!    本仓逐条保留，且**判决的来源是类型而不是字符串**（[`MediaIngestError`] 的变体）。

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use mc_core::channel::message::{InboundMessage, MediaRef, MessageKind};
use mc_core::id::Id;
use serde_json::Value;

use crate::engine::resolvers::{
    MediaIntentLedger, MediaResolver, RecordPendingMediaObjectParams, ResolvedIdentity,
    ResolvedInstallation,
};
use crate::wecom::ws_frame::{CHAT_TYPE_GROUP_INT, CHAT_TYPE_SINGLE_INT};

use super::media_crypt::MediaCryptError;
use super::media_download::{open_media, MediaDownloadError, MediaHeaders, MEDIA_MAX_BYTES};
use super::media_stream::{decrypt_to_file, peek_file, MediaStreamError};

/// 附件没收到时给发送者看的第一句话（上游 `mediaUnreadableNotice`）。
///
/// `WeCom` 部署只在中国，所以这些跟着这个 adapter 已经在写的那套中文产品语气。
pub const MEDIA_UNREADABLE_NOTICE: &str = "抱歉，有附件没能收到，麻烦重新发一次。";

/// 附件太大时给发送者看的那一句（上游 `mediaTooLargeNotice`）。
pub const MEDIA_TOO_LARGE_NOTICE: &str = "抱歉，附件太大了，我这边收不下。";

/// 一次附件摄入的失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaIngestError {
    /// 下载层（含地址闸的拒绝与超帽）。
    #[error(transparent)]
    Download(#[from] MediaDownloadError),
    /// 解密层。
    #[error(transparent)]
    Crypt(#[from] MediaCryptError),
    /// 流式那条路。
    #[error(transparent)]
    Stream(#[from] MediaStreamError),
    /// 意图账本：**没有持久的意图 ⇒ 没有上传**（失败关闭的方向）。
    #[error("wecom media: record media intent failed")]
    Ledger,
    /// 这个 key 已经归对账器了（上游 `!ok` 的那一支）：**绝不复活它**。
    #[error("wecom media: the storage key is owned by the reconciler")]
    ReconcilerOwned,
    /// 上传失败（存储后端给的是一句人可读的描述 —— **不得**含凭据）。
    #[error("wecom media: upload failed")]
    Storage,
    /// 流式那条路不可用（临时文件建不出来、后端说它能流式却又拒了）⇒ 退回缓冲路径，
    /// **不**为一个优化把附件判死。
    #[error("wecom media: streaming ingest unavailable")]
    StreamingUnavailable,
}

/// 上游 `mediaFailure`：坏消息的**种类**，而不是错误本身。
///
/// 发送者被告知"该怎么改"，而"太大"与"没收到"的答案不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFailure {
    /// 读不出来（下载 / 解密 / 存储）。
    Unreadable,
    /// 太大。
    TooLarge,
    /// 块闸**拒了**媒体主机解析出来的那个地址。
    ///
    /// 它与 [`MediaFailure::Unreadable`] 分开是**给运维**的、不是给发送者的：附件本身没有任何
    /// 问题，而在部署自己的配置改变之前，重试多少次都没用。
    Blocked,
}

/// 上游 `classifyMediaFailure`：把一个错误翻成一个类别。
#[must_use]
pub fn classify_media_failure(error: &MediaIngestError) -> MediaFailure {
    match error {
        MediaIngestError::Download(MediaDownloadError::TooLarge) => MediaFailure::TooLarge,
        MediaIngestError::Download(MediaDownloadError::Guard(_)) => MediaFailure::Blocked,
        _ => MediaFailure::Unreadable,
    }
}

/// 上游 `appendFailure`：每个类别只留一条。
///
/// 一条有四个附件的消息全都过期了，是**一件事**出了错，说四遍对谁都没用。
pub fn append_failure(list: &mut Vec<MediaFailure>, failure: MediaFailure) {
    if !list.contains(&failure) {
        list.push(failure);
    }
}

// =====================================================================
// 端口
// =====================================================================

/// 对象存储的**同步**端口（上游 `mediaStorage`）。
///
/// `object_url` 必须是配置的**纯函数** —— 意图账本要在对象存在**之前**把它的 URL 落库。
pub trait MediaStorage: Send + Sync {
    /// 上传一个对象。
    ///
    /// # Errors
    ///
    /// 人可读描述（**不得**含凭据）。
    fn upload(
        &self,
        key: &str,
        data: &[u8],
        content_type: &str,
        filename: &str,
    ) -> Result<String, String>;

    /// key → 对象 URL（**纯函数**，不做 I/O）。
    fn object_url(&self, key: &str) -> String;
}

/// 流式那一半（上游 `mediaStreamStorage`），两个生产后端都实现了它
/// （`storage.S3Storage` / `storage.LocalStorage`）。
///
/// 没有它的后端退回缓冲路径 —— 正确，只是内存不平。
pub trait MediaStreamStorage: Send + Sync {
    /// 从 `data` 流式上传（长度**已知**：临时文件的大小就是它）。
    ///
    /// # Errors
    ///
    /// 人可读描述（**不得**含凭据）。
    fn upload_stream(
        &self,
        key: &str,
        data: &mut dyn Read,
        size: i64,
        content_type: &str,
        filename: &str,
    ) -> Result<String, String>;
}

/// "附件没送到"这句话的投递口（上游 `*sendersRegistry` 上那条活连接，归 **M7-20**）。
///
/// `None` = 没有可用的通知器 ⇒ 只有日志（上游 `r.notify == nil` 同语义）。
pub trait MediaNotifier: Send + Sync {
    /// 往那个聊里说一句话。**失败只记日志**（调用方已经在一条出错的路上了）。
    fn notify(&self, installation_id: Id, chat_id: &str, chat_type: i32, text: &str);
}

/// `MediaNotifier` 的 no-op 实现（一个没有注册表的部署）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullNotifier;

impl MediaNotifier for NullNotifier {
    fn notify(&self, _installation_id: Id, _chat_id: &str, _chat_type: i32, _text: &str) {}
}

// =====================================================================
// 平台载荷（解码接缝，交接 H1）
// =====================================================================

/// 一个可下载的附件（上游 `InboundMedia`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundMedia {
    /// 附件行被打上的归一化类型（上游 `channel.MsgType`）。
    pub kind: MessageKind,
    /// 预签名的 COS 地址，五分钟有效、不需要任何 access token。
    pub url: String,
    /// 解锁 URL 背后那份字节的密钥（长连接模式每条 URL 现铸一把）。
    pub aes_key: String,
}

/// `WeCom` 侧的摊平信封（上游 `ws_frame.go` 的 `InboundMessage`，**只取本面用到的那几列**）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WecomInbound {
    /// 这条事件投递到的智能机器人标识（安装解析器用的路由键）。
    pub bot_id: String,
    /// `WeCom` 的每条消息标识（两阶段去重用它）。
    pub msg_id: String,
    /// 会话判别式（`"single"` = 一对一、`"group"` = 群聊）。
    pub chat_type: String,
    /// 这条消息来自的 `userid`（单聊）或 `chatid`（群聊）。
    pub chat_id: String,
    /// 打这条消息的人。
    pub sender_user_id: String,
    /// 要取回的附件，按用户发出的顺序。
    pub media: Vec<InboundMedia>,
}

/// 上游 `wecomMsgFromRaw`：从 `channel.InboundMessage.Raw` 里解出 `WeCom` 侧的信封。
///
/// 🔴 **这份解码暂住本片**（交接 H1）：`wecom_resolvers.go` 属 M7-19，而本片的流式与缓冲两条路
/// 都要读 `raw.media` 才能干活 ⇒ 先按上游 `ws_frame.go` 的 **JSON tag 字面量**解一遍，
/// M7-19 落地 `wecomMsgFromRaw` 时**收敛成一份**（一个片自己写第三份读法，正是上游那份文件要
/// 消灭的东西）。
///
/// 解不出来 ⇒ `None`（**失败关闭**：不猜、不把 `raw` 当空集悄悄跳过）。
#[must_use]
pub fn decode_wecom_inbound(raw: &Value) -> Option<WecomInbound> {
    let object = raw.as_object()?;
    let string = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let mut media = Vec::new();
    // 形态不对的 `media`（存在但不是数组）⇒ **失败关闭**：Go 的 `json.Unmarshal` 在那一格报错，
    // 于是上游的 `wecomMsgFromRaw` 交回一个错误、`HasMedia` 交 `false`。缺省 / `null` 才是空表。
    let items: &[Value] = match object.get("media") {
        None | Some(Value::Null) => &[],
        Some(Value::Array(items)) => items,
        Some(_) => return None,
    };
    {
        for item in items {
            let item = item.as_object()?;
            let kind = item
                .get("kind")
                .and_then(Value::as_str)
                .and_then(MessageKind::from_str_opt)
                .unwrap_or(MessageKind::Unknown);
            let url = item.get("url").and_then(Value::as_str).unwrap_or_default();
            let aes_key = item
                .get("aeskey")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if url.is_empty() || aes_key.is_empty() {
                return None;
            }
            media.push(InboundMedia {
                kind,
                url: url.to_owned(),
                aes_key: aes_key.to_owned(),
            });
        }
    }
    Some(WecomInbound {
        bot_id: string("bot_id"),
        msg_id: string("msg_id"),
        chat_type: string("chat_type"),
        chat_id: string("chat_id"),
        sender_user_id: string("sender_user_id"),
        media,
    })
}

// =====================================================================
// 解析器
// =====================================================================

/// `WeCom` 媒体解析端口的上游实现（上游 `wecomMediaResolver`）。
pub struct WecomMediaResolver {
    storage: Arc<dyn MediaStorage>,
    stream_storage: Option<Arc<dyn MediaStreamStorage>>,
    ledger: Arc<dyn MediaIntentLedger>,
    notify: Option<Arc<dyn MediaNotifier>>,
    /// 永远**不是**一把裸 `reqwest::Client`：被取的那个 URL 是从 wire 上下来的，而这把客户端
    /// 拒绝连到任何不是公网的地方（[`super::media_guard`]）。
    client: reqwest::Client,
}

impl std::fmt::Debug for WecomMediaResolver {
    /// 手写：端口都是 trait 对象 ⇒ 只报**存在性**（与 `ChannelDeps` 同款）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WecomMediaResolver")
            .field("storage", &"<dyn MediaStorage>")
            .field("stream_storage", &self.stream_storage.is_some())
            .field("ledger", &"<dyn MediaIntentLedger>")
            .field("notify", &self.notify.is_some())
            .finish_non_exhaustive()
    }
}

impl WecomMediaResolver {
    /// 装配。`storage` 与 `ledger` 是必需的 —— 缺任何一个都没有一个持久的东西可以把附件指过去，
    /// 于是解析器退化成"把占位文本留在原地"。
    ///
    /// # Errors
    ///
    /// [`MediaIngestError::StreamingUnavailable`] 只在客户端建不出来时（TLS 后端缺失）——
    /// 那时连缓冲路径也走不了。
    pub fn new(
        storage: Arc<dyn MediaStorage>,
        ledger: Arc<dyn MediaIntentLedger>,
        notify: Option<Arc<dyn MediaNotifier>>,
    ) -> Result<Self, MediaIngestError> {
        let client =
            super::media_guard::new_media_http_client(super::media_guard::MediaGuard::new())
                .map_err(|_| MediaIngestError::StreamingUnavailable)?;
        Ok(Self {
            storage,
            stream_storage: None,
            ledger,
            notify,
            client,
        })
    }

    /// 换掉流式后端（上游那条类型断言）。
    #[must_use]
    pub fn with_stream_storage(mut self, storage: Arc<dyn MediaStreamStorage>) -> Self {
        self.stream_storage = Some(storage);
        self
    }

    /// 换掉下载客户端（**用例**注入一个地址策略宽松的闸；生产一般不动）。
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// 上游 `HasMedia`（同步、纯内存）。
    #[must_use]
    pub fn has_media(&self, message: &InboundMessage) -> bool {
        decode_wecom_inbound(&message.raw).is_some_and(|inbound| !inbound.media.is_empty())
    }
}

impl MediaResolver for WecomMediaResolver {
    /// 上游逐字：**纯解码**（无 I/O）。它跑在连接器的 ACK 路径上，并决定这条消息要不要付一次
    /// 媒体截止时刻、一次延后的运行与一个信号量位置的代价。
    fn has_media(&self, message: &InboundMessage) -> bool {
        WecomMediaResolver::has_media(self, message)
    }

    /// 上游 `ResolveMedia`：下载、解密、存储这条消息上的每一个附件，返回一份多了
    /// "每个落地的对象一条 `MediaRef`"的消息。
    ///
    /// 附件彼此独立：一个失败不阻止其余的，而发送者在最后被**一次**告知没到的是什么。
    ///
    /// ⚠️ **同步签名 + 一次桥**（本片 D10）：整段工作跑在一个独立线程的 current-thread 运行时上。
    fn resolve_media(
        &self,
        installation: &ResolvedInstallation,
        _sender: &ResolvedIdentity,
        _session_id: Id,
        chat_message_id: Option<Id>,
        message: &InboundMessage,
    ) -> InboundMessage {
        let Some(inbound) = decode_wecom_inbound(&message.raw) else {
            tracing::warn!(
                message_id = message.message_id,
                "wecom media ingest skipped: raw decode failed"
            );
            return message.clone();
        };
        if inbound.media.is_empty() {
            return message.clone();
        }
        let Some(chat_message_id) = chat_message_id else {
            tracing::warn!(
                message_id = message.message_id,
                "wecom media ingest skipped: no chat message row"
            );
            return message.clone();
        };
        // 一次桥住整条消息（上游一次 `ResolveMedia` 调用里跑完所有附件）。
        // 桥起不来（建不出运行时）⇒ 原样交回**没有附着任何媒体**的消息：意图行还没落，
        // 所以那条消息只是没有附件，而不是一个被谎报成成功的摄入。
        let original = message.clone();
        let input = message.clone();
        block_on_ingest(async move {
            let mut resolved = input;
            let mut failures = Vec::new();
            for (index, media) in inbound.media.iter().enumerate() {
                match self
                    .ingest_one(installation, chat_message_id, &inbound, index, media)
                    .await
                {
                    Ok(reference) => resolved.media_refs.push(reference),
                    Err(error) => {
                        let failure = classify_media_failure(&error);
                        append_failure(&mut failures, failure);
                        log_failure(installation, &inbound, index, media, failure, &error);
                    }
                }
            }
            self.tell_the_sender(installation, &inbound, &failures);
            resolved
        })
        .unwrap_or(original)
    }
}

/// 上游 `ResolveMedia` 里那条日志（**url 与 key 永不进日志**）。
fn log_failure(
    installation: &ResolvedInstallation,
    inbound: &WecomInbound,
    index: usize,
    media: &InboundMedia,
    failure: MediaFailure,
    error: &MediaIngestError,
) {
    // 一次被拒的地址**单独一行**，因为运维下一步的动作与这里其它每一种失败都不同：
    // 附件本身没问题，而部署**拒绝**拨它的主机解析出来的地方 ⇒ 办法是配置而不是重试。
    // 只说 "ingest failed" 会把他们送去查 `WeCom`。
    //
    // url 与 key 都不进日志：一个是别人随后就能取的签名地址、另一个能解开它。
    // `MediaDownloadError` 的变体没有一个带载荷，所以整条 error 可以安全地记。
    if failure == MediaFailure::Blocked {
        tracing::warn!(
            installation_id = %installation.id,
            msg_id = inbound.msg_id,
            attachment = index,
            kind = media.kind.as_str(),
            error = %error,
            "wecom media ingest refused by the media address guard: the host resolved to a non-public address and was not dialed. If this deployment sits behind a fake-IP proxy, declare its range in MULTICA_WECOM_MEDIA_ALLOW_CIDRS; otherwise this is a URL that should not have been sent."
        );
        return;
    }
    tracing::warn!(
        installation_id = %installation.id,
        msg_id = inbound.msg_id,
        attachment = index,
        kind = media.kind.as_str(),
        error = %error,
        "wecom media ingest failed"
    );
}

impl WecomMediaResolver {
    /// 上游 `ingestOne`：把一个附件从 url 带成一条 `MediaRef`。
    ///
    /// 账本行**先落**：从那一刻起每一次失败 —— 下载、解密、上传、进程崩 —— 都留下一条意图让
    /// 对账器去收尾，而这里**什么都不删**。
    async fn ingest_one(
        &self,
        installation: &ResolvedInstallation,
        chat_message_id: Id,
        inbound: &WecomInbound,
        index: usize,
        media: &InboundMedia,
    ) -> Result<MediaRef, MediaIngestError> {
        let key = media_object_key(
            installation,
            chat_message_id,
            &inbound.msg_id,
            index,
            media.kind,
        );
        let link = self.storage.object_url(&key);
        let ok = self
            .ledger
            .record_pending_media_object(RecordPendingMediaObjectParams {
                storage_key: key.clone(),
                workspace_id: installation.workspace_id,
                chat_message_id: Some(chat_message_id),
                storage_url: link.clone(),
                installation_id: Some(installation.id),
            })
            .await
            .map_err(|_| MediaIngestError::Ledger)?;
        if !ok {
            // 这个 key 归对账器 ⇒ 绝不复活它。
            return Err(MediaIngestError::ReconcilerOwned);
        }

        if let Some(streamer) = self.stream_storage.as_ref() {
            // 内存平坦的那条路（缩略语：密文从 socket 流进解密、明文落进一个已 unlink 的临时文件、
            // 上传从那里读回来）。除了"流式这条不可用"，其它错误都原样上报。
            match self
                .ingest_streaming(streamer, installation, inbound, index, media, &key, &link)
                .await
            {
                Err(MediaIngestError::StreamingUnavailable) => {}
                other => return other,
            }
        }

        let fetched = super::media_download::download_media(&self.client, &media.url).await?;
        trace_media_headers(installation, inbound, index, &fetched.headers);
        let plain = super::media_crypt::decrypt_media(&media.aes_key, &fetched.body)?;
        let (filename, content_type) =
            describe_media(inbound, index, media, &fetched.headers.filename, &plain);
        self.storage
            .upload(&key, &plain, &content_type, &filename)
            .map_err(|_| MediaIngestError::Storage)?;
        Ok(MediaRef {
            message_kind: media.kind,
            storage_key: key,
            storage_url: link,
            filename,
            mime_type: content_type,
            size_bytes: i64::try_from(plain.len()).unwrap_or(i64::MAX),
            // 上游**不设**这两个字段（零值）：wecom 的附件不替换正文里的任何占位标记，
            // 它作为独立的附件存在。`index` 已经进了对象 key 与日志，不进这一对。
            inline_placeholder: String::new(),
            inline_index: 0,
        })
    }

    /// 上游 `ingestStreaming`：把一个附件**从不让它整体驻留内存**地带过来。
    #[allow(clippy::too_many_arguments)]
    async fn ingest_streaming(
        &self,
        streamer: &Arc<dyn MediaStreamStorage>,
        installation: &ResolvedInstallation,
        inbound: &WecomInbound,
        index: usize,
        media: &InboundMedia,
        key: &str,
        link: &str,
    ) -> Result<MediaRef, MediaIngestError> {
        let (mut body, headers) = open_media(&self.client, &media.url).await?;
        trace_media_headers(installation, inbound, index, &headers);
        let (mut file, size) = match decrypt_to_file(&media.aes_key, &mut body, Path::new("")).await
        {
            // 建不出临时文件**才**是退回的理由（上游 `strings.Contains(err.Error(), "media temp file")`
            // 的等价物，而本仓的判据是**类型**而不是字符串）。
            Err(MediaStreamError::TempFile) => {
                return Err(MediaIngestError::StreamingUnavailable);
            }
            Err(error) => return Err(MediaIngestError::Stream(error)),
            Ok(decrypted) => decrypted,
        };
        // 类型从文件头部嗅出来，而不是从整份东西 —— `http.DetectContentType` 也只读 512 字节。
        let head = peek_file(&mut file, 512)?;
        let (filename, content_type) =
            describe_media(inbound, index, media, &headers.filename, &head);
        streamer
            .upload_stream(key, &mut file, size, &content_type, &filename)
            .map_err(|_| MediaIngestError::Storage)?;
        Ok(MediaRef {
            message_kind: media.kind,
            storage_key: key.to_owned(),
            storage_url: link.to_owned(),
            filename,
            mime_type: content_type,
            size_bytes: size,
            inline_placeholder: String::new(),
            inline_index: 0,
        })
    }

    /// 上游 `tellTheSender`：往附件来自的那个聊里写一句短通知，走的是每条别的 `WeCom` 消息
    /// 都走的那条活 socket。
    ///
    /// 没有它，这次失败会以一种最糟的方式隐形：存下来的正文仍然写着 `[图片]`，于是 agent 就当
    /// 自己看过一张从未收到的图片来回答。agent 那一轮本身**刻意**照常走完 —— 人自己看得出来
    /// 图片没送到，而他们在旁边打的那句话的答案仍然值得要。
    fn tell_the_sender(
        &self,
        installation: &ResolvedInstallation,
        inbound: &WecomInbound,
        failures: &[MediaFailure],
    ) {
        if failures.is_empty() {
            return;
        }
        let Some(notifier) = self.notify.as_ref() else {
            return;
        };
        let chat_id = if inbound.chat_id.is_empty() {
            inbound.sender_user_id.clone()
        } else {
            inbound.chat_id.clone()
        };
        if chat_id.is_empty() {
            return;
        }
        let chat_type = if inbound.chat_type.eq_ignore_ascii_case("group") {
            CHAT_TYPE_GROUP_INT
        } else {
            CHAT_TYPE_SINGLE_INT
        };
        let mut lines: Vec<&str> = Vec::new();
        for failure in failures {
            let notice = if *failure == MediaFailure::TooLarge {
                MEDIA_TOO_LARGE_NOTICE
            } else {
                // `Blocked` **刻意**落在"读不出来"那套措辞上。从发送者那一侧看，一个被拒的地址与
                // 一次读不下去的下载是**同一件事** —— 附件没到 —— 而区分它们的是运维必须改什么，
                // 那属于日志。解析出来的地址与签名 url 都不进聊天消息。
                MEDIA_UNREADABLE_NOTICE
            };
            if !lines.contains(&notice) {
                lines.push(notice);
            }
        }
        notifier.notify(installation.id, &chat_id, chat_type, &lines.join("\n"));
    }
}

/// 上游 `traceMediaHeaders` 的**本片那一半**。
///
/// 上游在 `trace.go`（**M7-20**）里实现它、带一层开关与截断。本片只做最小的一步：
/// **记一条包含 `Content-Disposition` 原样值与解出来的名字的日志**，因为那两个值并排就是
/// "文件名看起来不对"的全部诊断依据，而它们**事后无法恢复** —— URL 只活五分钟。
///
/// 交接 H3：M7-20 落地 `trace.rs` 时**收敛**到那个带开关的实现上去。
fn trace_media_headers(
    installation: &ResolvedInstallation,
    inbound: &WecomInbound,
    index: usize,
    headers: &MediaHeaders,
) {
    tracing::info!(
        installation_id = %installation.id,
        dir = "in.media",
        msg_id = inbound.msg_id,
        index,
        content_disposition = headers.disposition.as_str(),
        filename = headers.filename.as_str(),
        "wecom trace"
    );
}

mod describe;

pub use describe::{
    base_content_type, content_type_for_extension, describe_media, fallback_media_name,
    media_extension, media_object_key, safe_media_segment, sniff_content_type,
};

/// 一次摄入的上限（与下载层同一个数，导出一份好让宿主与用例都引用**同一个**常量）。
pub const MEDIA_INGEST_MAX_BYTES: i64 = MEDIA_MAX_BYTES;

/// 在同步上下文里跑一段 async 摄入（见模块文档差异 1）。
///
/// 与 `dingtalk::media::block_on_engine` 同款：独立线程 + current-thread 运行时。失败 ⇒ 原样
/// 交回**没有附着任何媒体**的消息（上游"任何一处失败都留下那行意图、消息照常入库"）。
fn block_on_ingest<F>(future: F) -> Option<InboundMessage>
where
    F: std::future::Future<Output = InboundMessage> + Send,
{
    // `thread::scope`（而不是 `'static` 的 spawn）：这段 future **借用**解析器与安装行，
    // 而它们都比这个线程活得久。
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()?;
                Some(runtime.block_on(future))
            })
            .join()
            .ok()
            .flatten()
    })
}

#[cfg(test)]
mod tests;
