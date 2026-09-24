//! M7 anchor（`LUM-1765`）：**长连接宿主位** —— 渠道连接的装配点与停机句柄。
//!
//! # 这一片解决什么问题
//!
//! 五个平台的入站**全部是出站长连接**（slack 的 Socket Mode、lark 的自建 WS、dingtalk 的
//! Stream、wecom 的 aibot、telegram 的 `getUpdates` 长轮询）**没有任何 webhook 路由**
//! （`docs/60` §1.5）。长连接的生命周期（连接 / 退避重连 / 租约 / 优雅停机）**必须有自己的
//! 运行时宿主**，不能寄生在 HTTP router 上 —— 那就是本文件。
//!
//! # 为什么宿主在 `apps/mc-server`（而不是 `mc-http`）
//!
//! M5-9 已把「后台任务宿主」定在这里（`apps/mc-server/src/scheduler/` + `main.rs` 第 7 步，
//! `docs/48` §7.1），且 `main.rs` 已有「先停调度器、再停 actor」的停机链 ⇒ 渠道 supervisor
//! 加进**同一条链**（新增一步：**先停渠道连接**）。与 `scheduler/*_port.rs` 同造型：
//! **端口实现住在二进制 crate 里**（它们要打真库、构造 `Id`/时间戳，不该塞进 `mc-channel`）。
//!
//! # 装配判据 = **部署密钥存在**（`docs/60` §2.4 / §2.6 第 3 条）
//!
//! 缺 `MULTICA_<CHANNEL>_SECRET_KEY` ⇒ 该渠道**整体不装配**（不起连接、不注册工厂），
//! 但它的 HTTP 路由**仍然存在**，并按各端点自己的"未配置"语义回响应
//! （lark 列表是 200 空 + `install_supported:false`，**不是**统一 503）。
//!
//! # 锚点期的空跑（**显式**，不是静默失效）
//!
//! 端口实现（[`mc_channel::engine::InstallationStore`] / `LeaseStore` / `InboundHandler`）
//! 归 M7-1 / M7-2；所以 [`start`] 的第二个实参现在是 `None`，且：
//!
//! - 没有部署密钥 ⇒ 一个平台都不装配（**正常**路径，本锚点起服务器时不报错）；
//! - 有部署密钥但 `deps = None` ⇒ **只打一条 warn** 并明说"端口未接线"——
//!   绝不假装连上了（那会让"渠道没接"变成运行期才发现的静默失效，正是 `docs/37` 反复
//!   登记的那类事故）。
//!
//! # 停机顺序（`docs/60` §2.4 / R-M7-7，固定，不许改）
//!
//! **先停渠道连接 → 再停调度器 → 最后停 actor**：渠道连接挂着不退会让进程 graceful
//! shutdown 永远等在那里；而调度器的在跑 handler 要先收尾，actor 池最后停。

use std::sync::Arc;

use mc_channel::engine::ChannelDeps;
use mc_channel::registry::Registry;
use mc_core::channel::ChannelKind;
use mc_http::state::ChannelKeys;

/// 渠道面的装配结果：注册表 + 已配置平台 + 已起的监管句柄。
///
/// 句柄是空的（anchor 期没有真连接），但**类型先定死**：`main.rs` 的停机链是启动/停止顺序的
/// 唯一实现点，不该等到 M7-1 才改写。
pub struct ChannelHandles {
    registry: Arc<Registry>,
    configured: Vec<ChannelKind>,
    supervisors: Vec<mc_channel::engine::SupervisorHandle>,
    wired: bool,
}

impl std::fmt::Debug for ChannelHandles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelHandles")
            .field("registry", &self.registry)
            .field("configured", &self.configured)
            .field("supervisors", &self.supervisors.len())
            .field("wired", &self.wired)
            .finish()
    }
}

impl ChannelHandles {
    /// 装配好的注册表（宿主/诊断读它决定哪些平台真的有工厂）。
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// 已配置部署密钥的平台（**字典序**）。
    pub fn configured(&self) -> &[ChannelKind] {
        &self.configured
    }

    /// 端口是否已接线（`false` = 锚点期，或 M7-1/M7-2 还没接）。
    pub fn is_wired(&self) -> bool {
        self.wired
    }

    /// 有没有起任何连接。
    pub fn has_connections(&self) -> bool {
        !self.supervisors.is_empty()
    }

