//! Slack 媒体摄入（上游 `internal/integrations/slack/media_ingest.go`，445 行）。
//!
//! - **写者**：M7-3（`docs/60-M7-PLAN.md` §3.3）。
//! - **跑在 ACK 路径之外**：消息被接受并落库**之后**才下载 / 上传，网络与存储 I/O 都不在
//!   connector 的 ACK 关键路径上（Router 的 `tokio::spawn` 里跑，见 `engine/router/media.rs`）。
//! - **`has_media` 只解已经译好的事件载荷**（纯内存、无 I/O）：`false` 让消息留在普通入库
//!   路径上（没有 pending 标记、不延后 run、不占并发槽）。
//! - **`resolve_media` 先落意图账本再上传**：崩在中间也留下足够的行让对账器收尾；
//!   任何一行都**不删**。
//!
//! # 凭据边界（本文件是它的实现点）
//!
//! Slack 的私有文件 URL 要本安装的 bot token 当 bearer 凭据。所以下载客户端**只**接受
//! HTTPS 且落在 Slack 自己域名上（`slack.com` 或 `*.slack.com`）的 URL，**每一跳重定向**都
//! 重新校验（`net/http` 会把 `Authorization` 带给它返回 nil 的每一跳 ⇒ 那正是凭据边界）。
//!
//! # 与上游的两处**形态**差异（登记 `docs/32` §10）
//!
//! 1. **同步端口**：本仓的 [`MediaResolver::resolve_media`] 是**同步**签名（M7-1 定的契约，
//!    上游是 ctx-async），而 workspace 的 `reqwest` 没开 `blocking` feature 且 M7 不许新增
//!    依赖 / feature ⇒ 默认取回器 [`ThreadedFetcher`] 在**独立线程**上跑一个 current-thread
//!    运行时。这对**任何**调用上下文都成立（不要求 multi-thread runtime、不会 panic）。
//! 2. **没有 deadline 上下文**：上游按"剩余预算 ÷ 还没取的文件数"给每个文件分摊上限；同步端口
//!    拿不到 Router 的 deadline，所以每个文件用固定的 [`FILE_FETCH_TIMEOUT`]，而"预算耗尽"
//!    由 Router 的 `resolve_remote` 门槛 + [`MAX_FILES_PER_MESSAGE`] 兜底（语义不变：
//!    预算耗尽 ⇒ 不再起新的下载）。

use std::sync::Arc;
use std::time::Duration;

use mc_core::channel::message::{InboundMessage, MediaRef, MessageKind};
use mc_core::id::Id;
use reqwest::Url;

use crate::engine::resolvers::{
    EngineError, EngineResult, MediaIntentLedger, MediaResolver, RecordPendingMediaObjectParams,
    ResolvedIdentity, ResolvedInstallation,
};
use crate::slack::config::{Decrypter, FIELD_BOT_TOKEN};
use crate::slack::inbound::RawEvent;

/// 一条消息最多带几个文件（上游 `maxFilesPerMessage`）。
pub const MAX_FILES_PER_MESSAGE: usize = 10;
/// 单个文件的字节上限（20 MiB，上游 `maxInboundFileBytes`）。
pub const MAX_INBOUND_FILE_BYTES: usize = 20 << 20;
/// 单次取回的固定上限（上游 `fileFetchTimeout`；分摊语义见模块文档第 2 条）。
pub const FILE_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// 最多跟随几跳重定向（上游 `maxDownloadRedirects`）。
pub const MAX_DOWNLOAD_REDIRECTS: usize = 3;

// =====================================================================
// 主机 / URL 判据（纯函数）
// =====================================================================

/// `host` 是否是 Slack 自己的域名（上游 `isSlackFileHost`）。
///
/// `url_private` 落在 `files.slack.com` 上；企业版仍是 `*.slack.com` 的子域。
#[must_use]
pub fn is_slack_file_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "slack.com" || host.ends_with(".slack.com")
}

/// 一个下载 URL 是否能被解析器取回（上游 `isFetchableSlackFileURL`）。
///
/// 入站翻译用它在文件进信封**之前**丢掉解析器永远取不回的那些。
#[must_use]
pub fn is_fetchable_slack_file_url(raw_url: &str) -> bool {
    let Ok(parsed) = Url::parse(raw_url) else {
        return false;
    };
    validate_download_url(&parsed, is_slack_file_host).is_ok()
}

