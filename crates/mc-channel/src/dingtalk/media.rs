//! `DingTalk` **媒体面**：入站图片的取回 / 上传 / 意图账本（上游
//! `internal/integrations/dingtalk/media.go` 385 行）。
//!
//! - **写者**：M7-8（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §22）。
//! - **跑在 ACK 路径之外**：消息被受理并落库**之后**才下载 / 上传（Router 的 `tokio::spawn`
//!   里跑）。[`MediaResolver::has_media`] 只解**已经译好**的载荷（纯内存、无 I/O）。
//! - **意图行先落**：每个对象在上传**之前**先写 `channel_media_pending_object`；从那一刻起
//!   再怎么失败（下载、上传、进程崩）都留下一条意图，异步对账器可以凭它收尾。本文件
//!   **不删**任何东西。
//!
//! # 出站方向**不**做媒体上传（诚实交代）
//!
//! `DingTalk` 的出站机器人消息只有 `sampleMarkdown` 一种 `msgKey`（上游 `outbound_send.go`
//! 一个字面量），**没有**图片 / 文件发送端点。所以"媒体（`media.go` 385 行）单向一并覆盖"
//! 指的是**入站**这一个方向：平台 → 本仓对象存储。上游同样只有这一向（`media.go` 里没有
//! 任何上传到 `DingTalk` 的调用）。登记在 `docs/32` §22 的 D7。
//!
//! # 不可信出口（本文件的核心安全不变量）
//!
//! `messageFiles/download` 返回的 URL 是**平台签发的短期票据**，而它仍是**不可信出口输入**：
//!
//! 1. 形状校验：非空 host、无 userinfo、无 fragment、`http` 或 `https`（上游逐字）；
//! 2. **自己解析 + 自己拨号**：域名解析出来的**每一个**地址都必须是公网地址，否则整体拒
//!    （`dns_resolver` 端口只交出校验过的地址 ⇒ DNS 重绑定改不了连接去向）；
//! 3. **不用代理**（代理会重新解析目标，绕过上一条）；
//! 4. **每一跳重定向**都重新校验：最多 3 跳、`https → http` 降级一律拒、跨源 `http` 跳转一律拒、
//!    并且**删掉 `Referer`**（查询串本身就是短期 bearer 凭据，不能跨到下一跳）。
//!
//! # 与上游的形态差异（**逐条登记** `docs/32` §22）
//!
//! 1. **同步端口**：本仓的 [`MediaResolver::resolve_media`] 是**同步**签名（M7-1 定的契约），
//!    而 `reqwest` 没开 `blocking` feature ⇒ 默认取回器 [`ThreadedFetcher`] 在**独立线程**上
//!    跑一个 current-thread 运行时（与 `slack::media::ThreadedFetcher` 同款）。
//! 2. **顺序取回**：上游用 `errgroup.SetLimit(2)` 并发取最多 4 张图。本仓的每次取回**自己**
//!    就是一个线程 + 运行时 ⇒ 再做池化只会成倍复制运行时，而内存上界反而更宽松。
//!    这里改成**顺序**取回：语义不变（每张图互不牵连、失败只影响它自己），
//!    上界从"最多 2 × 10 MiB"**收紧**到"最多 1 × 10 MiB"。
//! 3. **没有 `http.DetectContentType`**：只认入站白名单里那五种的**魔数**（更严，不会更松），
//!    且内容类型不由响应头决定（上游也是嗅字节）。
//! 4. **对象存储端口在本文件重新声明**（`slack::media::MediaStorage` 语义相同）：`docs/60`
//!    §2.2 明确要"adapter 之间零互相依赖"，跨 adapter 复用一个端口会把这个方向变成依赖边；
//!    收敛到 engine 归 M7-21。
//!
//! # 凭据面
//!
//! `downloadCode` 与平台签发的下载 URL **都是**短期凭据：本文件不在任何 `tracing::*` 里插值
//! 它们，[`MediaError`] 的变体也**不带** URL / 查询串（`reqwest` 的 `*url::Error` 在类型层面
//! 进不来）。

use std::error::Error as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use mc_core::channel::message::{InboundMessage, MediaRef, MessageKind};
use mc_core::id::Id;
use reqwest::Url;
use sha2::{Digest as _, Sha256};

use crate::dingtalk::inbound::{decode_dingtalk_raw, DingtalkMediaResource, IMAGE_PLACEHOLDER};
use crate::dingtalk::outbound::{Credentials, OpenApiTransport, Sender};
use crate::dingtalk::resolvers::installation_row;
use crate::engine::resolvers::{
    EngineError, EngineResult, MediaIntentLedger, MediaResolver, RecordPendingMediaObjectParams,
    ResolvedIdentity, ResolvedInstallation,
};

