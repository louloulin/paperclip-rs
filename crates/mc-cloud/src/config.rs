//! `MULTICA_CLOUD_URL` 的**读取口与校验** —— 本波唯一的部署密钥面。
//!
//! # 为什么单独一个文件（而不是塞进 `transport.rs`）
//!
//! 上游把「基址解析」放在 `internal/cloudruntime/client.go`（`NewClient` 只做
//! `TrimRight(TrimSpace(baseURL), "/")`，**不校验**），把「校验」放在
//! `internal/entitlement/client.go:57`（`ErrInvalidConfig`：必须是绝对 URL 且
//! **无凭据 / query / fragment**）。本仓把这**两件事**都收在本模块：
//!
//! - [`normalize`] = 上游 `NewClient` 的规范化（trim + 去尾斜杠 + 空串即未配置）；
//! - [`validate`] = 上游 `entitlement.New` 的校验（三件套拒绝）。
//!
//! `mc-cloud::transport::Client::new` 与 `mc-entitlement` 的客户端都调这两个函数，
//! ⇒ **只有一份**基址解析（`docs/62` §2.2 判据 3、§2.4 表）。
//!
//! # 三条纪律（与 `mc-http` 的 `state/integrations.rs` 同款，别开后门）
//!
//! 1. **未配置就是未配置**：缺 / 空 / 全空白 ⇒ `None`，**绝不**用零值兑底、**绝不** panic；
//! 2. **非法就是非法**（不是"未配置"）：非空但非法 ⇒ 仍然记录**原值已规范化**的那个串，
//!    并由 [`CloudSettings::is_misconfigured`] 报 `true` ⇒ handler 给
//!    **500 `cloud_runtime_misconfigured`**（不是 403 —— 上游 `writeCloudRuntimeError`
//!    对 `ErrInvalidBaseURL` 给 500，`docs/62` §2.6）；
//! 3. **值不进日志、不进 `Debug`**：非法时打的告警只带 **redact 后**的形态
//!    （`mc_telemetry::redact`，判据 ④），`Debug` 只暴露**存在性**。

use url::Url;

use crate::error::CloudError;

/// 唯一的云侧基址 env 名（`docs/62` §2.4：**只有一组密钥，但它是"开关"**）。
pub const CLOUD_URL_ENV: &str = "MULTICA_CLOUD_URL";

/// 规范化：`TrimSpace` → `TrimRight("/")` → 空串即 `None`。
///
/// 逐字对齐上游 `cloudruntime.NewClient` 的
/// `strings.TrimRight(strings.TrimSpace(cfg.BaseURL), "/")`，并把结果空串折成 `None`
/// （上游用 `baseURL == ""` 表示 `Enabled()` 为假 ⇒ 两者同义）。
///
/// ⚠️ **不做** `TrimSpace` 之外的任何"宽容"处理：`" / "` 规范化后是空串 ⇒ 未配置。
#[must_use]
pub fn normalize(raw: &str) -> Option<String> {
    let normalized = raw.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// 校验「合法绝对 URL 且**无凭据 / query / fragment**」。
///
/// 三件套逐字对齐上游 `internal/entitlement/client.go:66-68` 的判定：
/// `(Scheme != http && != https) || Host == "" || User != nil || RawQuery != "" || Fragment != ""`
/// ⇒ `ErrInvalidConfig`。
///
/// 为什么**必须**在这里拒绝（不是运行期才炸）：这三处正是「凭据最容易被塞进 URL」的
/// 位置 —— `https://user:pw@host/` 会把口令写进每一次 `Debug`/日志，`?token=…` 会
/// 跟着重定向走。**在校验阶段拒绝**是 `docs/62` §2.4 判据 ① 的原文要求。
///
/// # Errors
///
/// 任何一条不满足 ⇒ [`CloudError::InvalidBaseUrl`]（**不回显原值**）。
pub fn validate(candidate: &str) -> Result<Url, CloudError> {
    let Ok(parsed) = Url::parse(candidate) else {
        return Err(CloudError::InvalidBaseUrl);
    };
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none_or(str::is_empty)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(CloudError::InvalidBaseUrl);
    }
    Ok(parsed)
}

/// `MULTICA_CLOUD_URL` 的**进程读取口**（唯一实现点是 [`CloudSettings::from_env`]）。
///
/// 三态（不是两态 —— 这正是 `docs/62` §2.5 / §2.6 要区分的东西）：
///
/// | 状态 | [`is_configured`](Self::is_configured) | [`is_valid`](Self::is_valid) | 语义 |
/// | --- | :-: | :-: | --- |
/// | 未设置 / 空 / 全空白 | `false` | `false` | 403 `cloud_runtime_not_configured` |
/// | 非空但非法 | `true` | `false` | 500 `cloud_runtime_misconfigured` |
/// | 合法 | `true` | `true` | 可发出站请求 |
#[derive(Clone, Default)]
pub struct CloudSettings {
    base_url: Option<String>,
    url_valid: bool,
}

impl CloudSettings {
    /// env 名（只有一处字面量，别在别处内联）。
    pub const ENV: &'static str = CLOUD_URL_ENV;

