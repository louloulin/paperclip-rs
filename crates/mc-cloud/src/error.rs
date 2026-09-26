//! 出站传输的**错误面**（`mc-cloud` 全部错误都在这里）。
//!
//! # 为什么错误变体**不携带**底层文本
//!
//! 上游 `cloudruntime.Client` 把 `fmt.Errorf("%w: %s", ErrInvalidBaseURL, c.baseURL)` 与
//! `reqwest` 的错误文本都直接抛给 handler。本仓**不照抄**这一步，理由是一条硬约束
//! （`docs/62` §2.4）：**错误路径不得回显任何凭据载体**。
//!
//! - `reqwest::Error` 的 `Display` **内嵌完整 URL**（含 host 与 path）；
//! - 上游的 `ErrInvalidBaseURL` 文本直接拼了 `baseURL`；
//! - 云侧响应体可能含云侧内部标识。
//!
//! ⇒ 本模块的 `Display` **全部是静态字符串**：`CloudError` 的任何渲染结果都不含
//! URL、不含云侧响应体、不含 `Idempotency-Key`。诊断信息走**结构化日志**（`tracing`
//! 的字段），不走错误文本。
//!
//! # 与 `docs/62` §2.6 的映射表的分工
//!
//! §2.6 的「上游情形 → 本地状态码 / code」是**handler 侧**的职责（M9-1 / M9-2 / M9-6
//! 各自映射，因为 `billing` 与 `subscriptions` 的未授权语义不同）。本模块只提供
//! 「哪一类失败」的可判定谓词（[`CloudError::is_disabled`] / [`CloudError::is_timeout`]），
//! **不**替 handler 决定状态码。
//!
//! ⚠️ §2.6 表里的「5xx ⇒ 502」一行在实现上是**透传**而不是 502：上游
//! `cloudruntime.doInner` 对任何 HTTP 状态都返回 `(Response, nil)`，只有**传输层**
//! 失败才是 error（`writeCloudRuntimeResponse` 把云侧的 5xx 原样写回客户端）。
//! 这一处口径订正登记在 `docs/32-M3-DAEMON-FACE.md` §9.13。

/// 出站请求的失败类别。
///
/// 四个变体逐字对应上游 `cloudruntime` 的四个错误源：`ErrDisabled` /
/// `ErrInvalidBaseURL` / `context.DeadlineExceeded` / 其余（连接拒绝、体超限）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CloudError {
    /// `MULTICA_CLOUD_URL` 为空 ⇒ 整个云面禁用（上游 `ErrDisabled`）。
    ///
    /// `docs/62` §2.6 的落点是 **403 `cloud_runtime_not_configured`**（由 handler 写）。
    #[error("multica-cloud URL is not configured")]
    Disabled,
    /// `MULTICA_CLOUD_URL` 存在但不是合法的绝对 URL（上游 `ErrInvalidBaseURL`）。
    ///
    /// §2.6 的落点是 **500 `cloud_runtime_misconfigured`**。⚠️ **不回显**原值
    /// —— 上游的文本里拼了 `baseURL`，本仓刻意不拼。
    #[error("multica-cloud URL is invalid")]
    InvalidBaseUrl,
    /// 出站路径不以 `/` 开头（上游 `doInner` 的第一道断言）。
    ///
    /// 这是**开发者错误**（不是运行期配置错误）：四条代理路由的路径都是常量字面量。
    #[error("cloud runtime path must start with /")]
    InvalidPath,
    /// 超时（上游 `context.DeadlineExceeded`）。§2.6 的落点是 **504**。
    #[error("cloud runtime request timed out")]
    Timeout,
    /// 其余传输失败（连接拒绝 / TLS / 读体中断）。§2.6 的落点是 **502**。
    ///
    /// **刻意不带** `reqwest::Error` 的文本：它的 `Display` 内嵌出站 URL。
    #[error("cloud runtime request failed")]
    Transport,
    /// 云侧响应体超过上限（上游 `cloud runtime response exceeds %d bytes`）。
    #[error("cloud runtime response exceeds the {limit} byte limit")]
    ResponseTooLarge { limit: usize },
    /// 云侧响应不是合法 JSON（只由 [`crate::transport::Response::json`] 产出）。
    ///
    /// 上游把 JSON 判定留给各 handler（`json.Valid(body)` 决定 `Content-Type`），
    /// 本仓同样不在这里判定 —— 只有**调用方显式要求解析**时才可能看到这个变体。
    #[error("cloud runtime response is not valid JSON")]
    InvalidJson,
    /// 调用方给了 `http` 层无法表示的出站头（非法头名 / 非 ASCII 头值）。
    ///
    /// ⚠️ **本仓新增的第五类**（上游没有对应变体）：Go 侧这个错误由 `net/http` 在**写请求**
    /// 时报出，本仓在**拼头**时报出（提前、且**不静默丢头** —— 静默丢掉
    /// `Stripe-Signature` 会让云侧回一个 401，故障点离病因很远）。
    /// 落点同 §2.6 的「其他」行：**502**。只报**头名**，不报头值（值可能是签名 / 幂等键）。
    #[error("cloud runtime request header is not representable: {name}")]
    InvalidHeader { name: String },
}