/// 一条消息最多取几张图（上游 `maxImagesPerMessage = 4`）。
pub const MAX_IMAGES_PER_MESSAGE: usize = 4;

/// 单张图的大小上限（上游 `maxInboundImageBytes = 10 MiB`）。
pub const MAX_INBOUND_IMAGE_BYTES: usize = 10 << 20;

/// 单次取回的固定上限（上游 `imageFetchTimeout = 30s`；
/// 分摊语义见模块文档差异 2）。
pub const IMAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// 上游的并发上限（上游 `mediaFetchConcurrency = 2`）。
///
/// 本仓**顺序**取回（差异 2）⇒ 这个常量只用来说明"我们的上界更紧"，不再当并发度用。
pub const MEDIA_FETCH_CONCURRENCY: usize = 2;

/// 最多跟随几跳重定向（上游 `maxDownloadRedirects = 3`）。
pub const MAX_DOWNLOAD_REDIRECTS: usize = 3;

/// 入站允许的图片类型 → 文件扩展名（上游 `allowedImageTypes`，逐条）。
pub const ALLOWED_IMAGE_TYPES: [(&str, &str); 5] = [
    ("image/png", ".png"),
    ("image/jpeg", ".jpg"),
    ("image/gif", ".gif"),
    ("image/webp", ".webp"),
    ("image/bmp", ".bmp"),
];

/// 白名单里的扩展名（不是白名单 ⇒ `None`）。
#[must_use]
pub fn image_extension(content_type: &str) -> Option<&'static str> {
    ALLOWED_IMAGE_TYPES
        .iter()
        .find(|(mime, _)| *mime == content_type)
        .map(|(_, extension)| *extension)
}

pub mod guard;

pub use guard::{
    guard_download_url, is_public_download_address, same_download_origin, sniff_image_content_type,
    unmap, validate_download_url, MediaError, NON_PUBLIC_PREFIXES,
};

// =====================================================================
// 端口
// =====================================================================

/// 对象存储的**同步**端口（上游 `storage.Storage` 的媒体子集）。
///
/// `object_url` 必须**不**做 I/O：解析器要在上传**之前**把这条 URL 落进意图账本，
/// 所以它只能是 key 的纯函数。
pub trait MediaStorage: Send + Sync {
    /// 上传一个对象。
    ///
    /// # Errors
    ///
    /// 存储后端失败 ⇒ 人可读描述（**不得**含凭据）。
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

/// 一次取回的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub data: Vec<u8>,
    /// 嗅出来的类型（**一定**在白名单里）。
    pub content_type: &'static str,
}

/// 取回端口（**同步**；默认实现 [`ThreadedFetcher`]）。
///
/// 实现必须：把 `code` 换成短期 URL、按 [模块文档](self) 的四条安全不变量取回、并给出
/// 白名单内的类型。
pub trait MediaFetcher: Send + Sync {
    /// 取一张图（平台只给 `downloadCode`；`ref` 与 `alt` 都要试）。
    ///
    /// # Errors
    ///
    /// 见 [`MediaError`]。
    fn fetch_resource(
        &self,
        credentials: &Credentials,
        resource: &DingtalkMediaResource,
    ) -> Result<Fetched, MediaError>;
}

/// 默认取回器：**独立线程 + current-thread 运行时**（见模块文档差异 1）。
pub struct ThreadedFetcher {
    transport: Arc<dyn OpenApiTransport>,
    timeout: Duration,
}

impl std::fmt::Debug for ThreadedFetcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ThreadedFetcher")
            .field("transport", &"<dyn OpenApiTransport>")
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl ThreadedFetcher {
    /// 装配（端口 + 单次取回上限）。
    #[must_use]
    pub fn new(transport: Arc<dyn OpenApiTransport>, timeout: Duration) -> Self {
        Self { transport, timeout }
    }

    /// 生产形态（默认上限）。
    #[must_use]
    pub fn http(transport: Arc<dyn OpenApiTransport>) -> Self {
        Self::new(transport, IMAGE_FETCH_TIMEOUT)
    }
}

impl Default for ThreadedFetcher {
    fn default() -> Self {
        Self::new(
            Arc::new(crate::dingtalk::outbound::HttpOpenApi::new()),
            IMAGE_FETCH_TIMEOUT,
        )
    }
}

