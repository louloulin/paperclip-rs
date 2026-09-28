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

use mc_channel::engine::{
    ChannelDeps, ChannelIssueOutcome, ChannelIssueParams, ChatRunParams, Engine, EngineError,
    InProcessLeaseStore, Installation, InstallationStore, IssueCreator, NoCommands, Router,
    RouterConfig, RunTriggerer, SessionReader, WorkspaceIdentity,
};
use mc_channel::registry::Registry;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_core::timestamp::Timestamp;
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

    // `Engine` 持有**同一个** `Arc<Registry>`（`Engine::new` 内部 `Arc::clone`），所以先建
    // engine、后注册工厂是安全的：sweep 在**运行时**才读注册表。
    let engine = match Engine::new(Arc::clone(&registry), deps) {
        Ok(engine) => engine,
        Err(error) => {
            // 时间不变式不成立（`poll <= renew < ttl`）⇒ 这是**装配期**的硬错误，不是某条安装
            // 的失败，所以明说并不起连接（绝不假装连上了）。
            tracing::error!(error = %error, "channel engine could not be assembled; no long-connection will be started");
            return ChannelHandles {
                registry,
                configured,
                supervisors: Vec::new(),
                wired: false,
            };
        }
    };

    // 每个已配置的平台注册自己的**真**工厂（`register_with`，不是 anchor 期的空 `register`）：
    // 部署密钥从 `ChannelKeys::get(kind)` 解成 `SecretBox` 交给平台自己的解密器 —— 这是
    // "配了密钥才起连接"这条判据的落点。
    for kind in &configured {
        register_platform(*kind, &registry, keys);
    }

    tracing::info!(
        configured = ?configured,
        factories = ?registry.kinds(),
        "channel registry assembled"
    );

    // 起监管任务（M7-1 的 `Supervisor::spawn` 已实现，anchor 期那条"`todo!()`"注释已过期）。
    // 它的返回值就是停机句柄 ⇒ 进 `supervisors`，停机链第一步（`shutdown`）才收得掉。
    let supervisors = match Arc::clone(engine.supervisor()).spawn() {
        Ok(handle) => vec![handle],
        Err(error) => {
            tracing::error!(error = %error, "channel supervisor could not be started; no long-connection will be started");
            return ChannelHandles {
                registry,
                configured,
                supervisors: Vec::new(),
                wired: false,
            };
        }
    };

    ChannelHandles {
        registry,
        configured,
        supervisors,
        wired: true,
    }
}

/// 把一个已配置平台的**真**依赖装进注册表（宿主交密钥，不交 env —— `mc-channel` 不读 env）。
///
/// `SecretBox` 由 [`ChannelKeys::get`] 交出；每个平台的 `register_with` 签名**各不相同**
/// （lark 按值收、其余按引用收），所以这里逐条写开而不是硬造一张同签名表。
fn register_platform(kind: ChannelKind, registry: &Registry, keys: &ChannelKeys) {
    // 已配置 ⇒ `get` 必为 `Some`（两者读同一张 `ChannelKeys` 表）；`expect` 只在两者漂移时触发。
    let Some(boxed) = keys.get(kind) else {
        tracing::error!(kind = %kind.as_str(), "configured channel has no deployment key; skipping");
        return;
    };
    match kind {
        ChannelKind::Slack => {
            let deps = mc_channel::slack::SlackDeps::with_secret_box(boxed.clone());
            mc_channel::slack::register_with(registry, &deps);
        }
        ChannelKind::Lark => {
            // lark 的 `register_with` 按值收 `FeishuChannelDeps`；连接器是 M7-11 的产物，
            // 这里**没有** ⇒ 工厂在 `build` 时响亮地拒装配（`InvalidConfig`），不是假装连上。
            mc_channel::lark::register_with(
                registry,
                mc_channel::lark::feishu_channel::FeishuChannelDeps {
                    connector: None,
                    api_client: Arc::new(mc_channel::lark::client::StubApiClient::new()),
                    decrypter: mc_channel::lark::feishu_channel::Decrypter::secret_box(
                        boxed.clone(),
                    ),
                    enricher: None,
                },
            );
        }
        ChannelKind::DingTalk => {
            // dingtalk 的 `Decrypter` 收的是**函数值**（`Fn(&str) -> Result<String, String>`），
            // 而落库的 `app_secret_encrypted` 是 **base64** 的 `secretbox` 密文
            // （`dingtalk::config::encode_ciphertext`）⇒ 这里现搭那条解密链。
            // ⚠️ 错误文案**不带**密文也不带明文（凭据纪律）。
            let decrypt = dingtalk_secretbox_decrypter(boxed.clone());
            let deps = mc_channel::dingtalk::DingTalkDeps::default().with_decrypter(decrypt);
            mc_channel::dingtalk::register_with(registry, &deps);
        }
        ChannelKind::WeCom => {
            // wecom 要的是 `CredentialsResolver`（能解封 `Installation.secret_encrypted`），
            // 不是 `Decrypter` —— 生产实现 `SecretboxCredentialsResolver` 正是同一把盒子。
            mc_channel::wecom::wecom_channel::register_with(
                registry,
                Arc::new(mc_channel::wecom::WeComDeps::new(Arc::new(
                    mc_channel::wecom::credentials::SecretboxCredentialsResolver::new(
                        boxed.clone(),
                    ),
                ))),
            );
        }
        ChannelKind::Telegram => {
            let deps = mc_channel::telegram::TelegramDeps::with_secret_box(boxed.clone());
            mc_channel::telegram::register_with(registry, &deps);
        }
        ChannelKind::Custom => {}
    }
}

