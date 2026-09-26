//! M9 的**云面部署配置读取口**（`cloud` / `entitlement`）—— 从 `state.rs` 拆出的独立文件。
//!
//! # 为什么单独一个文件（门 ⑩ 的预飞 + R-M9-3）
//!
//! `state.rs` 是**唯一被 M6-0 / M7-0 / M8-0 三个 anchor 追加过**的文件。M7-0 落地后它到
//! 640 行（>620 的预设阈值），M8-0 按 R-M8-7 把「读 env + 构造」下放到
//! `state/integrations.rs`。**M9-0 起手实测 `state.rs` = 665 行**（`docs/62` §8 的 R-M9-3
//! 判据是「>620 就下放」）⇒ 本 anchor 照同一判例把云面的读取口放在这里，
//! `state.rs` 只加两个字段与两行构造。
//!
//! # 三条与 `integrations.rs` 同款的纪律（别开后门）
//!
//! 1. **未配置就是未配置**：缺 / 空 / 全空白 ⇒ 禁用，**绝不**用零值兑底、**绝不** panic；
//! 2. **不新增 `AppState::new` 参数**：本结构在 `AppState::new` 的构造体里读 env；
//! 3. **值不进 `Debug`、不进日志**：非法时那条告警由 [`mc_cloud::config`] 发，且**已脱敏**。
//!
//! # 三态（`docs/62` §2.5 / §2.6 —— 不是两态）
//!
//! | `MULTICA_CLOUD_URL` | [`CloudConfig::is_enabled`] | [`CloudConfig::is_misconfigured`] | 代理路由 |
//! | --- | :-: | :-: | --- |
//! | 未设置 / 空 / 全空白 | `false` | `false` | **403** `cloud_runtime_not_configured` |
//! | 非空但非法 | `false` | `true` | **500** `cloud_runtime_misconfigured` |
//! | 合法 | `true` | `false` | 正常出站 |
//!
//! # entitlement 那一组为什么只有**部署事实**
//!
//! 策略平面是**进程级单例**（`mc_autopilot::quota::install_policy_provider` 的 `OnceLock`），
//! 由组合根 `apps/mc-server/src/entitlement.rs` 安装（`docs/62` §9.8）。
//! `AppState` 造得**早于**那次安装 ⇒ 它只该承载**部署事实**（配没配、合不合法），
//! 不该承载平面本身（否则会出现"同一个进程里两份平面视图"）。
//! 消费者读策略应该走 `mc_autopilot::quota::policy_for(workspace_id)` —— **只有一处真相**。

use std::sync::Arc;

use mc_cloud::config::CloudSettings;
pub use mc_cloud::config::CLOUD_URL_ENV;
use mc_cloud::{Client, Config, RequestRecorder};

/// 云面出站配置 + 客户端（`None` = 未配置或非法）。
///
/// `Clone` 廉价（`Client` 内部是 `Arc`），因为 `AppState` 派生 `Clone`。
#[derive(Clone)]
pub struct CloudConfig {
    settings: CloudSettings,
    client: Option<Client>,
}