impl MediaFetcher for ThreadedFetcher {
    fn fetch_resource(
        &self,
        credentials: &Credentials,
        resource: &DingtalkMediaResource,
    ) -> Result<Fetched, MediaError> {
        let transport = Arc::clone(&self.transport);
        let credentials = credentials.clone();
        let resource = resource.clone();
        let timeout = self.timeout;
        // 对任何调用上下文都成立：不在当前线程上 `block_on`（那在运行时 worker 上会 panic）。
        std::thread::scope(|scope| {
            let join = scope.spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| MediaError::Transport)?;
                runtime.block_on(fetch_resource(transport, &credentials, &resource, timeout))
            });
            join.join().map_err(|_| MediaError::Transport)?
        })
    }
}

/// 把 `ref` / `alt` 依次换成 URL 并取回（上游 `fetchResource`）。
///
/// 两路都失败 ⇒ 把两条失败合并报告（`errors.Join` 的等价物）。
async fn fetch_resource(
    transport: Arc<dyn OpenApiTransport>,
    credentials: &Credentials,
    resource: &DingtalkMediaResource,
    timeout: Duration,
) -> Result<Fetched, MediaError> {
    if resource.reference.is_empty() {
        return Err(MediaError::NoDownloadCode);
    }
    let sender = Sender::from_credentials(transport, credentials);
    let primary = fetch_by_code(&sender, &resource.reference, timeout).await;
    if primary.is_ok() || resource.alt.is_empty() || resource.alt == resource.reference {
        return primary;
    }
    match fetch_by_code(&sender, &resource.alt, timeout).await {
        Ok(fetched) => Ok(fetched),
        Err(_) => primary,
    }
}

/// 一次解析 + 取回（上游 `fetchByCode` + `fetchBytes`）。
async fn fetch_by_code(
    sender: &Sender,
    code: &str,
    timeout: Duration,
) -> Result<Fetched, MediaError> {
    let url = sender
        .message_file_download_url(code)
        .await
        .map_err(|_| MediaError::Transport)?;
    let client = guarded_client()?;
    fetch_bytes(&client, &url, timeout).await
}

/// 带四条安全不变量的下载客户端。
fn guarded_client() -> Result<reqwest::Client, MediaError> {
    reqwest::Client::builder()
        .timeout(IMAGE_FETCH_TIMEOUT)
        // 代理会重新解析目标 ⇒ 绕过下面那条"只许连校验过的地址"的保证。
        .no_proxy()
        .dns_resolver(Arc::new(PublicOnlyResolver))
        // ⚠️ 上游要删 `Referer`（`net/http` 会把查询串——短期 bearer 凭据——带进下一跳）。
        // `reqwest` **不**自动加 `Referer`（那是 `net/http` 的行为），本 adapter 自己也不设
        // ⇒ 这条保证由构造给出，重定向回调里没有可删的东西（登记 `docs/32` §22 的 D5）。
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_DOWNLOAD_REDIRECTS {
                return attempt.error(MediaError::TooManyRedirects);
            }
            if let Err(error) = guard_download_url(attempt.url()) {
                return attempt.error(error);
            }
            let previous = attempt.previous().last();
            if let Some(previous) = previous {
                if previous.scheme().eq_ignore_ascii_case("https")
                    && !attempt.url().scheme().eq_ignore_ascii_case("https")
                {
                    return attempt.error(MediaError::DowngradeRedirect);
                }
                if attempt.url().scheme().eq_ignore_ascii_case("http")
                    && !same_download_origin(previous, attempt.url())
                {
                    return attempt.error(MediaError::CrossOriginRedirect);
                }
            }
            attempt.follow()
        }))
        .build()
        .map_err(|_| MediaError::Transport)
}

/// 只交出**公网**地址的解析器（上游 `publicDownloadDialer` 的等价物）。
#[derive(Debug)]
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addresses = resolve_public(&host).await?;
            let iterator: Box<dyn Iterator<Item = SocketAddr> + Send> =
                Box::new(addresses.into_iter());
            Ok(iterator)
        })
    }
}

/// 解析一个主机名并要求**每一个**答案都是公网地址（上游逐字：任一非公网 ⇒ 整体拒）。
async fn resolve_public(
    host: &str,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    if let Ok(address) = host.parse::<IpAddr>() {
        let address = unmap(address);
        return if is_public_download_address(address) {
            Ok(vec![SocketAddr::new(address, 0)])
        } else {
            Err(Box::new(MediaError::BlockedAddress))
        };
    }
    let resolved = tokio::net::lookup_host((host, 0)).await?;
    let mut addresses = Vec::new();
    for address in resolved {
        let bare = unmap(address.ip());
        if !is_public_download_address(bare) {
            return Err(Box::new(MediaError::BlockedAddress));
        }
        addresses.push(SocketAddr::new(bare, address.port()));
    }
    if addresses.is_empty() {
        return Err(Box::new(MediaError::Resolve));
    }
    Ok(addresses)
}