/// dingtalk 的生产 `Decrypter`：落库 base64 → `secretbox` 解封 → UTF-8 明文。
///
/// 三个失败分支的错误文案都**不带**密文/明文（`docs/60` §2.3）：`Decrypter::decrypt` 会把这条
/// 串拼进 `ChannelError::InvalidConfig`，而那个错误值会被 HTTP 层序列化。
fn dingtalk_secretbox_decrypter(
    boxed: mc_secrets::secretbox::SecretBox,
) -> mc_channel::dingtalk::Decrypter {
    mc_channel::dingtalk::Decrypter::new(
        "secretbox",
        Arc::new(move |ciphertext: &str| {
            use base64::Engine as _;
            // PostgreSQL 的 `encode(…, 'base64')` 每 64 字符折一行 ⇒ 先去 ASCII 空白
            // （与 `dingtalk::config::strip_whitespace` 同一条判据）。
            let stripped: String = ciphertext
                .chars()
                .filter(|c| !matches!(c, ' ' | '\t' | '\n' | '\r'))
                .collect();
            let sealed = base64::engine::general_purpose::STANDARD
                .decode(&stripped)
                .map_err(|_| "app secret ciphertext is not valid base64".to_string())?;
            let plain = boxed
                .open(&sealed)
                .map_err(|_| "app secret could not be decrypted".to_string())?;
            String::from_utf8(plain).map_err(|_| "app secret is not valid utf-8".to_string())
        }),
    )
}

/// 组装 engine 的端口袋（`main.rs` 第 7 步调用；`start` 的签名不变）。
///
/// # 为什么端口实现住在**这个二进制 crate** 里
///
/// 与 `scheduler/*_port.rs` 同一条纪律（见模块文档）：要打真库、构造 `Id`/时间戳的端口实现
/// 不该塞进 `mc-channel`。`mc-channel` 那边 [`InstallationStore::list_active`] 的文档逐字写着
/// 「DB 实现由后续片落地」——**本片就是那个后续片**。
///
/// # 入站面为什么是**失败关闭**的三个端口
///
/// `Router` 的四个端口里，`CommandClassifier` 有生产实现（`NoCommands`）、`RunTriggerer` 有
/// （`RunBatcher` 包的那个内层触发器属于人起的 chat turn，**还没有**生产实现），
/// 而 [`SessionReader`] / [`IssueCreator`] **一个生产实现都没有**（全仓只有 `#[cfg(test)]` 替身）。
/// 本片要交付的是「连接真的被监管器跑起来」，而 `/issue` 建单与 workspace 身份解析要打
/// `mc-repos`，属于后续片 ⇒ 这里给**失败关闭**的实现：装配照常成功、连接照常起，
/// 真有入站消息走到那两个端口时**响亮地报错**，而不是静默丢弃（静默丢弃 = "渠道没接"的
/// 运行期事故，`docs/37` 反复登记的那一类）。
pub fn build_deps(pool: &sqlx::PgPool) -> Result<Arc<ChannelDeps>, EngineError> {
    let router = Arc::new(Router::new(
        Arc::new(NoCommands),
        Arc::new(NoRunTrigger),
        Arc::new(NoSessionIdentity),
        Arc::new(NoIssueCreation),
        RouterConfig::default(),
    ));
    let leases = InProcessLeaseStore::new("multica", Arc::new(Timestamp::now))
        .map_err(|error| EngineError::infra(error.to_string()))?;
    Ok(Arc::new(ChannelDeps::new(
        router,
        Arc::new(PgActiveInstallations { pool: pool.clone() }),
        Arc::new(leases),
    )))
}

/// `InstallationStore` 的**真** PG 实现（`channel_installation` 里的 `status = 'active'`）。
///
/// 跨全部渠道类型（`channel_type` 逐行解回 [`ChannelKind`]，不按平台过滤 —— 这是
/// `ports.rs` 明确要的形状：上游把这个硬编码的 `feishu` 当成要消灭的限制）。
struct PgActiveInstallations {
    pool: sqlx::PgPool,
}

