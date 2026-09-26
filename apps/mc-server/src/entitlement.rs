//! M9 anchor（`LUM-1815`）建桩、**M9-9（`LUM-1824`）原地填充**：entitlement 平面的
//! **组合根适配器**。
//!
//! # 这一片解决什么问题
//!
//! 本仓**已有**一个为这件事准备的接缝：`crates/mc-autopilot/src/quota.rs` 的
//! `QuotaPolicyProvider` trait + `install_policy_provider`（M5-1 落下），模块头逐字写着
//! 「M9/云侧装自己的实现即可让同一批调用点变成按工作区下发策略」。
//! `mc-entitlement` 提供 `Provider`/`Decision`/`Stub`；**适配器**（把云侧策略翻译成本仓的
//! `QuotaPolicy` 并安装）就只能在这里 —— `apps/mc-server` 是唯一同时看得见两者的地方
//! （`docs/62` §9.8 的裁定：不落 `mc-authz`、不并进 `mc-cloud`）。
//!
//! # anchor 期的行为：**诚实空跑**（与 `channels.rs` / `integrations.rs` 同款）
//!
//! | 部署状态 | anchor 期（本文件） | M9-9 之后 |
//! | --- | --- | --- |
//! | 未配 `MULTICA_CLOUD_URL` | `info` 一条，「云基址未配置 ⇒ 平面不装」 | 同（**这是正常路径**） |
//! | 配了且合法 | **`warn`** 一条：「配置了但策略客户端尚未落地（M9-9）⇒ **不装平面**」 | 装平面（`install_policy_provider`） |
//! | 配了但非法 | `warn`（同上，且云代理面给 500） | 同（不装） |
//!
//! 🔴 **两件事绝对不能做**：
//!
//! 1. **不得**装一个替身平面（`mc_entitlement::stub::Stub`）来"让代码看起来接通了"
//!    —— 那是**假策略**：`is_enabled()` 会变成 `true`，而 `GET /api/autopilots/usage`
//!    会开始报 `action=enforce` 并**真的拦住** autopilot。R7/`docs/37` 反复登记的那类
//!    "静默假接入"在配额面上会直接造成生产事故；
//! 2. **不得**在 `AppState::new` 里装平面：`install_policy_provider` 是**进程级一次性**
//!    （后装者被忽略，见 `quota.rs` 的注释）⇒ 装机点必须唯一且显式 = `main.rs` 的第 8.5 步。
//!
//! # 为什么本模块的签名里**没有** `realtime` / `db`
//!
//! 上游 `entitlement.Client` 逐字：「It has **no goroutines or background lifecycle**」
//! ⇒ 它不做投递、不读库、不订阅事件。唯一的外部输入是 `MULTICA_CLOUD_URL`
//! （经 `mc-http` 的 `state::cloud::EntitlementConfig` 读取口进来 ⇒ 生产只有一处 env 解析）。
//!
//! # 停机
//!
//! 平面没有后台任务 ⇒ [`EntitlementHandles::shutdown`] 现在是**空操作**。
//! 保留它的理由与 M7-0 的 `ChannelHandles` 相同：`main.rs` 的停机链是启动/停止顺序的
//! **唯一实现点**，等到 M9-9 真要挂东西（例如一个刷新节流器）时不该改写停机链。

use mc_http::state::cloud::EntitlementConfig;

/// entitlement 面的装配结果。
pub struct EntitlementHandles {
    /// 云基址是否已配置且合法（⇒ M9-9 之后平面会被装上）。
    configured: bool,
    /// 平面是否**真的**装进了进程。
    ///
    /// anchor 期**恒 `false`** —— 这是本文件唯一诚实的值（见模块头第 1 条禁令）。
    wired: bool,
}

impl std::fmt::Debug for EntitlementHandles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EntitlementHandles")
            .field("configured", &self.configured)
            .field("wired", &self.wired)
            .finish()
    }
}

impl EntitlementHandles {
    /// 云基址是否已配置且合法。
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// 策略平面是否已安装（`false` ⇒ quota 恒 `off`）。
    #[must_use]
    pub fn is_wired(&self) -> bool {
        self.wired
    }