/// 取一个平台签发的 URL（上游 `fetchBytes`）。
async fn fetch_bytes(
    client: &reqwest::Client,
    raw_url: &str,
    timeout: Duration,
) -> Result<Fetched, MediaError> {
    let parsed = Url::parse(raw_url).map_err(|_| MediaError::InvalidUrl)?;
    guard_download_url(&parsed)?;
    let response = tokio::time::timeout(timeout, client.get(parsed).send())
        .await
        .map_err(|_| MediaError::Transport)?
        .map_err(|error| classify_connect_error(&error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(MediaError::Http {
            status: status.as_u16(),
        });
    }
    let mut response = response;
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| MediaError::Read)? {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_INBOUND_IMAGE_BYTES {
            return Err(MediaError::TooLarge);
        }
    }
    match sniff_image_content_type(&data) {
        Some(content_type) => Ok(Fetched { data, content_type }),
        None => Err(MediaError::DisallowedContentType {
            content_type: "unrecognized".to_string(),
        }),
    }
}

// =====================================================================
// 解析器
// =====================================================================

/// `DingTalk` 媒体解析端口的上游实现（上游 `mediaResolver`）。
pub struct DingTalkMediaResolver {
    decrypt: crate::dingtalk::Decrypter,
    storage: Arc<dyn MediaStorage>,
    ledger: Arc<dyn MediaIntentLedger>,
    fetch: Arc<dyn MediaFetcher>,
}

impl std::fmt::Debug for DingTalkMediaResolver {
    /// 端口是 trait 对象 ⇒ 只列存在性（解密器自己也脱敏）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkMediaResolver")
            .field("decrypt", &self.decrypt)
            .field("storage", &"<dyn MediaStorage>")
            .field("ledger", &"<dyn MediaIntentLedger>")
            .field("fetch", &"<dyn MediaFetcher>")
            .finish()
    }
}

impl DingTalkMediaResolver {
    /// 装配（存储与意图账本都是必需的）。
    #[must_use]
    pub fn new(
        decrypt: crate::dingtalk::Decrypter,
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
        // 账本是 `async` 端口，而本函数是同步的：这里做一次**短路**的 `block_on`
        // （future 自己持有 `Arc`，线程上的 future 必须是 `'static`）。
        let future = async move { ledger.record_pending_media_object(params).await };
        block_on_engine(future).map_err(|_| MediaError::Ledger)
    }
}

impl MediaResolver for DingTalkMediaResolver {
    /// **纯解码**（无 I/O）：只认译好的载荷里**有媒体**。
    fn has_media(&self, message: &InboundMessage) -> bool {
        decode_dingtalk_raw(message).is_ok_and(|raw| !raw.media.is_empty())
    }

    /// 逐张下载 + 上传，返回回填了 `media_refs` 的消息副本。
    ///
    /// 一张失败**不影响**其余（上游注释逐字）；超过 [`MAX_IMAGES_PER_MESSAGE`] 张 ⇒ 整条
    /// 跳过（`DingTalk` 没有平台侧的图片上限，这个界是本 adapter 自己的内存预算）。
    fn resolve_media(
        &self,
        installation: &ResolvedInstallation,
        _sender: &ResolvedIdentity,
        _session_id: Id,
        chat_message_id: Option<Id>,
        message: &InboundMessage,
    ) -> InboundMessage {
        let mut resolved = message.clone();
        let Ok(raw) = decode_dingtalk_raw(message) else {
            return resolved;
        };
        if raw.media.is_empty() {
            return resolved;
        }
        if raw.media.len() > MAX_IMAGES_PER_MESSAGE {
            tracing::warn!(
                message_id = message.message_id,
                count = raw.media.len(),
                limit = MAX_IMAGES_PER_MESSAGE,
                "dingtalk media resolve skipped: too many images"
            );
            return resolved;
        }
        let Some(chat_message_id) = chat_message_id else {
            tracing::warn!(
                message_id = message.message_id,
                "dingtalk media resolve skipped: no chat message row"
            );
            return resolved;
        };
        let Some(row) = installation_row(installation) else {
            tracing::warn!(
                message_id = message.message_id,
                "dingtalk media resolve skipped: installation platform row unavailable"
            );
            return resolved;
        };
        let Ok(credentials) =
            crate::dingtalk::outbound::decode_credentials(&row.config, &self.decrypt)
        else {
            tracing::warn!(
                message_id = message.message_id,
                "dingtalk media resolve skipped: decode credentials failed"
            );
            return resolved;
        };

        for (index, resource) in raw.media.iter().enumerate() {
            match self.ingest_one(installation, chat_message_id, index, resource, &credentials) {
                Ok(reference) => resolved.media_refs.push(reference),
                Err(error) => {
                    // 尽力而为：这一条失败留下意图行（或什么都没有），对账器事后收尾。
                    tracing::warn!(
                        message_id = message.message_id,
                        image = index,
                        error = %error,
                        "dingtalk media ingest failed"
                    );
                }
            }
        }
        resolved
    }
}

