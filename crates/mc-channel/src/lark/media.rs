//! lark 的**媒体引用抽取**与摄入（上游 `internal/integrations/lark/media_ingest.go` 501 行）。
//!
//! - **写者**：M7-12（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §29）。
//! - **跑在 ACK 路径之外**：消息被接受并落库**之后**才下载 / 上传，网络与存储 I/O 都不在
//!   connector 的 ACK 关键路径上（Router 的 `spawn` 里跑，见 `engine/router/media.rs`）。
//! - **`has_media` 只解已经译好的事件载荷**（纯内存、**无 I/O**）：`false` 让消息留在普通
//!   入库路径上（没有 pending 标记、不延后 run、不占并发槽）。
//! - **`resolve_media` 先落意图账本再上传**：崩在中间也留下足够的行让对账器收尾；
//!   任何一行都**不删**（上游注释逐字）。
//!
//! # 与上游的两处**形态**差异（登记 `docs/32` §29 的 D 项）
//!
//! 1. **同步端口 + 拉取式响应体**：本仓的 [`MediaResolver::resolve_media`] 是**同步**签名
//!    （M7-1 定的契约，上游是 ctx-async），而 [`ApiClient`] 的下载是 async ⇒ 它在
//!    **独立线程 + current-thread 运行时**上跑一次短路的 `block_on`（与
//!    `slack::media` 的同名手法逐字同款：对**任何**调用上下文都成立，不要求 multi-thread
//!    runtime、不会 panic）。拉取式的 [`ResourceBody`] 因此被一次性读到
//!    传输层自己的上限（[`MAX_MESSAGE_RESOURCE_BYTES`]）。
//! 2. **没有 `UploadStream` 那条路**：上游按 `Content-Length` 是否已知在"流式上传"与
//!    "缓冲后上传"之间分流。本仓的存储端口是**同步**的（见差异 1）⇒ 只有缓冲上传一条路；
//!    传输层的 100 MiB 上限仍然兜住最坏情况（语义不变：超限 ⇒ 拒绝而不是截断）。
//!
//! # 凭据边界
//!
//! 下载要本安装的明文 `app_secret`（经 [`InstallationCredentials`]，`Debug` 已脱敏）。
//! 本文件**零** `tracing::*` 插值凭据或正文：只插 `message_id` / `message_type` /
//! `file`（资源序号）/ `category`（错误类别）。

use std::collections::HashMap;
use std::sync::Arc;

use mc_core::channel::message::{InboundMessage, MediaRef, MessageKind};
use mc_core::id::Id;

pub mod paths;

use paths::first_non_empty;
pub use paths::{
    clean_filename, ensure_audio_filename_extension, is_generic_binary_content_type,
    media_content_type, media_extension, media_filename, media_object_key, safe_path_segment,
};

use super::client::ApiClient;
use super::content_flatten::LarkPostContent;
use super::feishu_channel::{installation_credentials_for, Decrypter, LarkInboundMessage};
use super::http_client::resource::MAX_MESSAGE_RESOURCE_BYTES;
use super::params::{DownloadResourceParams, InstallationCredentials};
use super::resolvers::platform_installation;
use crate::engine::resolvers::{
    EngineError, EngineResult, MediaIntentLedger, MediaResolver, RecordPendingMediaObjectParams,
    ResolvedIdentity, ResolvedInstallation,
};

/// 单条消息资源的字节上限（上游 `maxMessageResourceBytes`，100 MiB）—— 复用传输层那一个常量。
pub const MAX_INBOUND_RESOURCE_BYTES: usize = MAX_MESSAGE_RESOURCE_BYTES;

// =====================================================================
// 端口
// =====================================================================

/// 对象存储的**同步**端口（上游 `mediaStorage`）。
///
/// `object_url` 必须**不**做 I/O：解析器要在上传**之前**把这条 URL 落进意图账本，
/// 所以它只能是 key 的纯函数。宿主用 `mc-storage` 实现它（与 `slack::media::MediaStorage` 同款
/// —— 两个 adapter **各自**声明自己那份端口，见 `docs/60` 的"adapter 之间零互相依赖"）。
pub trait MediaStorage: Send + Sync {
    /// 上传一个对象，返回它的最终 URL。
    ///
    /// # Errors
    ///
    /// 存储层的失败描述（**不得**含资源内容或凭据）。
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

/// 一次下载的结果（拉取式响应体的**内存形态**；见模块文档差异 1）。
///
/// 拆成一个本地类型（而不是把 HTTP 层的拉取式响应体传进制名逻辑）的理由：
/// 命名与类型判定只需要这四个字段，且它们必须能在没有 runtime 的地方使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedResource {
    /// 资源字节。
    pub data: Vec<u8>,
    /// 响应头给的 `Content-Type`（可能为空）。
    pub content_type: String,
    /// `Content-Disposition` 里的文件名（没有就是 `None`）。
    pub filename: Option<String>,
    /// `Content-Length`（说不清时是 `0`）。
    pub size_bytes: i64,
}