impl CloudConfig {
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
        let settings = CloudSettings::from_env_with(get);
        let client = match settings.base_url() {
            // 非法基址 ⇒ `Client::new` 报错 ⇒ 保持 `None`（handler 给 500，不是 403）。
            Some(base_url) => Client::new(Config::new(base_url)).ok(),
            None => None,
        };
        Self { settings, client }
    }

    /// 用一组**已解析**的设置构造（确定性接缝：M9-1/M9-2/M9-6 的替身用例用它与
    /// 可注入的 `base_url` 组合，不必碰进程 env）。
    #[must_use]
    pub fn with_settings(settings: CloudSettings) -> Self {
        let client = settings
            .base_url()
            .and_then(|base_url| Client::new(Config::new(base_url)).ok());
        Self { settings, client }
    }

    /// 换掉计量口（**在构造之后**，因为 `AppState::new` 拿不到指标收集器；
    /// 与上游 `Client.SetRecorder` 同一动机）。
    #[must_use]
    pub fn with_recorder(mut self, recorder: Arc<dyn RequestRecorder>) -> Self {
        let settings = self.settings.clone();
        let client = settings
            .base_url()
            .and_then(|base_url| Client::new(Config::new(base_url).with_recorder(recorder)).ok());
        self.client = client;
        self
    }

    /// env 读取的三态结果（诊断用）。
    #[must_use]
    pub fn settings(&self) -> &CloudSettings {
        &self.settings
    }

    /// 出站客户端（`None` = 未配置**或**非法 —— 用
    /// [`is_misconfigured`](Self::is_misconfigured) 区分）。
    #[must_use]
    pub fn client(&self) -> Option<&Client> {
        self.client.as_ref()
    }

    /// 唯一能发出站请求的状态（合法基址 + 客户端已建）。
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    /// 「配了但非法」⇒ handler 给 **500** `cloud_runtime_misconfigured`。
    #[must_use]
    pub fn is_misconfigured(&self) -> bool {
        self.settings.is_misconfigured()
    }

    /// 是否**存在**配置（含非法配置）。
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.settings.is_configured()
    }

    /// 规范化后的基址（`None` = 未配置）。**不要**把它写进日志或响应。
    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        self.settings.base_url()
    }
}

impl std::fmt::Debug for CloudConfig {
    /// 手写脱敏：**绝不**打印 URL（判据 ①，`docs/62` §2.4）。只暴露三态判定。
    ///
    /// `finish_non_exhaustive` 是**有意**的：`settings` 与 `client` 的原始形态
    /// 都不应该出现在这里（前者持基址、后者持有出站 URL）—— clippy 的
    /// `missing_fields_in_debug` 也会要求这个。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudConfig")
            .field("configured", &self.is_configured())
            .field("enabled", &self.is_enabled())
            .field("misconfigured", &self.is_misconfigured())
            .field("base_url", &self.base_url().map(|_| "<redacted>"))
            .field("recorder", &self.client.as_ref().map(Client::has_recorder))
            .finish_non_exhaustive()
    }
}

/// entitlement 面的**部署事实**（平面本身是进程级单例，见模块头）。
#[derive(Clone, Default)]
pub struct EntitlementConfig {
    settings: CloudSettings,
}

impl EntitlementConfig {
    /// 从进程环境读（**同一个 env**：`MULTICA_CLOUD_URL` —— 云基址是唯一的部署密钥）。
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 从任意「名字 → 值」查询函数读。
    #[must_use]
    pub fn from_env_with<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        Self {
            settings: CloudSettings::from_env_with(get),
        }
    }

    /// 当前环境的部署事实。
    #[must_use]
    pub fn settings(&self) -> &CloudSettings {
        &self.settings
    }

    /// 策略端点可用的**必要**条件（合法云基址）。
    ///
    /// ⚠️ 它是**必要不充分**：平面是否真的装上了还要看组合根
    /// （`mc_autopilot::quota::policy_provider()` 是不是仍是默认的
    /// `NoEntitlementPlane`）。M9-9 的判据要用后者，不能只看这里。
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.settings.is_valid()
    }

    /// 基址（**不要**写进日志或响应）。
    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        self.settings
            .base_url()
            .filter(|_| self.settings.is_valid())
    }
}