impl DingTalkMediaResolver {
    /// 单张图：下载码 → `MediaRef`（上游 `ResolveMedia` 循环体）。
    ///
    /// 意图行**先落**：从那一刻起再怎么失败都留下一条意图，本函数**不**删任何东西。
    fn ingest_one(
        &self,
        installation: &ResolvedInstallation,
        chat_message_id: Id,
        index: usize,
        resource: &DingtalkMediaResource,
        credentials: &Credentials,
    ) -> Result<MediaRef, MediaError> {
        let key = media_object_key(installation, chat_message_id, resource, index);
        let link = self.storage.object_url(&key);
        if !self.record_intent(installation, chat_message_id, &key, &link)? {
            // key 已经归对账器 ⇒ 别复活那一行、更别上传。
            return Err(MediaError::Ledger);
        }
        let fetched = self.fetch.fetch_resource(credentials, resource)?;
        let extension = image_extension(fetched.content_type).ok_or_else(|| {
            MediaError::DisallowedContentType {
                content_type: fetched.content_type.to_string(),
            }
        })?;
        let filename = format!("dingtalk-image-{}{extension}", index + 1);
        self.storage
            .upload(&key, &fetched.data, fetched.content_type, &filename)
            .map_err(|_| MediaError::Storage)?;
        Ok(MediaRef {
            message_kind: MessageKind::Image,
            storage_key: key,
            storage_url: link,
            filename,
            mime_type: fetched.content_type.to_string(),
            size_bytes: i64::try_from(fetched.data.len()).unwrap_or(i64::MAX),
            inline_placeholder: IMAGE_PLACEHOLDER.to_string(),
            inline_index: resource.inline_index,
        })
    }
}

/// 对象 key（上游 `dingtalkMediaObjectKey`，逐字：`workspaces/<ws>/dingtalk/<inst>/<sha256>`）。
#[must_use]
pub fn media_object_key(
    installation: &ResolvedInstallation,
    chat_message_id: Id,
    resource: &DingtalkMediaResource,
    index: usize,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(chat_message_id.to_string().as_bytes());
    hasher.update([0u8]);
    hasher.update(resource.reference.as_bytes());
    hasher.update([0u8]);
    hasher.update(index.to_string().as_bytes());
    let digest = hasher.finalize();
    format!(
        "workspaces/{}/dingtalk/{}/{}",
        installation.workspace_id,
        installation.id,
        hex::encode(digest)
    )
}

/// 把 `reqwest` 的连接失败还原成我们**自己**的失败类别。
///
/// `reqwest` 会把解析器（[`PublicOnlyResolver`]）的错误包进它的 `Error` 源链 ⇒ 这里沿
/// `source()` 找一次 [`MediaError`]，好让"非公网目标"在**上层**仍然是 `BlockedAddress`
/// 而不是笼统的传输失败（诊断与用例都要这一条）。找不到 ⇒ [`MediaError::Transport`]
/// （**不带 URL**）。
fn classify_connect_error(error: &reqwest::Error) -> MediaError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = error.source();
    while let Some(current) = source {
        if let Some(media) = current.downcast_ref::<MediaError>() {
            return media.clone();
        }
        source = current.source();
    }
    MediaError::Transport
}

/// 在同步上下文里跑一个 engine 的 future（端口是 async、调用点是 sync）。
///
/// 与 [`ThreadedFetcher`] 同款：独立线程 + current-thread 运行时。
fn block_on_engine<F>(future: F) -> EngineResult<bool>
where
    F: std::future::Future<Output = EngineResult<bool>> + Send + 'static,
{
    let failed = || EngineError::infra("dingtalk media: blocking executor unavailable");
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
            .map_err(|_| failed())?
    })
}

#[cfg(test)]
mod tests;