/// 校验一个下载 URL 的形状（上游 `validateDownloadURL`）：非空 host、无 userinfo、
/// HTTPS、且 host 通过 `allowed_host`。
fn validate_download_url(
    parsed: &Url,
    allowed_host: impl Fn(&str) -> bool,
) -> Result<(), MediaError> {
    if parsed.host_str().is_none() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(MediaError::InvalidUrl);
    }
    if parsed.scheme() != "https" {
        return Err(MediaError::InsecureScheme);
    }
    if !allowed_host(parsed.host_str().unwrap_or_default()) {
        return Err(MediaError::BlockedHost);
    }
    Ok(())
}

// =====================================================================
// 端口：对象存储 + 取回
// =====================================================================

/// 对象存储的**同步**端口（上游 `mediaStorage`）。
///
/// `object_url` 必须**不**做 I/O：解析器要在上传**之前**把这条 URL 落进意图账本，
/// 所以它只能是 key 的纯函数。宿主用 `mc-storage`（`StorageProvider::put` 是 async）实现它。
pub trait MediaStorage: Send + Sync {
    /// 上传一个对象，返回它的最终 URL。
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

/// 一次取回的请求（**不**实现 `Debug`：它带 bot token）。
pub struct FetchRequest<'a> {
    /// 要取的 URL（Slack 私有文件 URL —— 也是凭据，不进日志）。
    pub url: &'a str,
    /// 本安装的明文 bot token。
    pub bot_token: &'a str,
}

/// 一次取回的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub data: Vec<u8>,
    /// 响应头给的 content type（已剥参数、已小写），可能为空。
    pub content_type: String,
}

/// 取回端口（**同步**；默认实现 [`ThreadedFetcher`]）。
///
/// 实现**必须**只把 token 发给 Slack 自己的域名，且**每一跳重定向都重新校验**。
pub trait MediaFetcher: Send + Sync {
    /// 取一个文件的字节（含重定向跟随与大小上限）。
    fn fetch(&self, request: FetchRequest<'_>) -> Result<Fetched, MediaError>;
}

/// 默认取回器：**独立线程 + current-thread 运行时**跑 `reqwest`（见模块文档差异 1）。
#[derive(Debug, Clone)]
pub struct ThreadedFetcher {
    /// 单次取回的上限。
    pub timeout: Duration,
}

impl Default for ThreadedFetcher {
    fn default() -> Self {
        Self {
            timeout: FILE_FETCH_TIMEOUT,
        }
    }
}

impl MediaFetcher for ThreadedFetcher {
    fn fetch(&self, request: FetchRequest<'_>) -> Result<Fetched, MediaError> {
        let url = request.url.to_string();
        let bot_token = request.bot_token.to_string();
        let timeout = self.timeout;
        // 对任何调用上下文都成立：不在当前线程上 `block_on`（那在运行时 worker 上会 panic）。
        std::thread::scope(|scope| {
            let join = scope.spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| MediaError::Transport)?;
                runtime.block_on(fetch_following_redirects(&url, &bot_token, timeout))
            });
            join.join().map_err(|_| MediaError::Transport)?
        })
    }
}

/// 跟随重定向地取一个文件：**每一跳**都过 `validate_download_url`，
/// 且每一跳都重新带上 Authorization（上游注释：`net/http` 只在同 host / 其子域上转发它）。
async fn fetch_following_redirects(
    raw_url: &str,
    bot_token: &str,
    timeout: Duration,
) -> Result<Fetched, MediaError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        // 自己跟随：库的默认策略会把 `Authorization` 带过 host 边界，那正是要防的。
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| MediaError::Transport)?;
    let mut current = Url::parse(raw_url).map_err(|_| MediaError::InvalidUrl)?;
    for _ in 0..=MAX_DOWNLOAD_REDIRECTS {
        validate_download_url(&current, is_slack_file_host)?;
        let response = client
            .get(current.clone())
            .bearer_auth(bot_token)
            .send()
            .await
            .map_err(|_| MediaError::Transport)?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(MediaError::Transport)?;
            let next = current.join(location).map_err(|_| MediaError::InvalidUrl)?;
            // 下一跳在循环顶部重新校验（并把 token 重新带上）。
            current = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(MediaError::Http {
                status: response.status().as_u16(),
            });
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(media_content_type)
            .unwrap_or_default();
        let mut data = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|_| MediaError::Read)? {
            data.extend_from_slice(&chunk);
            if data.len() > MAX_INBOUND_FILE_BYTES {
                return Err(MediaError::TooLarge);
            }
        }
        return Ok(Fetched { data, content_type });
    }
    Err(MediaError::TooManyRedirects)
}