/// 媒体摄入失败（上游那批 `slog.Warn` 的分类）。
///
/// 变体**只带结构信息**（序号 / 类别），**不带**资源内容、URL 或凭据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    /// 这条消息没有 `chat_message` 行 ⇒ 定不了 key、也落不了意图行（不猜）。
    #[error("lark media: no chat message row")]
    NoChatMessage,
    /// 安装行的投影不可用（不是本 adapter 造的 `ResolvedInstallation`）。
    #[error("lark media: installation payload unavailable")]
    NoInstallation,
    /// 解密 `app_secret` 失败。
    #[error("lark media: credentials unavailable")]
    Credentials,
    /// 意图账本写失败，或这个 key 已经归对账器（`deleting`）⇒ **不上传**。
    #[error("lark media: intent ledger refused this object")]
    Intent,
    /// 下载失败（链路 / 平台拒绝 / 超过传输层上限）。
    #[error("lark media: download failed")]
    Download,
    /// 上传失败（存储层）。
    #[error("lark media: upload failed")]
    Upload,
}

// =====================================================================
// 一条要下载的资源（上游 `larkMediaResource`）
// =====================================================================

/// 从消息载荷里抽出来的一个可下载资源（上游 `larkMediaResource`）。
///
/// 字段**刻意不 `pub`**：它们只在本文件内流通，外部（包括用例）走
/// [`media_resources_from_message`] 的结论。`key` 是平台 `image_key` / `file_key`
/// —— ⚠️ 它**等价于**凭据（本安装内可用于取回资源）⇒ 它的 `Debug` 只报长度。
#[derive(Clone, PartialEq, Eq)]
pub struct LarkMediaResource {
    key: String,
    kind: MessageKind,
    /// 平台资源类别：`image`（`image_key`）或 `file`（`file_key` 系）。
    fetch_type: &'static str,
    filename: String,
    mime_type: String,
    size_bytes: i64,
    message_id: String,
}

impl std::fmt::Debug for LarkMediaResource {
    /// 手写脱敏：`key` 只报长度（见结构文档）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LarkMediaResource")
            .field("key_len", &self.key.len())
            .field("kind", &self.kind)
            .field("fetch_type", &self.fetch_type)
            .field("filename", &self.filename)
            .field("mime_type", &self.mime_type)
            .field("size_bytes", &self.size_bytes)
            .field("message_id", &self.message_id)
            .finish()
    }
}

impl LarkMediaResource {
    /// 平台资源键（**唯一**出口；调用点因此总是显式可见的）。
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 跨平台的媒体类别。
    #[must_use]
    pub fn kind(&self) -> MessageKind {
        self.kind
    }

    /// 平台资源类别（`image` / `file`）。
    #[must_use]
    pub fn fetch_type(&self) -> &'static str {
        self.fetch_type
    }
}

// =====================================================================
// 抽取（纯函数）
// =====================================================================

