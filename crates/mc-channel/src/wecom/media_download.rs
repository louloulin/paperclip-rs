//! `media_download.go`（459 行）的本地落点：**取回一个回调所指的字节**。
//!
//! - **写者**：M7-18（`LUM-1783` / `docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §35 的 D1）。
//! - **上游定位**（文件头逐字）：URL 是一个预签名的腾讯 COS 地址 —— 没有 `access_token`、
//!   没有头、五分钟有效。这让取回本身很平凡，而把所有的心思都推到别处去：body 是**加密**到达的、
//!   它的大小**不在回调里声明**，而"这个文件是什么"的唯一描述来自**响应头**。
//!
//! # 两条取回路径（上限是同一个数，界的东西不同）
//!
//! | 路径 | 形态 | 上限界住什么 |
//! | --- | --- | --- |
//! | [`download_media`] | 整份装进内存（CBC 要先看到尾巴才能相信头） | **堆**：下载并发 × 100 MiB |
//! | [`open_media`] | 流（[`MediaBody`]，交给 [`super::media_stream`]） | **磁盘**：同上，但只占一个缓冲区 |
//!
//! 缓冲路径是**回退**：实现了 `UploadStream` 的存储后端（两个已发布实现都有）走流式那条。
//!
//! # 凭据面（上游 `stripURL` 的本地形态，登记 `docs/32` §35 的 D7）
//!
//! `net/http` 把每一次失败都包进一个 `*url.Error`，而它的 `Error()` **打印它取的那个 URL**
//! —— 这里的 URL 是一个预签名的 COS 链接：一张五分钟内对**任何持有者**生效的、指向同事私有
//! 附件的 bearer 凭据。一次 DNS 抖动、一次 TCP reset、一次 TLS 错误或那次下载超时，都会把
//! 它写进应用日志，再从那里进到任何转运这些日志的地方去。上游事后用 `stripURL` 把它剥掉；
//! **本仓在类型层面就进不来**：`reqwest::Error` 的 `Display` **同样**带着 URL，所以本文件的每个
//! 传输失败都先经过 [`drop_transport_error`]，而 [`MediaDownloadError`] 的变体**没有一个**带
//! 载荷。HTTP 状态码那一条**刻意不带响应体片段**（上游带 512 字节用于诊断）：COS 的 XML 错误体
//! 里会回显它收到的那个 URL。
//!
//! # 与上游的三点形态差异（同一节登记）
//!
//! 1. **`context.Context` → `reqwest` 的每请求超时**：上游用 ctx 表达"整次取回 30s"；
//!    本仓在请求上挂 `.timeout(MEDIA_DOWNLOAD_TIMEOUT)`（`open_media` 那条**刻意不挂** ——
//!    上游也是：调用方在整个解密的长度上读这个 body，而一个盖住取回的截止时刻会把读切断）。
//! 2. **`DialContext` → `reqwest` 的 `dns_resolver`**（见 [`super::media_guard`] 的模块文档）；
//!    `reqwest` 对 **IP 字面量** host 不调解析器 ⇒ 那一步由 [`check_media_url`] 自己兜。
//! 3. **`mime.ParseMediaType` → 本仓手写的解析器**（[`filename`] 子模块）：`mime` crate 不在
//!    `mc-channel` 的依赖边里（M7-0 冻结）。

use async_trait::async_trait;
use reqwest::header::CONTENT_DISPOSITION;

use super::media_guard::MediaGuardError;

pub mod filename;

pub use filename::{
    clean_media_filename, decode_form_encoded_filename, has_extended_filename,
    media_filename_from_disposition, same_form_encoding, strip_control_runes,
};

/// 一个下载体的上限（上游 `maxMediaBytes`）。
///
/// `WeCom` 把智能机器人的文件与视频压在 100 MB，图片没有写帽，所以这就是**一切**的上限：
/// 比它更大到达的东西，不是回调本该交给我们的。
///
/// 在这条路径上 body 是**整份缓冲**的（CBC 解密要先看到尾巴才能相信头）⇒ 这个上限同时也是
/// **每次下载的内存**上界，再乘以路由的媒体并发。这条路径是回退：实现了 `UploadStream` 的
/// 存储后端走 [`super::media_stream`]，那里同一个上限界的是**磁盘**。
pub const MEDIA_MAX_BYTES: i64 = 100 << 20;