/// 取回 / 校验失败（**不带 URL、不带令牌**：Slack 私有 URL 与 token 都是凭据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("slack media: invalid file URL shape")]
    InvalidUrl,
    #[error("slack media: file URL is not HTTPS")]
    InsecureScheme,
    #[error("slack media: blocked non-Slack file host")]
    BlockedHost,
    #[error("slack media: too many redirects")]
    TooManyRedirects,
    #[error("slack media: download failed")]
    Transport,
    #[error("slack media: read failed")]
    Read,
    #[error("slack media: http {status}")]
    Http { status: u16 },
    #[error("slack media: file exceeds the {} MiB limit", MAX_INBOUND_FILE_BYTES >> 20)]
    TooLarge,
}

// =====================================================================
// 解析器
// =====================================================================

/// Slack 媒体解析端口的上游实现（上游 `slackMediaResolver`）。
pub struct SlackMediaResolver {
    decrypt: Decrypter,
    storage: Arc<dyn MediaStorage>,
    ledger: Arc<dyn MediaIntentLedger>,
    fetch: Arc<dyn MediaFetcher>,
}

impl std::fmt::Debug for SlackMediaResolver {
    /// 端口是 trait 对象 ⇒ 只列存在性（解密器自己也是脱敏的）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackMediaResolver")
            .field("decrypt", &self.decrypt)
            .field("storage", &"<dyn MediaStorage>")
            .field("ledger", &"<dyn MediaIntentLedger>")
            .field("fetch", &"<dyn MediaFetcher>")
            .finish()
    }
}

impl SlackMediaResolver {
    /// 装配（存储与意图账本都是必需的；默认取回器 = [`ThreadedFetcher`]）。
    #[must_use]
    pub fn new(
        decrypt: Decrypter,
        storage: Arc<dyn MediaStorage>,
        ledger: Arc<dyn MediaIntentLedger>,
    ) -> Self {
        Self {
            decrypt,
            storage,
            ledger,
            fetch: Arc::new(ThreadedFetcher::default()),
        }
    }

    /// 换掉取回器（用例注入替身；生产一般不动）。
    #[must_use]
    pub fn with_fetcher(mut self, fetch: Arc<dyn MediaFetcher>) -> Self {
        self.fetch = fetch;
        self
    }

    /// 把这条消息译成 `raw`（解不开 ⇒ `None`，按"没有媒体"处理）。
    fn raw_of(message: &InboundMessage) -> Option<RawEvent> {
        if message.raw.is_null() {
            return None;
        }
        serde_json::from_value(message.raw.clone()).ok()
    }
}

impl MediaResolver for SlackMediaResolver {
    /// 纯解码（无 I/O）：只认译好的载荷里**有文件**。
    fn has_media(&self, message: &InboundMessage) -> bool {
        has_media(message)
    }

    /// 下载 + 上传每个文件，返回回填了 `media_refs` 的消息副本。
    ///
    /// 文件之间**互不牵连**：一个失败不影响其余（上游注释逐字）。
    fn resolve_media(
        &self,
        installation: &ResolvedInstallation,
        _sender: &ResolvedIdentity,
        _session_id: Id,
        chat_message_id: Option<Id>,
        message: &InboundMessage,
    ) -> InboundMessage {
        let Some(raw) = Self::raw_of(message) else {
            return message.clone();
        };
        if raw.files.is_empty() {
            return message.clone();
        }
        let Some(chat_message_id) = chat_message_id else {
            // 没有 `chat_message` 行就没法给对象定 key、也没法落意图行 ⇒ 跳过（不猜）。
            tracing::warn!(
                message_id = message.message_id,
                "slack media resolve skipped: no chat message row"
            );
            return message.clone();
        };
        let Some(config) = installation
            .platform
            .as_ref()
            .and_then(|platform| {
                platform.downcast_ref::<crate::slack::resolvers::InstallationRow>()
            })
            .map(|row| row.config.clone())
        else {
            tracing::warn!(
                message_id = message.message_id,
                "slack media resolve skipped: installation platform row unavailable"
            );
            return message.clone();
        };
        let creds = match crate::slack::config::decode_credentials(&config, &self.decrypt) {
            Ok(creds) => creds,
            Err(error) => {
                tracing::warn!(
                    message_id = message.message_id,
                    error = %error,
                    "slack media resolve skipped: decode credentials failed"
                );
                return message.clone();
            }
        };
        let files = &raw.files[..raw.files.len().min(MAX_FILES_PER_MESSAGE)];
        let mut resolved = message.clone();
        for (index, file) in files.iter().enumerate() {
            match self.ingest_one(installation, chat_message_id, index, file, &creds.bot_token) {
                Ok(reference) => resolved.media_refs.push(reference),
                Err(error) => {
                    // 尽力而为：这一条失败留下意图行（或什么都没有），对账器事后收尾。
                    tracing::warn!(
                        message_id = message.message_id,
                        file = index,
                        app_id = installation.kind.as_str(),
                        error = %error,
                        "slack media ingest failed"
                    );
                }
            }
        }
        resolved
    }
}

