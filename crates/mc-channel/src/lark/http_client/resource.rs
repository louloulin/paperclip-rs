//! lark **资源响应面**：消息资源下载 + 资源响应的解码
//! （上游 `internal/integrations/lark/http_client.go` 的 `downloadMessageResource*` 一族）。
//!
//! - **写者**：M7-10（`docs/60-M7-PLAN.md` §3.3；本片是 stage 5 的第一片，**0 路由**）。
//! - **为什么是独立文件**：`http_client.rs` 单文件首版 **978 行** > 门 ⑩ 的 800 硬限 ⇒ 按
//!   「传输核心（父模块）/ 端点面（[`super::api`]）/ 资源响应面（本文件）」切开。资源面
//!   **自带**响应体的形态判定与字节上限，和普通 JSON 端点不是一回事，边界取在
//!   `download_once` 的入口。
//! - **上游的四条形态，逐条照搬**：
//!   1. `Content-Length` 说得清就先拒（> [`MAX_MESSAGE_RESOURCE_BYTES`]）；
//!   2. 非 2xx 也要**读体**并按平台码分流（与 [`super::refusal_or_http`] 同一条判据）;
//!   3. `Content-Type` 含 `json` ⇒ **读完**再判是不是平台错误信封；不是错误的 JSON 体
//!      则当成资源字节（平台确实会用 JSON 体表达一部分失败）；
//!   4. 其余走**流式**体，带上限由消费侧的 [`ResourceBody::read_all_capped`] 兜。
//! - **本文件不含** `ApiClient` 的 trait impl：`#[async_trait]` 展开后同一 trait 的两个 impl 块
//!   会撞 `E0119` ⇒ **每个文件只留一个** `impl ApiClient for HttpApiClient`（那个在
//!   [`super::api`] 里，含两个下载端点）。本文件以**固有 impl** 交出一个 `download_once`
//!   （`pub(super)`），供那两个端点复用。
//! - **一处形态差异（登记 `docs/32` §25 的 D 项）**：流式版本与上游一样存在，但本仓把它放进
//!   `ApiClient` trait（上游只在具体类型上）—— 媒体面（M7-12）必须隔着 `dyn ApiClient` 用它，
//!   而 `http_client.rs`/`api.rs` 是**单写者**文件、后续片不得再改。

use super::{refusal_or_http, HttpApiClient};
use crate::lark::client::{parse_lark_error_body, ApiError};

/// 飞书对单条消息资源的字节上限（上游 `maxMessageResourceBytes`，100 MiB）。
pub const MAX_MESSAGE_RESOURCE_BYTES: usize = 100 * 1024 * 1024;

// =====================================================================
// 资源响应的形态（从 `types.rs` 搬来：它们是这一面的解码结果）
// =====================================================================

/// 一次**缓冲**资源下载的结果（上游 `DownloadedResource`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DownloadedResource {
    /// 资源字节。
    pub data: Vec<u8>,
    /// `Content-Type`；响应没给时是 `application/octet-stream`。
    pub content_type: String,
    /// `Content-Disposition` 里的文件名（没有就是 `None`）。
    pub filename: Option<String>,
    /// 字节数（`Content-Length` 说得清就用它，否则回落到实读长度）。
    pub size_bytes: i64,
}

/// 一次**流式**资源下载的响应（上游 `DownloadedResourceStream` 去掉 `io.ReadCloser` 的部分）。
///
/// 本仓的 `mc-channel` 依赖面是**冻结**的（`docs/60` §3.1：无 `tokio-util`、无 `bytes`），
/// 所以不暴露 `AsyncRead`，而是用**拉取式**的 [`ResourceBody`]：调用方按 chunk 消费、
/// 带上限截断。媒体面（M7-12）据此把大传输留在 ACK 路径之外 —— 与上游
/// `io.ReadCloser` 的用途一致，只是把读接口换成了本仓依赖集里能表达的那一种。
#[derive(Debug)]
pub struct DownloadedResourceStream {
    /// 响应体（拉取式）。
    pub body: ResourceBody,
    /// `Content-Type`。
    pub content_type: String,
    /// `Content-Disposition` 里的文件名（没有就是 `None`）。
    pub filename: Option<String>,
    /// `Content-Length`（说不清时是 `0`，与上游 `SizeBytes` 同款）。
    pub size_bytes: i64,
}