/// 单次取回的上限（上游 `mediaDownloadTimeout` 30s）。
///
/// 路由本来就把媒体解析跑在一个 45s 的预算里，那个预算由同一条消息上的每个附件、
/// 以及排在它前面的任何东西共同花掉 ⇒ 一个慢对象不能把整个预算吃掉。
pub const MEDIA_DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 取回失败。
///
/// `MediaTooLarge` 是**唯一**被上层按名字分类的那一个（上游同名的哨兵：调用方据此告诉用户
/// "文件太大了"而不是"出了点问题"）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaDownloadError {
    /// 上游 `errMediaTooLarge`：body 越过了 [`MEDIA_MAX_BYTES`]，无论是 `Content-Length` 声明的
    /// 还是读到一半发现的。
    #[error("wecom: media exceeds the {MEDIA_MAX_BYTES} byte limit")]
    TooLarge,
    /// URL 空 / 解析不了 / 没有 host（上游三条 `errors.New`）。
    ///
    /// **不回显输入**：`url.Parse` 的错误文本会引用整条输入，而输入在这里是一条活的预签名链接。
    #[error("wecom: media url is not fetchable")]
    InvalidUrl,
    /// 不是 `http` / `https`。
    #[error("wecom: media url scheme is not fetchable")]
    UnsupportedScheme,
    /// 没有可用的**带闸**客户端（上游：`hc == nil` 是一个接线 bug，而顺手拿 `http.DefaultClient`
    /// 正是 `media_guard` 存在的理由所要防的那次**无闸**取回）。
    #[error("wecom: media download: no guarded http client configured")]
    NoClient,
    /// 地址被闸拒了（上游 `ErrMediaAddrBlocked`）——**与传输失败刻意分开**。
    #[error(transparent)]
    Guard(#[from] MediaGuardError),
    /// 非 2xx（上游带状态码 + 状态文本 + 响应体片段；本仓只带状态码，见模块文档的凭据面）。
    #[error("wecom: media download: http {status}")]
    Http { status: u16 },
    /// 传输 / TLS / 超时失败（上游经 `stripURL` 之后的那一半）。
    #[error("wecom: media download failed")]
    Transport,
    /// 读响应体失败。
    #[error("wecom: media download: read body failed")]
    Read,
}

/// 响应关于这个文件说了什么。**两条取回路径都返回它**，于是缓冲与流式摄入学到的是同一件事。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaHeaders {
    /// 从 `Content-Disposition` 里解出来的展示名，或者空。
    ///
    /// 回调体自己**不**带名字、大小或 MIME ⇒ 这个头是原始名字唯一存在的地方。
    pub filename: String,
    /// `Content-Disposition` 到达时的原样值，只留给追踪。
    ///
    /// COS 在那个头里放的东西是这个包**唯一**无法在本地核对的一件事 —— 它来自的 URL 五分钟后
    /// 就失效了，所以一个看起来不对的名字**没法重新取回来核对**，必须在当时被记下来。
    pub disposition: String,
}

/// 一个取回来的 body 加上响应关于它说的话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedMedia {
    /// 原始响应体 —— **仍然是加密的**；[`super::media_crypt`] 把它变成文件。
    pub body: Vec<u8>,
    pub headers: MediaHeaders,
}

/// 上游 `downloadMedia`：取一个媒体 URL（上限用默认值）。
///
/// # Errors
///
/// 见 [`MediaDownloadError`]；错误里带着原因（链接过期、body 超帽、服务端卡住），
/// 因为调用方要把它们变成人读得懂的话。
pub async fn download_media(
    client: &reqwest::Client,
    raw_url: &str,
) -> Result<DownloadedMedia, MediaDownloadError> {
    download_media_capped(client, raw_url, MEDIA_MAX_BYTES).await
}