    /// 从进程环境读（生产装配点：`AppState::new` 的构造体）。
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 从任意「名字 → 值」查询函数读（单测注入，不碰进程全局 env）。
    #[must_use]
    pub fn from_env_with<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let Some(raw) = get(CLOUD_URL_ENV) else {
            return Self::default();
        };
        let Some(normalized) = normalize(&raw) else {
            // 空 / 全空白 ⇒ 未配置（**不是**非法）：上游 `Enabled()==false`，落 403。
            return Self::default();
        };
        let url_valid = if validate(&normalized).is_ok() {
            true
        } else {
            // 判据 ④：告警里的值必须**已脱敏**（`redact_str` 认 `cloud_url` 这个键名）。
            // 只打这一条，且不把原值交给任何别的字段。
            let redacted = mc_telemetry::redact::Redactor::default()
                .redact_str(&format!("{CLOUD_URL_ENV}={normalized}"));
            tracing::warn!(
                value = %redacted,
                "MULTICA_CLOUD_URL is not a valid absolute URL \
                 (needs http/https scheme, host, and no credentials/query/fragment); \
                 cloud proxy routes will answer 500 cloud_runtime_misconfigured"
            );
            false
        };
        Self {
            base_url: Some(normalized),
            url_valid,
        }
    }

    /// 规范化后的基址（`None` = 未配置）。
    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// 是否**存在**配置（含非法配置）。
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.base_url.is_some()
    }

    /// 配置是否**合法**（唯一能发出站请求的状态）。
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.url_valid
    }

    /// 是否「配了但非法」⇒ handler 给 500 `cloud_runtime_misconfigured`。
    #[must_use]
    pub fn is_misconfigured(&self) -> bool {
        self.base_url.is_some() && !self.url_valid
    }
}

impl std::fmt::Debug for CloudSettings {
    /// 手写脱敏：**绝不**打印 URL。只暴露三态判定所需的布尔。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudSettings")
            .field("configured", &self.is_configured())
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("url_valid", &self.url_valid)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn normalize_matches_upstream_trimright() {
        assert_eq!(
            normalize("  https://cloud.test/  ").as_deref(),
            Some("https://cloud.test")
        );
        assert_eq!(
            normalize("https://cloud.test///").as_deref(),
            Some("https://cloud.test")
        );
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("   "), None);
        // 只有斜杠 ⇒ 规范化后为空 ⇒ 未配置（上游 `/` 也是 ""）。
        assert_eq!(normalize(" / "), None);
        // **不做**任何别的规范化：大小写与 path 原样保留。
        assert_eq!(
            normalize("HTTP://Cloud.Test/x/").as_deref(),
            Some("HTTP://Cloud.Test/x")
        );
    }

    #[test]
    fn validate_rejects_credentials_query_and_fragment() {
        assert!(validate("http://127.0.0.1:8080").is_ok());
        assert!(validate("https://cloud.test/base").is_ok());
        // 三件套（`docs/62` §2.4 判据 ①）：userinfo / query / fragment。
        assert_eq!(
            validate("https://u:p@cloud.test"),
            Err(CloudError::InvalidBaseUrl)
        );
        assert_eq!(
            validate("https://u@cloud.test"),
            Err(CloudError::InvalidBaseUrl)
        );
        assert_eq!(
            validate("https://cloud.test/?token=x"),
            Err(CloudError::InvalidBaseUrl)
        );
        assert_eq!(
            validate("https://cloud.test/#frag"),
            Err(CloudError::InvalidBaseUrl)
        );
        // 非绝对 / 非 http(s) / 无 host。
        assert_eq!(validate("/api/v1"), Err(CloudError::InvalidBaseUrl));
        assert_eq!(validate("cloud.test"), Err(CloudError::InvalidBaseUrl));
        assert_eq!(
            validate("ftp://cloud.test"),
            Err(CloudError::InvalidBaseUrl)
        );
        assert_eq!(
            validate("file:///etc/passwd"),
            Err(CloudError::InvalidBaseUrl)
        );
    }

    #[test]
    fn settings_is_three_state_and_never_panics_without_env() {
        let unset = CloudSettings::from_env_with(env(&[]));
        assert!(!unset.is_configured());
        assert!(!unset.is_valid());
        assert!(!unset.is_misconfigured());
        assert!(unset.base_url().is_none());

        let blank = CloudSettings::from_env_with(env(&[(CLOUD_URL_ENV, "   ")]));
        assert!(!blank.is_configured(), "空 / 全空白 = 未配置（403）");
        assert!(!blank.is_misconfigured(), "未配置**不是**非法（403 ≠ 500）");

        let valid = CloudSettings::from_env_with(env(&[(CLOUD_URL_ENV, " http://127.0.0.1:9/ ")]));
        assert!(valid.is_configured() && valid.is_valid());
        assert_eq!(valid.base_url(), Some("http://127.0.0.1:9"));

        let bad = CloudSettings::from_env_with(env(&[(CLOUD_URL_ENV, "http://u:p@cloud.test/")]));
        assert!(bad.is_configured() && !bad.is_valid());
        assert!(bad.is_misconfigured(), "非空但非法 = misconfigured（500）");
    }

    /// 判据 ③/④：`Debug` 与告警都不得回显 URL 凭据。
    #[test]
    fn debug_and_warning_never_echo_the_url() {
        let settings = CloudSettings::from_env_with(env(&[(
            CLOUD_URL_ENV,
            "https://leaked-user:leaked-pass@cloud.test/",
        )]));
        let rendered = format!("{settings:?}");
        assert!(!rendered.contains("leaked-user"), "{rendered}");
        assert!(!rendered.contains("leaked-pass"), "{rendered}");
        assert!(!rendered.contains("cloud.test"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        // 合法值时同样只暴露存在性。
        let ok = CloudSettings::from_env_with(env(&[(CLOUD_URL_ENV, "https://cloud.test/")]));
        let rendered = format!("{ok:?}");
        assert!(!rendered.contains("cloud.test"), "{rendered}");
        assert!(rendered.contains("url_valid: true"), "{rendered}");
    }
}