impl SlackMediaResolver {
    /// 单个文件：URL → `MediaRef`（上游 `ingestOne`）。
    ///
    /// 意图行**先落**：从那一刻起，之后再怎么失败（下载、上传、进程崩）都留下一条意图，
    /// 对账器可以凭它收尾；本函数**不**删任何东西。
    fn ingest_one(
        &self,
        installation: &ResolvedInstallation,
        chat_message_id: Id,
        index: usize,
        file: &crate::slack::inbound::RawFile,
        bot_token: &str,
    ) -> Result<MediaRef, MediaError> {
        // Slack 事先声明了大小 ⇒ 超限的文件在花掉一行意图与整次传输**之前**就被拒。
        // （声明少了也没关系：下载那边还有字节上限兜底。）
        if usize::try_from(file.size.max(0)).unwrap_or(usize::MAX) > MAX_INBOUND_FILE_BYTES {
            return Err(MediaError::TooLarge);
        }
        let key = slack_media_object_key(installation, chat_message_id, file, index);
        let link = self.storage.object_url(&key);
        let owned = self.record_intent(installation, chat_message_id, &key, &link)?;
        if !owned {
            // key 已经归对账器 ⇒ 别复活那一行、更别上传。
            return Err(MediaError::InvalidUrl);
        }
        let fetched = self.fetch.fetch(FetchRequest {
            url: &file.download_url,
            bot_token,
        })?;
        let content_type = slack_file_content_type(file, &fetched.content_type, &fetched.data)?;
        let filename = slack_file_name(file, index, &content_type);
        self.storage
            .upload(&key, &fetched.data, &content_type, &filename)
            .map_err(|_| MediaError::Transport)?;
        Ok(MediaRef {
            message_kind: slack_media_kind(&content_type),
            storage_key: key,
            storage_url: link,
            filename,
            mime_type: content_type,
            size_bytes: i64::try_from(fetched.data.len()).unwrap_or(i64::MAX),
            inline_placeholder: String::new(),
            inline_index: 0,
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
        // 账本是 `async` 端口，而本函数是同步的：在这里做一次**短路**的 `block_on`。
        // future 自己持有 `Arc`（线程上的 future 必须是 `'static`）。
        let future = async move { ledger.record_pending_media_object(params).await };
        block_on_engine(future).map_err(|_| MediaError::Transport)
    }
}

/// 在同步上下文里跑一个 engine 的 future（端口是 async、调用点是 sync）。
///
/// 与 [`ThreadedFetcher`] 同款：独立线程 + current-thread 运行时，对任何调用上下文都成立。
fn block_on_engine<F>(future: F) -> EngineResult<bool>
where
    F: std::future::Future<Output = EngineResult<bool>> + Send + 'static,
{
    let failed = || EngineError::infra("slack media: blocking executor unavailable");
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return Err(failed());
                };
                runtime.block_on(future)
            })
            .join()
            .unwrap_or_else(|_| Err(failed()))
    })
}

// =====================================================================
// 纯函数（上游同名函数）
// =====================================================================

/// 一条消息是否引用了要下载的平台媒体（上游 `HasMedia`，**纯内存、无 I/O**）。
#[must_use]
pub fn has_media(message: &InboundMessage) -> bool {
    SlackMediaResolver::raw_of(message).is_some_and(|raw| !raw.files.is_empty())
}

/// 存储对象该算什么类型（上游 `slackFileContentType`）。
///
/// 未授权 / 缺 scope 的令牌会让 Slack 用 **200 + HTML 登录页**回答，所以"文件本身没声明是 HTML
/// 却拿到 HTML"是一次**失败的下载**，不是文件 —— 把它当附件存下来就是把登录页交给 agent。
pub fn slack_file_content_type(
    file: &crate::slack::inbound::RawFile,
    response_type: &str,
    data: &[u8],
) -> Result<String, MediaError> {
    let declared = media_content_type(&file.mimetype);
    let response_type = media_content_type(response_type);
    if response_type == "text/html" && declared != "text/html" {
        return Err(MediaError::Http { status: 200 });
    }
    if !declared.is_empty() {
        return Ok(declared);
    }
    if !response_type.is_empty() {
        return Ok(response_type);
    }
    Ok(sniff_content_type(data))
}