/// 一条消息 payload 上挂的**自解析**资源（上游 `mediaResourcesFromMessage`）。
///
/// ⚠️ **纯内存、无 I/O**（`has_media` 在 ACK 路径上调它）。认不出的 `msg_type` 与缺键的
/// payload 都返回空（不是错误）。
#[must_use]
pub fn media_resources_from_message(message: &LarkInboundMessage) -> Vec<LarkMediaResource> {
    #[derive(serde::Deserialize)]
    struct ResourcePayload {
        #[serde(default, rename = "image_key")]
        image_key: String,
        #[serde(default, rename = "file_key")]
        file_key: String,
        #[serde(default, rename = "file_name")]
        file_name: String,
        #[serde(default)]
        name: String,
        #[serde(default, rename = "mime_type")]
        mime_type: String,
        #[serde(default, rename = "content_type")]
        content_type: String,
        #[serde(default)]
        size: i64,
        #[serde(default)]
        size_bytes: i64,
    }
    if message.content.is_empty() {
        return Vec::new();
    }
    let Ok(payload) = serde_json::from_str::<ResourcePayload>(&message.content) else {
        return Vec::new();
    };
    let filename = first_non_empty(&[&payload.file_name, &payload.name]);
    let mime_type = first_non_empty(&[&payload.mime_type, &payload.content_type]);
    let size_bytes = if payload.size_bytes == 0 {
        payload.size
    } else {
        payload.size_bytes
    };
    match message.message_type.as_str() {
        "image" => {
            if payload.image_key.is_empty() {
                return Vec::new();
            }
            vec![LarkMediaResource {
                key: payload.image_key,
                kind: MessageKind::Image,
                fetch_type: "image",
                filename,
                mime_type,
                size_bytes,
                message_id: message.message_id.clone(),
            }]
        }
        "post" => media_resources_from_post(message),
        "media" | "video" => {
            if payload.file_key.is_empty() {
                return Vec::new();
            }
            vec![LarkMediaResource {
                key: payload.file_key,
                kind: MessageKind::Video,
                fetch_type: "file",
                filename,
                mime_type,
                size_bytes,
                message_id: message.message_id.clone(),
            }]
        }
        "file" | "audio" => {
            if payload.file_key.is_empty() {
                return Vec::new();
            }
            let kind = if message.message_type == "audio" {
                MessageKind::Audio
            } else {
                MessageKind::File
            };
            vec![LarkMediaResource {
                key: payload.file_key,
                kind,
                fetch_type: "file",
                filename,
                mime_type,
                size_bytes,
                message_id: message.message_id.clone(),
            }]
        }
        _ => Vec::new(),
    }
}

/// 富文本（`post`）里内嵌的媒体 span（上游 `mediaResourcesFromPost`）。
///
/// ⚠️ 一条 `post` 可能**多次**引用同一个 `image_key` / `file_key`。对象 key 是
/// `(消息, 类型, 键)` 的函数 ⇒ 重复项会上传到**同一个** key 两次：后一次失败的尝试可能毁掉
/// 前一次成功已经产出的对象（悬挂附件），后一次成功则会让一个对象挂上两条附件行。
/// 所以在这里、在任何上传**之前**就把它们合掉。
#[must_use]
pub fn media_resources_from_post(message: &LarkInboundMessage) -> Vec<LarkMediaResource> {
    if message.content.is_empty() {
        return Vec::new();
    }
    let Ok(doc) = serde_json::from_str::<LarkPostContent>(&message.content) else {
        return Vec::new();
    };
    let mut out: Vec<LarkMediaResource> = Vec::new();
    let mut seen: HashMap<String, bool> = HashMap::new();
    for paragraph in &doc.content {
        for span in paragraph {
            match span.tag.as_str() {
                "img" => {
                    if span.image_key.is_empty()
                        || seen
                            .insert(format!("image\u{0}{}", span.image_key), true)
                            .is_some()
                    {
                        continue;
                    }
                    out.push(LarkMediaResource {
                        key: span.image_key.clone(),
                        kind: MessageKind::Image,
                        fetch_type: "image",
                        filename: first_non_empty(&[&span.file_name, &span.name]),
                        mime_type: span.mime_type.clone(),
                        size_bytes: 0,
                        message_id: message.message_id.clone(),
                    });
                }
                "media" => {
                    if span.file_key.is_empty()
                        || seen
                            .insert(format!("file\u{0}{}", span.file_key), true)
                            .is_some()
                    {
                        continue;
                    }
                    out.push(LarkMediaResource {
                        key: span.file_key.clone(),
                        kind: MessageKind::Video,
                        fetch_type: "file",
                        filename: first_non_empty(&[&span.file_name, &span.name]),
                        mime_type: span.mime_type.clone(),
                        size_bytes: 0,
                        message_id: message.message_id.clone(),
                    });
                }
                _ => {}
            }
        }
    }
    out
}

// =====================================================================
// 解析器
// =====================================================================

/// 媒体解析端口的上游实现（上游 `feishuMediaResolver`）。
pub struct LarkMediaResolver {
    api: Arc<dyn ApiClient>,
    decrypter: Decrypter,
    storage: Arc<dyn MediaStorage>,
    ledger: Arc<dyn MediaIntentLedger>,
}