impl std::fmt::Debug for EntitlementConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntitlementConfig")
            .field("configured", &self.is_configured())
            .field("base_url", &self.base_url().map(|_| "<redacted>"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }

    /// `DoD` 6：`AppState` 缺 `MULTICA_CLOUD_URL` **不 panic**，且是「未配置」而不是「非法」。
    #[test]
    fn missing_env_is_disabled_not_misconfigured_and_never_panics() {
        let cloud = CloudConfig::from_env_with(env(&[]));
        assert!(!cloud.is_configured());
        assert!(!cloud.is_enabled());
        assert!(!cloud.is_misconfigured());
        assert!(cloud.base_url().is_none());
        assert!(cloud.client().is_none());

        let entitlement = EntitlementConfig::from_env_with(env(&[]));
        assert!(!entitlement.is_configured());
        assert!(entitlement.base_url().is_none());
    }

    #[test]
    fn blank_env_is_also_disabled() {
        for blank in ["", "   ", "/"] {
            let cloud = CloudConfig::from_env_with(env(&[(mc_cloud::CLOUD_URL_ENV, blank)]));
            assert!(!cloud.is_configured(), "{blank:?}");
            assert!(!cloud.is_misconfigured(), "{blank:?}");
        }
    }

    #[test]
    fn a_valid_base_url_enables_the_client() {
        let cloud =
            CloudConfig::from_env_with(env(&[(mc_cloud::CLOUD_URL_ENV, "http://127.0.0.1:9999/")]));
        assert!(cloud.is_configured() && cloud.is_enabled());
        assert!(!cloud.is_misconfigured());
        assert_eq!(cloud.base_url(), Some("http://127.0.0.1:9999"));
        let client = cloud.client().expect("客户端已建");
        assert!(client.enabled());
    }

    /// 非法基址 ⇒ **500 那一态**（不是 403 那一态），且**没有**客户端可以被误用。
    #[test]
    fn an_invalid_base_url_is_misconfigured_and_yields_no_client() {
        for bad in [
            "https://user:pass@cloud.test",
            "https://cloud.test/?token=x",
            "not-a-url",
        ] {
            let cloud = CloudConfig::from_env_with(env(&[(mc_cloud::CLOUD_URL_ENV, bad)]));
            assert!(cloud.is_configured(), "{bad}");
            assert!(!cloud.is_enabled(), "{bad}");
            assert!(cloud.is_misconfigured(), "{bad}");
            assert!(cloud.client().is_none(), "{bad}");
        }
    }

    /// `DoD` 6 的另一半：`Debug` 输出**不含** URL 凭据。
    #[test]
    fn debug_never_echoes_the_base_url() {
        let cloud = CloudConfig::from_env_with(env(&[(
            mc_cloud::CLOUD_URL_ENV,
            "https://leaked-user:leaked-pass@cloud.test/",
        )]));
        let rendered = format!("{cloud:?}");
        assert!(!rendered.contains("leaked-user"), "{rendered}");
        assert!(!rendered.contains("leaked-pass"), "{rendered}");
        assert!(!rendered.contains("cloud.test"), "{rendered}");
        assert!(rendered.contains("misconfigured: true"), "{rendered}");

        let ok =
            CloudConfig::from_env_with(env(&[(mc_cloud::CLOUD_URL_ENV, "https://cloud.test/")]));
        let rendered = format!("{ok:?}");
        assert!(!rendered.contains("cloud.test"), "{rendered}");
        assert!(rendered.contains("enabled: true"), "{rendered}");

        let entitlement = EntitlementConfig::from_env_with(env(&[(
            mc_cloud::CLOUD_URL_ENV,
            "https://cloud.test/",
        )]));
        let rendered = format!("{entitlement:?}");
        assert!(!rendered.contains("cloud.test"), "{rendered}");
        assert!(rendered.contains("configured: true"), "{rendered}");
    }

    /// 计量口的注入口（`with_recorder`）：换口之后客户端仍可用，且口被接上。
    #[test]
    fn recorder_can_be_attached_after_construction() {
        #[derive(Debug)]
        struct Counting(AtomicUsize);
        impl RequestRecorder for Counting {
            fn record_cloud_runtime_request(&self, _op: &str, _status: &str, _duration: f64) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let cloud =
            CloudConfig::from_env_with(env(&[(mc_cloud::CLOUD_URL_ENV, "http://127.0.0.1:9999")]))
                .with_recorder(Arc::new(Counting(AtomicUsize::new(0))));
        assert!(cloud.is_enabled());
        let client = cloud.client().expect("客户端已建");
        assert!(client.has_recorder());
        // 未配置时注入计量口不会凭空造出客户端。
        let disabled = CloudConfig::from_env_with(env(&[]))
            .with_recorder(Arc::new(Counting(AtomicUsize::new(0))));
        assert!(!disabled.is_enabled());
        assert!(disabled.client().is_none());
    }
}