impl CloudError {
    /// 是否「URL 未配置」（⇒ §2.6 的 403 `cloud_runtime_not_configured`）。
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// 是否「URL 非法」（⇒ §2.6 的 500 `cloud_runtime_misconfigured`）。
    #[must_use]
    pub fn is_misconfigured(&self) -> bool {
        matches!(self, Self::InvalidBaseUrl)
    }

    /// 是否超时（⇒ §2.6 的 504）。
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }

    /// 指标用的状态桶 —— 与 [`crate::transport::status_bucket`] 的取值域一致
    /// （`{ok,4xx,5xx,timeout,error}`，上游 `requestStatusBucket`）。
    #[must_use]
    pub fn status_bucket(&self) -> &'static str {
        if self.is_timeout() {
            "timeout"
        } else {
            "error"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 凭据面判据 ③（`docs/62` §2.4）：**错误路径不回显**任何 URL / 响应体 / 幂等键。
    ///
    /// 这一条是「静态字符串」设计的回归锁：谁把 `reqwest::Error` 的文本或 base URL
    /// 拼回 `Display`，本用例立刻红。
    #[test]
    fn error_display_never_carries_url_or_secret_material() {
        let all = [
            CloudError::Disabled,
            CloudError::InvalidBaseUrl,
            CloudError::InvalidPath,
            CloudError::Timeout,
            CloudError::Transport,
            CloudError::ResponseTooLarge { limit: 1 << 20 },
            CloudError::InvalidJson,
            CloudError::InvalidHeader {
                name: "idempotency-key".into(),
            },
        ];
        for error in &all {
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains("http://"), "{rendered}");
            assert!(!rendered.contains("https://"), "{rendered}");
            assert!(!rendered.contains("user:pass"), "{rendered}");
            assert!(!rendered.contains("idem-key"), "{rendered}");
            assert!(!rendered.contains("Stripe-Signature"), "{rendered}");
        }
    }

    #[test]
    fn predicates_match_the_four_upstream_sources() {
        assert!(CloudError::Disabled.is_disabled());
        assert!(CloudError::InvalidBaseUrl.is_misconfigured());
        assert!(CloudError::Timeout.is_timeout());
        assert!(!CloudError::Transport.is_timeout());
        assert_eq!(CloudError::Transport.status_bucket(), "error");
        assert_eq!(CloudError::Timeout.status_bucket(), "timeout");
        // 超限只报上限数字，不报体内容。
        assert_eq!(
            CloudError::ResponseTooLarge { limit: 4096 }.to_string(),
            "cloud runtime response exceeds the 4096 byte limit"
        );
    }
}