/// [`download_media`] 把上限做成参数的版本；**只有用例**传的不是 [`MEDIA_MAX_BYTES`]。
///
/// 读到一半发现 body 太大，在任何上限下都是同一段代码，而在真上限上证明它意味着在
/// race detector 底下缓冲 100 MB。
///
/// # Errors
///
/// 见 [`MediaDownloadError`]。
pub async fn download_media_capped(
    client: &reqwest::Client,
    raw_url: &str,
    limit: i64,
) -> Result<DownloadedMedia, MediaDownloadError> {
    check_media_url(raw_url)?;
    let response = send(client, raw_url, Some(MEDIA_DOWNLOAD_TIMEOUT)).await?;
    if response
        .content_length()
        .is_some_and(|declared| declared > u64::try_from(limit).unwrap_or(u64::MAX))
    {
        return Err(MediaDownloadError::TooLarge);
    }
    let status = response.status();
    if !status.is_success() {
        return Err(MediaDownloadError::Http {
            status: status.as_u16(),
        });
    }
    let mut body = MediaBody::new(response, limit);
    let mut buffer = Vec::new();
    while let Some(item) = body.next_chunk().await {
        buffer.extend_from_slice(&item?);
    }
    Ok(DownloadedMedia {
        body: buffer,
        headers: MediaHeaders {
            filename: media_filename_from_disposition(body.disposition().unwrap_or_default()),
            disposition: body.disposition().unwrap_or_default().to_owned(),
        },
    })
}

/// 上游 `openMedia`：不带缓冲的 [`download_media`] —— 把 body 作为一个流交出去，让调用方
/// 边到达边解密，外加缓冲路径读到的同一份 [`MediaHeaders`]。
///
/// 同一道闸、同一套状态处理、同一个上限 —— 上限由调用方读的那个流自己执行
/// （[`MediaBody`] 计数），所以一个对自己的长度撒谎的 body 仍然不能把进程带走。
///
/// 调用方读这个流直到结束；它拥有底下的响应。
///
/// # Errors
///
/// 见 [`MediaDownloadError`]。
pub async fn open_media(
    client: &reqwest::Client,
    raw_url: &str,
) -> Result<(MediaBody, MediaHeaders), MediaDownloadError> {
    check_media_url(raw_url)?;
    // 这里**没有**取回超时，与 `download_media` 不同（上游逐字）：调用方在整个解密的长度上读
    // 这个 body，而一个盖住取回的截止时刻会把读切断。路由自己的媒体预算界住整个操作。
    let response = send(client, raw_url, None).await?;
    if response
        .content_length()
        .is_some_and(|declared| declared > u64::try_from(MEDIA_MAX_BYTES).unwrap_or(u64::MAX))
    {
        return Err(MediaDownloadError::TooLarge);
    }
    let status = response.status();
    if !status.is_success() {
        return Err(MediaDownloadError::Http {
            status: status.as_u16(),
        });
    }
    let headers = MediaHeaders {
        filename: media_filename_from_disposition(
            response
                .headers()
                .get(CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default(),
        ),
        disposition: response
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    };
    Ok((MediaBody::new(response, MEDIA_MAX_BYTES), headers))
}

/// 发一次请求（共用的一段：超时、以及把 `reqwest` 的错误扔成**不带 URL** 的变体）。
async fn send(
    client: &reqwest::Client,
    raw_url: &str,
    timeout: Option<std::time::Duration>,
) -> Result<reqwest::Response, MediaDownloadError> {
    let mut request = client.get(raw_url);
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    request
        .send()
        .await
        .map_err(|error| drop_transport_error(&error))
}

/// 上游 `stripURL` 的本地形态（见模块文档的凭据面）。
///
/// 上游是把 `*url.Error` 剥成它的 `Err` 字段；本仓连那一步都不需要 —— `reqwest::Error` 直接
/// 被丢掉，因为它的 `Display` 同样会打印那条预签名 URL。
fn drop_transport_error(error: &reqwest::Error) -> MediaDownloadError {
    // 只看**种类**，绝不把它的 `Display` 拼进我们自己的消息（那里面有一条预签名 URL）。
    if error.is_body() {
        return MediaDownloadError::Read;
    }
    MediaDownloadError::Transport
}

/// 一次取回的**分块来源**：缓冲路径与流式解密都从这里拿字节。
///
/// 与上游的差别只有一个：上游直接拿到一个 `io.Reader`（Go 的响应体天生同步），而 `reqwest`
/// 的 body 只能**拉**（`Response::chunk`）—— 而本仓的 `mc-channel` 依赖面里 `reqwest` 没开
/// `stream` feature（M7-0 冻结）⇒ 用不了 `bytes_stream()`。所以这里是一个**异步的分块拉取**
/// 接缝，语义与 `cappedBody.Read` 逐条相同：上限在这一个地方执行。
#[async_trait]
pub trait MediaChunkSource: Send {
    /// 下一段密文；`None` = body 结束。
    ///
    /// # Errors
    ///
    /// 见 [`MediaDownloadError`]（`TooLarge` 也是从这里出来的）。
    async fn next_chunk(&mut self) -> Option<Result<Vec<u8>, MediaDownloadError>>;
}

/// 上游 `cappedBody` 的本地形态：`reqwest` 的响应体 + 一个字节计数器。
///
/// 作用与上游一样：**一个不声明长度的 body 不能拿它以前能拿来填满堆的方式填满磁盘**。
/// 计数留**一个字节的余量**，与 `downloadMedia` 一样 —— 读满上限与"还有更多要来"才能被区分开。
pub struct MediaBody {
    inner: reqwest::Response,
    remaining: i64,
    disposition: String,
}

impl std::fmt::Debug for MediaBody {
    /// 手写：`reqwest::Response` 的 `Debug` 会把**请求 URL**（一条预签名凭据）打出来。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaBody")
            .field("remaining", &self.remaining)
            .field("content_disposition", &self.disposition)
            .finish_non_exhaustive()
    }
}