impl std::fmt::Debug for LarkMediaResolver {
    /// 端口是 trait 对象 ⇒ 只列存在性（解密器自己也是脱敏的）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LarkMediaResolver")
            .field("api", &"<dyn ApiClient>")
            .field("decrypter", &self.decrypter)
            .field("storage", &"<dyn MediaStorage>")
            .field("ledger", &"<dyn MediaIntentLedger>")
            .finish()
    }
}

impl LarkMediaResolver {
    /// 装配（四件都是必需的：缺任何一件就跳过摄入，而不是半途而废）。
    #[must_use]
    pub fn new(
        api: Arc<dyn ApiClient>,
        decrypter: Decrypter,
        storage: Arc<dyn MediaStorage>,
        ledger: Arc<dyn MediaIntentLedger>,
    ) -> Self {
        Self {
            api,
            decrypter,
            storage,
            ledger,
        }
    }

    /// 把这条消息译成 payload（解不开 ⇒ `None`，按"没有媒体"处理）。
    fn payload_of(message: &InboundMessage) -> Option<LarkInboundMessage> {
        if message.raw.is_null() {
            return None;
        }
        serde_json::from_value(message.raw.clone()).ok()
    }

    /// 日志用的一条 warn（字段**只**有结构信息，见模块文档的凭据一段）。
    ///
    /// `category` **按值**传（[`MediaError`] 是 1 字节的 `Copy` 枚举；按引用传反而多一层取地址）。
    fn warn(message_id: &str, index: usize, category: MediaError) {
        tracing::warn!(
            message_id,
            file = index,
            category = %category,
            "lark media ingest skipped"
        );
    }

    /// 下载**一个**资源（走 [`ApiClient`] 的流式口，一次性读到传输层上限）。
    fn download(
        &self,
        credentials: &InstallationCredentials,
        resource: &LarkMediaResource,
    ) -> Result<FetchedResource, MediaError> {
        let api = Arc::clone(&self.api);
        let credentials = credentials.clone();
        let params = DownloadResourceParams {
            message_id: resource.message_id.clone(),
            file_key: resource.key.clone(),
            resource_type: resource.fetch_type.to_string(),
        };
        block_on(async move {
            let mut stream = api
                .download_message_resource_stream(credentials, params)
                .await
                .map_err(|_| MediaError::Download)?;
            // 传输层已经在 `Content-Length` 说得清时拒过一次；这里再按字节实读上限兜一次，
            // 并把字节留在内存里（存储端口是同步的，见模块文档差异 1）。
            let data = stream
                .body
                .read_all_capped(
                    "download_message_resource_stream",
                    MAX_INBOUND_RESOURCE_BYTES,
                )
                .await
                .map_err(|_| MediaError::Download)?;
            Ok(FetchedResource {
                data,
                content_type: stream.content_type,
                filename: stream.filename,
                size_bytes: stream.size_bytes,
            })
        })
    }

    /// 落一行意图账本（同步端口上的 `async` 调用：见模块文档差异 1）。
    fn record_intent(
        &self,
        installation: &ResolvedInstallation,
        chat_message_id: Id,
        key: &str,
        link: &str,
    ) -> Result<bool, MediaError> {
        let ledger = Arc::clone(&self.ledger);
        let params = RecordPendingMediaObjectParams {
            storage_key: key.to_string(),
            workspace_id: installation.workspace_id,
            chat_message_id: Some(chat_message_id),
            storage_url: link.to_string(),
            installation_id: Some(installation.id),
        };
        block_on_ledger(async move { ledger.record_pending_media_object(params).await })
    }