    /// 停机：按「先停渠道连接」的次序收掉全部监管任务。
    ///
    /// 每个句柄是 `abort` + `await`（见 [`mc_channel::engine::SupervisorHandle::shutdown`]）：
    /// `abort` 保证**不**无限等一条卡死的平台连接，`await` 保证收尾（关 socket / 释放租约）
    /// 在返回前跑完或已被取消。
    pub async fn shutdown(self) {
        for handle in self.supervisors {
            handle.shutdown().await;
        }
    }
}

/// 装配并启动渠道连接面（`main.rs` 在 `scheduler::start` **之前**调用）。
///
/// `deps` = 端口袋（安装行读取 / 租约 / 入站 handler）。锚点期传 `None`：
/// 端口实现归 M7-1（`InstallationStore` + handler）与 M7-2（`LeaseStore`）。
///
/// **不**返回错误：装配期的失败（未知 kind / 配置非法）在 M7-1 起连接时才会出现，
/// 而那时应当是**该 installation** 失败 + 退避重试，不是整个进程起不来。
pub fn start(keys: &ChannelKeys, deps: Option<Arc<ChannelDeps>>) -> ChannelHandles {
    let registry = Arc::new(Registry::new());
    let configured = keys.configured();

    // 未配置 ⇒ 一个平台都不装配（**正常**路径：自托管的单平台部署就是这个形状）。
    if configured.is_empty() {
        tracing::info!("no channel deployment keys configured; channel connections disabled");
        return ChannelHandles {
            registry,
            configured,
            supervisors: Vec::new(),
            wired: deps.is_some(),
        };
    }

    // 有密钥但端口未接线 ⇒ 只 warn + 明说，不起连接（绝不假装连上了）。
    let Some(deps) = deps else {
        tracing::warn!(
            configured = ?configured,
            "channel deployment keys are set but the engine ports are not wired yet \
             (M7-1 / M7-2); no long-connection will be started"
        );
        return ChannelHandles {
            registry,
            configured,
            supervisors: Vec::new(),
            wired: false,
        };
    };

    // 每个已配置的平台注册自己的工厂（anchor 期五个 `register()` 都是空实现 ⇒
    // 注册表仍为空，这正是"零路由、零读数变化"的形态证据）。
    for kind in &configured {
        match kind {
            ChannelKind::Slack => mc_channel::slack::register(&registry, &deps),
            ChannelKind::Lark => mc_channel::lark::register(&registry, &deps),
            ChannelKind::DingTalk => mc_channel::dingtalk::register(&registry, &deps),
            ChannelKind::WeCom => mc_channel::wecom::register(&registry, &deps),
            ChannelKind::Telegram => mc_channel::telegram::register(&registry, &deps),
            ChannelKind::Custom => {}
        }
    }

    tracing::info!(
        configured = ?configured,
        factories = ?registry.kinds(),
        "channel registry assembled"
    );

    // 起监管任务归 M7-1：`Supervisor::spawn` 现在仍是 `todo!()`，所以这里**不**调用它
    // （调用会让服务器在"配了密钥"的部署里 panic —— 那是比"没接上"更糟的失效形态）。
    ChannelHandles {
        registry,
        configured,
        supervisors: Vec::new(),
        wired: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_http::state::ChannelKeys;

    /// 无密钥 ⇒ 不装配、不报错、wired 与 deps 无关（自托管单平台部署的形状）。
    #[test]
    fn no_keys_means_nothing_is_assembled() {
        let handles = start(&ChannelKeys::default(), None);
        assert!(handles.configured().is_empty());
        assert!(!handles.is_wired());
        assert!(!handles.has_connections());
        assert!(handles.registry().is_empty(), "锚点期注册表必须为空");
    }

    /// 有密钥但端口未接线 ⇒ 明说未接线（**不**假装连上、**不** panic）。
    #[test]
    fn keys_without_ports_report_unwired() {
        let keys = ChannelKeys::from_env_with(|name| {
            (name == "MULTICA_LARK_SECRET_KEY")
                .then(|| "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".to_string())
        });
        let handles = start(&keys, None);
        assert_eq!(handles.configured(), [ChannelKind::Lark]);
        assert!(!handles.is_wired());
        assert!(!handles.has_connections());
        assert!(
            handles.registry().is_empty(),
            "端口未接线 ⇒ 不起任何连接、注册表为空"
        );
    }
}