#[async_trait::async_trait]
impl InstallationStore for PgActiveInstallations {
    async fn list_active(&self) -> Result<Vec<Installation>, EngineError> {
        // 指纹 = `md5(config::text || updated_at)`：**不透明、确定、且凭据一换就变**
        // （密文在 `config` 里）⇒ sweep 之间重装过的渠道会被拆掉重建，而不是拿过期凭据一直跑。
        // ⚠️ 用 PG 的 `md5` 而不是进程内哈希：零新依赖，且**不**把 `config` 明文带进进程。
        let rows: Vec<(uuid::Uuid, String, serde_json::Value, String)> = sqlx::query_as(
            "SELECT id, channel_type, config, md5(config::text || updated_at::text) \
             FROM channel_installation WHERE status = 'active'",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| EngineError::infra(format!("channel_installation sweep: {error}")))?;

        // 认不出的 `channel_type` 跳过并**记一条**（不静默：那是迁移与代码漂移的信号）。
        let mut out = Vec::with_capacity(rows.len());
        for (id, channel_type, config, fingerprint) in rows {
            let Some(kind) = ChannelKind::from_storage_str(&channel_type) else {
                tracing::warn!(channel_type = %channel_type, "unknown channel_installation.channel_type; skipping");
                continue;
            };
            out.push(Installation {
                id: Id(id),
                kind,
                fingerprint,
                config,
            });
        }
        Ok(out)
    }
}

/// 失败关闭的运行触发（人起的 chat run 还没有生产触发器）。
struct NoRunTrigger;

#[async_trait::async_trait]
impl RunTriggerer for NoRunTrigger {
    async fn schedule_chat_run(&self, _params: ChatRunParams) -> Result<(), EngineError> {
        Err(EngineError::infra(
            "channel run trigger is not wired yet; inbound messages are refused, not dropped",
        ))
    }

    async fn drain(&self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// 失败关闭的 workspace 身份读取（`/issue` 的标识符与深链要它；生产实现要打 `mc-repos`）。
struct NoSessionIdentity;

#[async_trait::async_trait]
impl SessionReader for NoSessionIdentity {
    async fn workspace_identity(
        &self,
        _workspace_id: Id,
    ) -> Result<WorkspaceIdentity, EngineError> {
        Err(EngineError::infra(
            "channel session reader is not wired yet; /issue is refused, not silently ignored",
        ))
    }
}

/// 失败关闭的 `/issue` 建单（同上：要有 `mc-repos` 的建单端口）。
struct NoIssueCreation;

#[async_trait::async_trait]
impl IssueCreator for NoIssueCreation {
    async fn create_issue(
        &self,
        _params: ChannelIssueParams,
    ) -> Result<ChannelIssueOutcome, EngineError> {
        Err(EngineError::infra(
            "channel issue creation is not wired yet; /issue is refused, not silently ignored",
        ))
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

    /// **D-1 的验收核心**：配了密钥 + 端口已接线 ⇒ `has_connections() == true`。
    ///
    /// 这一条在 M7-FU 之前**必红**：anchor 期 `start` 从不调 `Supervisor::spawn`，
    /// `supervisors` 恒为空 ⇒ 五个平台一条长连接都没被生产宿主跑起来
    /// （`docs/60-M3-PLAN` 登记的两项掉棒之一）。
    ///
    /// 池子用 `connect_lazy` 指向一个**不存在**的库：`list_active` 会在 sweep 里失败并被
    /// 记一条 error（这正是我们想要的——它证明监管任务**真的在跑**，而不是空壳），
    /// 但不影响本条断言。
    #[tokio::test]
    async fn configured_keys_with_wired_ports_actually_start_connections() {
        let pool =
            sqlx::PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").expect("惰性池构造");
        let deps = build_deps(&pool).expect("端口袋装配");

        let keys = ChannelKeys::from_env_with(|name| {
            (name == "MULTICA_SLACK_SECRET_KEY")
                .then(|| "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".to_string())
        });
        assert_eq!(keys.configured(), [ChannelKind::Slack]);

        let handles = start(&keys, Some(deps));
        assert!(handles.is_wired(), "端口已接线");
        assert!(
            handles.has_connections(),
            "配了密钥 + 端口已接线 ⇒ 必须真的起了监管任务"
        );
        assert_eq!(
            handles.registry().kinds(),
            [ChannelKind::Slack],
            "已配置的平台注册了**真**工厂（`register_with`），不是 anchor 期的空实现"
        );
    }

    /// 无密钥时**不**起连接（装配判据不变：有密钥才装配）。
    #[tokio::test]
    async fn no_keys_means_no_connections_even_with_wired_ports() {
        let pool =
            sqlx::PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").expect("惰性池构造");
        let deps = build_deps(&pool).expect("端口袋装配");
        let handles = start(&ChannelKeys::default(), Some(deps));
        assert!(handles.configured().is_empty());
        assert!(
            !handles.has_connections(),
            "没配密钥 ⇒ 不装配、不起连接（正常路径）"
        );
    }
}