    /// 单个资源的完整流程（上游 `ResolveMedia` 的循环体）。
    ///
    /// 意图行**先落**：从那一刻起，之后再怎么失败（下载、上传、进程崩）都留下一条意图，
    /// 对账器可以凭它收尾；本函数**不**删任何东西。
    fn ingest_one(
        &self,
        payload: &LarkInboundMessage,
        installation: &ResolvedInstallation,
        credentials: &InstallationCredentials,
        chat_message_id: Id,
        index: usize,
        resource: &LarkMediaResource,
    ) -> Result<MediaRef, MediaError> {
        let key = media_object_key(installation, chat_message_id, resource);
        let link = self.storage.object_url(&key);
        if !self.record_intent(installation, chat_message_id, &key, &link)? {
            // 这个 key 已经归对账器（'deleting'）：绝不复活那一行、更别上传。
            return Err(MediaError::Intent);
        }
        let fetched = self.download(credentials, resource)?;
        let content_type = media_content_type(resource, &fetched);
        let filename = media_filename(payload, resource, &fetched, &content_type, index);
        self.storage
            .upload(&key, &fetched.data, &content_type, &filename)
            .map_err(|_| MediaError::Upload)?;
        let size_bytes = if fetched.size_bytes == 0 {
            i64::try_from(fetched.data.len()).unwrap_or(i64::MAX)
        } else {
            fetched.size_bytes
        };
        Ok(MediaRef {
            message_kind: resource.kind,
            storage_key: key,
            storage_url: link,
            filename,
            mime_type: content_type,
            size_bytes,
            inline_placeholder: String::new(),
            inline_index: 0,
        })
    }
}

impl MediaResolver for LarkMediaResolver {
    /// 纯解码（无 I/O）：只认译好的载荷里**有可下载资源**。
    fn has_media(&self, message: &InboundMessage) -> bool {
        Self::payload_of(message)
            .is_some_and(|payload| !media_resources_from_message(&payload).is_empty())
    }

    /// 下载 + 上传每个资源，返回回填了 `media_refs` 的消息副本。
    ///
    /// 资源之间**互不牵连**：一个失败不影响其余（上游注释逐字）。
    fn resolve_media(
        &self,
        installation: &ResolvedInstallation,
        _sender: &ResolvedIdentity,
        _session_id: Id,
        chat_message_id: Option<Id>,
        message: &InboundMessage,
    ) -> InboundMessage {
        let Some(payload) = Self::payload_of(message) else {
            return message.clone();
        };
        let resources = media_resources_from_message(&payload);
        if resources.is_empty() {
            return message.clone();
        }
        // 没有 `chat_message` 行就没法给对象定 key、也没法落意图行 ⇒ 跳过（不猜）。
        let Some(chat_message_id) = chat_message_id else {
            tracing::warn!(
                message_id = message.message_id,
                "lark media ingest skipped: no chat message row"
            );
            return message.clone();
        };
        let Some(lark_installation) = platform_installation(installation) else {
            tracing::warn!(
                message_id = message.message_id,
                "lark media ingest skipped: installation payload unavailable"
            );
            return message.clone();
        };
        let Ok(credentials) = installation_credentials_for(lark_installation, &self.decrypter)
        else {
            Self::warn(&message.message_id, 0, MediaError::Credentials);
            return message.clone();
        };
        let mut resolved = message.clone();
        for (index, resource) in resources.iter().enumerate() {
            match self.ingest_one(
                &payload,
                installation,
                &credentials,
                chat_message_id,
                index,
                resource,
            ) {
                Ok(reference) => resolved.media_refs.push(reference),
                Err(error) => Self::warn(&message.message_id, index, error),
            }
        }
        resolved
    }
}

// =====================================================================
// 同步 ↔ 异步的桥（与 `slack::media` 同款，见模块文档差异 1）
// =====================================================================

/// 在同步上下文里跑一个异步块。
///
/// 独立线程 + current-thread 运行时：对任何调用上下文都成立（不要求 multi-thread
/// runtime、在 runtime 里调用也不会 panic）。
fn block_on<F, T>(future: F) -> Result<T, MediaError>
where
    F: std::future::Future<Output = Result<T, MediaError>> + Send + 'static,
    T: Send + 'static,
{
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return Err(MediaError::Download);
                };
                runtime.block_on(future)
            })
            .join()
            .unwrap_or(Err(MediaError::Download))
    })
}

/// [`block_on`] 的账本版（错误面是 engine 的）。
fn block_on_ledger<F>(future: F) -> Result<bool, MediaError>
where
    F: std::future::Future<Output = EngineResult<bool>> + Send + 'static,
{
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return Err(MediaError::Intent);
                };
                runtime
                    .block_on(future)
                    .map_err(|_: EngineError| MediaError::Intent)
            })
            .join()
            .unwrap_or(Err(MediaError::Intent))
    })
}

/// 供诊断：一条 payload 里"是不是有媒体"（与 [`MediaResolver::has_media`] 同一判据）。
#[must_use]
pub fn has_media(message: &LarkInboundMessage) -> bool {
    !media_resources_from_message(message).is_empty()
}

#[cfg(test)]
mod tests;