    /// 停机：anchor 期是**空操作**（平面没有后台生命周期）。
    ///
    /// `async` 是**签名契约**（与 `channels::ChannelHandles` / `integrations::IntegrationHandles`
    /// 同款）：M9-9 落地后这里会 await 刷新节流器的收尾，而 `main.rs` 的停机链**不**希望
    /// 那时再改一次。空体 + `async` ⇒ 显式 `allow`（先例：`routes/issues/mod.rs` 的
    /// `not_implemented`，同一条 lint）。
    #[allow(clippy::unused_async)]
    pub async fn shutdown(self) {
        // 有意留空：见模块头「停机」段。
    }
}

/// 装配 entitlement 平面（`main.rs` 在**第 8 步之后、调度器之前**调用）。
///
/// `config` = `AppState::entitlement`（云基址的部署事实）。
///
/// **不**返回错误：配置缺失/非法都是**正常**部署形态，不是启动失败
/// （与 `channels::start` / `integrations::start` 同判例）。
pub fn start(config: &EntitlementConfig) -> EntitlementHandles {
    if !config.is_configured() {
        tracing::info!(
            "MULTICA_CLOUD_URL is not configured; entitlement plane stays off \
             (quota judgements return the off shape, and /api/issues/limit-usage keeps 204)"
        );
        return EntitlementHandles {
            configured: false,
            wired: false,
        };
    }

    // ⚠️ 配置了但客户端未落地：**只 warn、不装平面**（模块头第 1 条禁令）。
    // 端点路径在这里出现一次是为了让 `mc-entitlement` 这条依赖边**真的被链进二进制**，
    // 同时给运维一个可 curl 的路径（M9-9 落地后这一行会删掉）。
    tracing::warn!(
        endpoint_prefix = mc_entitlement::client::POLICY_ENDPOINT_PREFIX,
        timeout_secs = mc_entitlement::cache::REQUEST_TIMEOUT.as_secs(),
        "MULTICA_CLOUD_URL is configured but the entitlement policy client is not \
         implemented yet (lands in M9-9); the plane is intentionally NOT installed"
    );
    EntitlementHandles {
        configured: true,
        wired: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(pairs: &[(&str, &str)]) -> EntitlementConfig {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        EntitlementConfig::from_env_with(move |name| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        })
    }

    /// anchor 期的**唯一**可接受行为：配了也不装平面（否则 quota 会被假策略真的拦住）。
    #[test]
    fn anchor_never_installs_a_plane_and_never_panics() {
        for pairs in [
            vec![],
            vec![(mc_cloud_url(), "")],
            vec![(mc_cloud_url(), "http://127.0.0.1:9999/")],
            vec![(mc_cloud_url(), "https://user:pass@cloud.test")],
        ] {
            let handles = start(&config_with(&pairs));
            assert!(!handles.is_wired(), "{pairs:?} ⇒ anchor 期恒不装平面");
        }
        // `configured` 只反映「云基址合法」，与是否装平面**无关**。
        assert!(!start(&config_with(&[])).is_configured());
        assert!(start(&config_with(&[(mc_cloud_url(), "http://127.0.0.1:9999")])).is_configured());
        assert!(!start(&config_with(&[(mc_cloud_url(), "nope")])).is_configured());
    }

    /// 平面未被安装 ⇒ 进程级平面仍是默认的 `NoEntitlementPlane`（quota 恒 `off`）。
    ///
    /// 这条用例是 R-M9-9 的**回归保护**：谁在 anchor 期装了替身平面，它立刻红。
    #[test]
    fn quota_plane_is_still_the_default_after_starting_the_host() {
        let handles = start(&config_with(&[(mc_cloud_url(), "http://127.0.0.1:9999")]));
        assert!(!handles.is_wired());
        let workspace = mc_core::Id::new();
        assert!(
            !mc_autopilot::quota::is_enabled(workspace),
            "anchor 期任何工作区的 quota 都必须仍是 off（不得有假策略）"
        );
        assert!(mc_autopilot::quota::policy_for(workspace).is_none());
        assert_eq!(
            mc_autopilot::quota::off_usage().action,
            mc_autopilot::quota::ACTION_OFF
        );
    }

    /// env 名只有一处字面量（`mc-cloud` 的 `config::CLOUD_URL_ENV`，经 `mc-http`
    /// 的 `state::cloud` 再导出）—— 本 crate **不**依赖 `mc-cloud`。
    fn mc_cloud_url() -> &'static str {
        mc_http::state::cloud::CLOUD_URL_ENV
    }
}