impl MediaBody {
    fn new(response: reqwest::Response, limit: i64) -> Self {
        let disposition = response
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        Self {
            inner: response,
            // 一个字节的余量，与 `downloadMedia` 一样。
            remaining: limit.saturating_add(1),
            disposition,
        }
    }

    /// 响应到达时的 `Content-Disposition`（空串 = 服务端没发 ⇒ `None`）。
    #[must_use]
    pub fn disposition(&self) -> Option<&str> {
        if self.disposition.is_empty() {
            None
        } else {
            Some(&self.disposition)
        }
    }
}

#[async_trait]
impl MediaChunkSource for MediaBody {
    async fn next_chunk(&mut self) -> Option<Result<Vec<u8>, MediaDownloadError>> {
        if self.remaining <= 0 {
            return Some(Err(MediaDownloadError::TooLarge));
        }
        match self.inner.chunk().await {
            Ok(None) => None,
            Err(_) => Some(Err(MediaDownloadError::Read)),
            Ok(Some(chunk)) => {
                let length = i64::try_from(chunk.len()).unwrap_or(i64::MAX);
                if length > self.remaining {
                    self.remaining = 0;
                    return Some(Err(MediaDownloadError::TooLarge));
                }
                self.remaining -= length;
                if self.remaining == 0 {
                    // 读满那一个字节的余量 ⇒ body 比帽还大（恰好超出 1 字节也算超出）。
                    return Some(Err(MediaDownloadError::TooLarge));
                }
                Some(Ok(chunk.to_vec()))
            }
        }
    }
}

/// 上游 `checkMediaURL`：拒掉任何不该把传输指向的地方。
///
/// URL 是**从已认证的 socket 上**过来的，所以这是一道护栏而不是一道防御 —— 但它是一个
/// 从外面来的、命名了一个我们随后要去 GET 的主机的字符串。
///
/// # Errors
///
/// [`MediaDownloadError::InvalidUrl`] / [`MediaDownloadError::UnsupportedScheme`] /
/// [`MediaDownloadError::Guard`]（host 是**非公网**的 IP 字面量；`reqwest` 对字面量不调解析器
/// ⇒ 那一步只能自己做，见模块文档的差异 2）。
pub fn check_media_url(raw_url: &str) -> Result<(), MediaDownloadError> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return Err(MediaDownloadError::InvalidUrl);
    }
    let parsed = reqwest::Url::parse(trimmed).map_err(|_| MediaDownloadError::InvalidUrl)?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(MediaDownloadError::UnsupportedScheme);
    }
    let Some(host) = parsed.host_str() else {
        return Err(MediaDownloadError::InvalidUrl);
    };
    if host.is_empty() {
        return Err(MediaDownloadError::InvalidUrl);
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(literal) = bare.parse::<std::net::IpAddr>() {
        if !super::media_guard::public_addr_only(literal) {
            return Err(MediaDownloadError::Guard(MediaGuardError::BlockedAddress));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