/// 拉取式的响应体（本仓对上游 `io.ReadCloser` 的等价物，见 [`DownloadedResourceStream`]）。
///
/// 两形态：**已发出**的 `reqwest` 响应（非 JSON 的资源路径），或**已读进内存**的字节
/// （`Content-Type` 含 `json` 的那条分支 —— 上游也先把那里读完再判是不是平台错误体）。
/// 本类型只做两件事：按 chunk 取字节、按上限拒绝超限。
#[derive(Debug)]
pub struct ResourceBody {
    inner: ResourceBodyKind,
    read_bytes: u64,
}

/// [`ResourceBody`] 的两形态。
#[derive(Debug)]
enum ResourceBodyKind {
    /// 还在链路上的响应。
    Response(reqwest::Response),
    /// 已经读完的字节（一次性给出，再读就是 `None`）。
    Buffered(Option<Vec<u8>>),
}

impl ResourceBody {
    /// 包一个已发出的响应。
    pub(crate) fn new(response: reqwest::Response) -> Self {
        Self {
            inner: ResourceBodyKind::Response(response),
            read_bytes: 0,
        }
    }

    /// 包一段已经读完的字节。
    pub(crate) fn buffered(bytes: Vec<u8>) -> Self {
        Self {
            inner: ResourceBodyKind::Buffered(Some(bytes)),
            read_bytes: 0,
        }
    }

    /// 取下一块；`None` = 读完。
    ///
    /// # Errors
    ///
    /// 链路层失败（连接中断 / 响应体读不出来）⇒ [`ApiError::Transport`]。
    pub async fn read_chunk(&mut self, op: &'static str) -> Result<Option<Vec<u8>>, ApiError> {
        match &mut self.inner {
            ResourceBodyKind::Buffered(slot) => {
                let Some(bytes) = slot.take() else {
                    return Ok(None);
                };
                self.read_bytes = self.read_bytes.saturating_add(bytes.len() as u64);
                Ok(Some(bytes))
            }
            ResourceBodyKind::Response(response) => match response.chunk().await {
                Ok(Some(chunk)) => {
                    let bytes = chunk.to_vec();
                    self.read_bytes = self.read_bytes.saturating_add(bytes.len() as u64);
                    Ok(Some(bytes))
                }
                Ok(None) => Ok(None),
                Err(_) => Err(ApiError::Transport { op }),
            },
        }
    }

    /// 读到上限为止；超过 `cap` 立即失败并**丢弃**已读内容。
    ///
    /// # Errors
    ///
    /// 链路层失败，或总量超过 `cap`（[`ApiError::ResourceTooLarge`]）。
    pub async fn read_all_capped(
        &mut self,
        op: &'static str,
        cap: usize,
    ) -> Result<Vec<u8>, ApiError> {
        let mut buffer = Vec::new();
        while let Some(chunk) = self.read_chunk(op).await? {
            if buffer.len().saturating_add(chunk.len()) > cap {
                return Err(ApiError::ResourceTooLarge { op, cap });
            }
            buffer.extend_from_slice(&chunk);
        }
        Ok(buffer)
    }

    /// 本类型自身已经读出的字节数（诊断用；不是 `Content-Length`）。
    #[must_use]
    pub fn bytes_read(&self) -> u64 {
        self.read_bytes
    }
}