/// 剥掉 `; charset=…` 参数并小写（上游对两处 content type 都这么做）。
fn media_content_type(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// 内容嗅探（上游用 `net/http.DetectContentType`；本仓不引 `mime` / `http` ⇒
/// 只认它真正会命中的那几种，其余回落到 `application/octet-stream`）。
#[must_use]
pub fn sniff_content_type(data: &[u8]) -> String {
    let head = data.get(..data.len().min(512)).unwrap_or(data);
    if head.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "image/png".to_string();
    }
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg".to_string();
    }
    if head.starts_with(b"GIF8") {
        return "image/gif".to_string();
    }
    if head.starts_with(b"%PDF-") {
        return "application/pdf".to_string();
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return "image/webp".to_string();
    }
    // 可打印 ASCII 占比高 ⇒ 当纯文本（`DetectContentType` 的简化形态）。
    if !head.is_empty()
        && head.iter().all(|byte| {
            byte.is_ascii_graphic() || *byte == b'\n' || *byte == b'\t' || *byte == b'\r'
        })
    {
        return "text/plain".to_string();
    }
    "application/octet-stream".to_string()
}

/// mime → 归一化媒体种类（上游 `slackMediaKind`）。
#[must_use]
pub fn slack_media_kind(content_type: &str) -> MessageKind {
    if content_type.starts_with("image/") {
        MessageKind::Image
    } else if content_type.starts_with("video/") {
        MessageKind::Video
    } else if content_type.starts_with("audio/") {
        MessageKind::Audio
    } else {
        MessageKind::File
    }
}

/// 对象 key：由**这条 chat 消息**（而不是平台消息）派生。
///
/// 平台消息可能被摄入两次（去重认领过期后可重领），共用 key 会让第二次摄入撞进第一次的意图行
/// —— 那可能是一行墓碑，而意图 upsert 会拒绝它，于是媒体**静默丢失**（上游注释逐字）。
#[must_use]
pub fn slack_media_object_key(
    installation: &ResolvedInstallation,
    chat_message_id: Id,
    file: &crate::slack::inbound::RawFile,
    index: usize,
) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(chat_message_id.0.as_bytes());
    hasher.update([0u8]);
    hasher.update(file.id.as_bytes());
    hasher.update([0u8]);
    hasher.update(index.to_string().as_bytes());
    format!(
        "workspaces/{}/slack/{}/{}",
        installation.workspace_id.0,
        installation.id.0,
        hex::encode(hasher.finalize())
    )
}

/// 落库时的显示名（上游 `slackFileName`）：上传者自己给的名字可用就用它，
/// 否则按 file id + 序号 + 扩展名造一个。
#[must_use]
pub fn slack_file_name(
    file: &crate::slack::inbound::RawFile,
    index: usize,
    content_type: &str,
) -> String {
    let name = clean_file_name(&file.name);
    if !name.is_empty() {
        return name;
    }
    format!(
        "slack-file-{}-{}{}",
        safe_media_segment(&file.id),
        index + 1,
        media_extension(content_type)
    )
}

/// 把上传者给的名字洗成一个安全的文件名（上游 `cleanFileName`）。
fn clean_file_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    let normalized = name.replace('\\', "/");
    let base = normalized.rsplit('/').next().unwrap_or_default();
    // `path.Base` 会把 `..` / `...` 原样交回；全是点的名字不是文件名。
    if base.is_empty() || base.trim_matches('.').is_empty() {
        return String::new();
    }
    base.to_string()
}

/// content type → 扩展名（上游 `mediaExtension`；本仓不查 `mime` 数据库 ⇒
/// 表里没有的回落空串，见模块文档差异登记）。
#[must_use]
pub fn media_extension(content_type: &str) -> &'static str {
    match content_type {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "video/mp4" => ".mp4",
        "application/pdf" => ".pdf",
        "text/plain" => ".txt",
        "text/html" => ".html",
        _ => "",
    }
}

/// 把 id 收成文件名里安全的片段（上游 `safeMediaSegment`）。
#[must_use]
pub fn safe_media_segment(segment: &str) -> String {
    let cleaned: String = segment
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 未使用但保留的常量引用点：`FIELD_BOT_TOKEN` 在错误分类里出现（编译期钉住字段名一致）。
const _: &str = FIELD_BOT_TOKEN;

#[cfg(test)]
mod tests;