/// 从 `Content-Disposition` 头里取文件名；缺失或解析不了就是 `None`。
///
/// `mime` 不属于 `mc-channel` 的依赖面（冻结）⇒ 只做**受限**解析：取 `filename=` 参数，
/// 剥掉两侧引号。`filename*=UTF-8''…`（RFC 5987）**不**处理 —— 上游用 `mime.ParseMediaType`，
/// 它也只回 `filename` 那一个参数。
#[must_use]
fn filename_from_content_disposition(raw: &str) -> Option<String> {
    let lowered = raw.to_ascii_lowercase();
    let start = lowered.find("filename=")? + "filename=".len();
    let value = raw.get(start..)?;
    let value = value.split(';').next().unwrap_or(value).trim();
    let unquoted = value.trim_matches('"');
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

/// 资源响应里的**头信息**（上游 `downloadMessageResourceStreamOnce` 的判定面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceHeaders {
    /// `Content-Type` 的主类型（已剥掉 `; charset=…`）；缺失时是 `application/octet-stream`。
    pub content_type: String,
    /// `Content-Type` 里含 `json` —— 平台把一部分失败也表达成 JSON 体，上游据此**读完**再判。
    pub is_json: bool,
    /// `Content-Disposition` 里的文件名。
    pub filename: Option<String>,
    /// `Content-Length`（说不清时是 `0`）。
    pub size_bytes: i64,
}

/// 从一条资源响应里取头信息（上游 `downloadMessageResourceStreamOnce` 的那一半）。
///
/// 只做**形态判定**；是不是平台错误体由调用方拿 [`super::client::parse_lark_error_body`] 判。
pub(crate) fn resource_headers(response: &reqwest::Response) -> ResourceHeaders {
    let raw_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let main_type = raw_type
        .split(';')
        .next()
        .unwrap_or(raw_type)
        .trim()
        .to_string();
    let raw_disposition = response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    ResourceHeaders {
        is_json: main_type.to_ascii_lowercase().contains("json"),
        content_type: if main_type.is_empty() {
            "application/octet-stream".to_string()
        } else {
            main_type
        },
        filename: filename_from_content_disposition(raw_disposition),
        size_bytes: i64::try_from(response.content_length().unwrap_or(0)).unwrap_or(i64::MAX),
    }
}

impl HttpApiClient {
    /// 一次资源下载的尝试（不发令牌、不重试；重试在调用方）。
    pub(super) async fn download_once(
        &self,
        base: &str,
        path: &str,
        token: &str,
        op: &'static str,
    ) -> Result<DownloadedResourceStream, ApiError> {
        let response = self
            .config
            .http
            .get(format!("{base}{path}"))
            .bearer_auth(token)
            .timeout(self.config.resource_download_timeout)
            .send()
            .await
            .map_err(|_| ApiError::Transport { op })?;
        let status = response.status().as_u16();
        // Content-Length 说得清的先拒掉（说不清的那部分由消费侧的 `read_all_capped` 兜）。
        if response
            .content_length()
            .is_some_and(|length| length > MAX_MESSAGE_RESOURCE_BYTES as u64)
        {
            return Err(ApiError::ResourceTooLarge {
                op,
                cap: MAX_MESSAGE_RESOURCE_BYTES,
            });
        }
        if !(200..300).contains(&status) {
            let raw = response
                .bytes()
                .await
                .map_err(|_| ApiError::Transport { op })?;
            return Err(refusal_or_http(status, &raw, op));
        }
        let headers = resource_headers(&response);
        if headers.is_json {
            // 平台把一部分失败也表达成 `Content-Type: application/json` 的体 ⇒ 先读完、
            // 判是不是错误信封；不是错误的 JSON 体则当成资源字节返回（上游同款）。
            let raw = response
                .bytes()
                .await
                .map_err(|_| ApiError::Transport { op })?;
            if let Some(code) = parse_lark_error_body(&raw).filter(|code| *code != 0) {
                return Err(ApiError::Refused {
                    op,
                    status: None,
                    code,
                });
            }
            let size_bytes = i64::try_from(raw.len()).unwrap_or(i64::MAX);
            return Ok(DownloadedResourceStream {
                body: ResourceBody::buffered(raw.to_vec()),
                content_type: headers.content_type,
                filename: headers.filename,
                size_bytes,
            });
        }
        Ok(DownloadedResourceStream {
            body: ResourceBody::new(response),
            content_type: headers.content_type,
            filename: headers.filename,
            size_bytes: headers.size_bytes,
        })
    }
}

#[cfg(test)]
mod tests;
