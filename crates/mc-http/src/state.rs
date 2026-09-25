//! 全局 AppState：所有 router 共享。

use std::sync::Arc;

use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use mc_secrets::{SecretBox, Secrets};
use mc_storage::Storage;
use serde::Serialize;

// M8 anchor scaffold（LUM-1797 / docs/61-M8-PLAN.md §6.3 的 R-M8-7）：github / vcs /
// composio 三组部署密钥的**读取口**拆到独立文件（`state.rs` 已被两个 anchor 追加过，
// M7-0 落地后 640 行 > 620 的预设阈值 ⇒ 不再往本文件堆 env 解析）。
pub mod integrations;

/// 配置快照 —— M3 anchor scaffold（LUM-1406 / docs/15 §7.2 第 6 项）点名的**共享锚点**之一。
///
/// 规则（scaffold 冻结）：新增字段时**只改本文件两处** —— 本 struct 与下面手写的
/// `impl Default for ConfigSnapshot`。所有构造点已按 `..Default::default()` 收尾，
/// 所以给 M3 追加字段不会波及 `apps/mc-server`、`mc-conformance` 或任何 `tests/*.rs`。
///
/// 为什么这里**没有** `#[derive(Default)]`（scaffold 对 §7.2 第 6 项的替代分支）：
/// 本 struct 的默认值是**语义默认**而不是全零 —— `dev_mode: true`（`send-code` 因此返回
/// `dev_code`）、`host: "127.0.0.1"`、`port: 3500`、`session_ttl_secs: 30 天`。
/// 这个手写 impl 由 W1-Google（LUM-1399）先于本片加入；改回 derive 会一次性把它们清零，
/// 静默改变 `/api/auth/send-code` 的行为与 `mc-conformance` 的回放结论（⑨ 门），
/// 并丢掉调用方依赖的默认值。因此按 §7.2 第 6 项的「若不采纳」分支处理：
/// 保留手写 impl，把剩余的字面量构造点收敛成 `..Default::default()`，
/// 并在 PR / issue 里列出全部构造点。
/// `config_snapshot_defaults_are_semantic` 用例锁住这些默认值，
/// 防止后来者「顺手」把它换成 derive。
#[derive(Clone, Debug, Serialize)]
pub struct ConfigSnapshot {
    pub host: String,
    pub port: u16,
    pub session_cookie: String,
    pub api_key_header: String,
    pub csrf_header: String,
    /// 开发模式 —— 当 false 时 cookie 不设 Secure，send-code 不返回 `dev_code`，
    /// 邮件发送用纯生产日志路径。
    pub dev_mode: bool,
    /// Session TTL（秒）；由 `/api/auth/refresh` 与 `verify-code` 使用。
    pub session_ttl_secs: u64,
    /// 验证码 TTL（秒）；`send-code` 签发时写入 `expires_at`。
    pub verification_code_ttl_secs: u64,
    /// `send-code` 速率限制（每邮箱每分钟）。
    pub send_code_per_email_per_min: u32,
    /// 邀请速率限制：单 workspace 每小时最大邀请条数。
    /// `None` 表示未设置（调用方应使用默认值 50）。
    /// 由 M1 sub-issue C 追加。
    pub invitation_per_workspace_per_hour: Option<u32>,
    /// runtime host 段（M3-2 按 `docs/15-M3-PLAN.md` §7.6 **接线**而来）。
    ///
    /// 值来自 `mc_config::RuntimeConfig`（它此前只存在于 `Config` 里、没有出口）。
    /// 这是 §7.6 说的「接线而非新增 env 变量」：字段名与默认值都由 `mc-config`
    /// 决定，本文件不复制一份字面量。M3-3（`mc-task`）用这里的
    /// `max_concurrent_tasks_per_agent` / `lease_secs` / `retry_max` 做并发与重试，
    /// `default_runtime` 决定新 task 的默认 adapter。
    pub runtime: mc_config::RuntimeConfig,
}

#[derive(Clone)]
pub struct RuntimeHandles {
    pub actors: ActorRegistry,
    pub adapters: Arc<AdapterRegistry>,
}

/// 运行时 adapter 注册表。
///
/// M0 脚手架里的 `AdapterRegistry`（空壳 + `names: Vec<String>`）**已在 M3-2
/// 删除**，实现搬到 `mc-runtime`：它要覆盖 launch / 流式事件 / 取消 / 版本探测 /
/// 能力声明，还要能被一致性套件和 `mc-scheduler` 复用，放在 `mc-http` 里没有道理。
///
/// 这里保留 `pub use` 而不是让调用方直接依赖 `mc_runtime`：`mc-http` 的
/// 12 个调用点（`apps/mc-server`、8 个 `tests/*.rs`、`mc-conformance`、本 crate
/// 的 `routes/{auth,inbox}.rs`）只用改标识符，不必各自新增依赖。
///
/// 默认构造是**空注册表**（不探测、不 spawn 任何进程）—— 测试与 conformance
/// 回放不依赖机器上装了哪个 CLI；生产装配用
/// [`AdapterRegistry::with_builtin_adapters`]。
pub use mc_runtime::AdapterRegistry;

/// Google OAuth 出站配置（上游 `handler.GoogleLogin` 读的 `os.Getenv` 面）。
///
/// 为什么挂在 `AppState` 而不是 `ConfigSnapshot`：
/// - `ConfigSnapshot` 派生 `Serialize`，`client_secret` 不应进入任何可序列化的快照
///   （M1-E 之后会有 `/api/config`）；
/// - `ConfigSnapshot` 有 12 处字面量构造（8 个测试文件 + `main.rs` + 本 crate），
///   而 `AppState` 只有 2 处 —— W1-Google 切片只想动 `/auth/*` 路由文件。
///
/// 环境变量（前三个与上游同名；后两个是本仓为测试加的 base URL 覆盖开关）：
/// - `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` —— 任一为空 = 功能未配置；
/// - `GOOGLE_REDIRECT_URI` —— 可选，请求体里的 `redirect_uri` 优先；
/// - `MC_GOOGLE_TOKEN_URL` —— 默认 [`GoogleOAuthConfig::DEFAULT_TOKEN_URL`]；
/// - `MC_GOOGLE_USERINFO_URL` —— 默认 [`GoogleOAuthConfig::DEFAULT_USERINFO_URL`]。
///
/// 完整语义与偏离见 `docs/29-W1-GOOGLE.md`。
#[derive(Clone, Debug)]
pub struct GoogleOAuthConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
    pub token_url: String,
    pub userinfo_url: String,
    /// 出站 HTTP client；`None` = 构造失败（TLS 后端初始化异常）→
    /// `/auth/google` 按「换 token 传输失败」返回 502。
    pub http: Option<reqwest::Client>,
}

impl GoogleOAuthConfig {
    /// 上游 `GoogleLogin` 里硬编码的 token 端点。
    pub const DEFAULT_TOKEN_URL: &'static str = "https://oauth2.googleapis.com/token";
    /// 上游 `GoogleLogin` 里硬编码的 userinfo 端点。
    pub const DEFAULT_USERINFO_URL: &'static str = "https://www.googleapis.com/oauth2/v2/userinfo";
    /// 出站超时（秒）。上游用 `http.DefaultClient`（无超时），本仓给一个上限，
    /// 避免 Google 侧挂起时长期占用 axum worker（登记在 docs/29）。
    pub const HTTP_TIMEOUT_SECS: u64 = 15;

    /// 构造出站 client；失败返回 `None`（不 panic）。
    pub fn build_http_client() -> Option<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(Self::HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| tracing::error!(error = %e, "failed to build google oauth http client"))
            .ok()
    }

    /// 从进程环境读取（生产路径）。
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 从任意名字→值的查询函数读取 —— 与 `mc_config::Config::build_with` 同款，
    /// 让映射本身可以在不触碰进程全局 env 的情况下被单测。
    pub fn from_env_with<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let non_empty = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Self {
            client_id: non_empty("GOOGLE_CLIENT_ID"),
            client_secret: non_empty("GOOGLE_CLIENT_SECRET"),
            redirect_uri: non_empty("GOOGLE_REDIRECT_URI"),
            token_url: non_empty("MC_GOOGLE_TOKEN_URL")
                .unwrap_or_else(|| Self::DEFAULT_TOKEN_URL.to_string()),
            userinfo_url: non_empty("MC_GOOGLE_USERINFO_URL")
                .unwrap_or_else(|| Self::DEFAULT_USERINFO_URL.to_string()),
            http: Self::build_http_client(),
        }
    }

    /// 上游判据：`clientID == "" || clientSecret == ""` → feature disabled。
    pub fn is_configured(&self) -> bool {
        self.client_id.is_some() && self.client_secret.is_some()
    }

    /// `redirect_uri` 解析：请求体优先，其次 `GOOGLE_REDIRECT_URI`，都没有则空串
    /// （上游同样会把空串发给 Google）。
    pub fn redirect_uri_for(&self, requested: Option<&str>) -> String {
        requested
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .or_else(|| self.redirect_uri.clone())
            .unwrap_or_default()
    }
}

impl Default for GoogleOAuthConfig {
    fn default() -> Self {
        Self {
            client_id: None,
            client_secret: None,
            redirect_uri: None,
            token_url: Self::DEFAULT_TOKEN_URL.to_string(),
            userinfo_url: Self::DEFAULT_USERINFO_URL.to_string(),
            http: Self::build_http_client(),
        }
    }
}

/// 插件部署密钥（`MULTICA_PLUGIN_SECRET_KEY`）—— M6 anchor（LUM-1665）落下的**唯一**读取口。
///
/// ## 为什么挂在 `AppState` 而不是某个新 crate
///
/// 上游在进程启动时读一次（`cmd/server/router.go:1281` 的
/// `secretbox.LoadKey("MULTICA_PLUGIN_SECRET_KEY")`），密钥再派生出每一处签名/加密上下文；
/// 本仓照 `GoogleOAuthConfig::from_env()` 的先例，把「读 env」收敛到 `AppState::new`：
/// - `mc-plugin-host`（M6-1）**故意不依赖** `mc-http`（依赖方向是 `mc-http → mc-plugin-host`），
///   所以它只能接受 `&[u8; 32]`，不能自己读 env —— 否则配置入口两处、测试无法注入；
/// - 反过来若把 env 读取放到 M6-1 的 crate 里，`mc-http` 就得给 `AppState::new` 加参数，
///   而那会波及 21 个 `AppState::new` 调用点（含 8 个 `tests/*.rs`、`mc-conformance`）。
///
/// ## 线格式（逐字照抄上游 `internal/util/secretbox/secretbox.go` 的 `LoadKey`）
///
/// - env 值是 **base64（Go 的 `StdEncoding`：带填充的标准字母表）**；
/// - 解出后**必须恰好 32 字节**（AES-256-GCM 的 key）；
/// - 未设置 / 空串 / 非法 base64 / 长度不对 ⇒ 一律**当作未配置**（`None`）——
///   **绝不**用零密钥兜底、**绝不** panic；
/// - **不做 trim**：上游只在 `raw == ""` 时判空，`" abc "` 会走到 base64 解码那一步被拒；
///   本地加 trim 就等于把上游拒绝的输入放行（**拓宽**而不是照搬）。
///
/// 加密块的字节排布（`nonce‖ciphertext‖tag`，进 `plugin_secret.ciphertext` BYTEA）归
/// `mc-plugin-host::credentials`（M6-1）；本文件**只负责把 env 变成 32 字节**。
#[derive(Clone)]
pub struct PluginSecretKey {
    key: [u8; 32],
}

impl PluginSecretKey {
    /// 环境变量名（与上游同名）。
    pub const ENV_VAR: &'static str = "MULTICA_PLUGIN_SECRET_KEY";
    /// 期望的密钥长度（字节）：AES-256-GCM。
    pub const KEY_SIZE: usize = 32;

    /// 从进程环境读取（生产装配点：`AppState::new`）。
    pub fn from_env() -> Option<Self> {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 从任意名字→值的查询函数读取 —— 与 `GoogleOAuthConfig::from_env_with` 同款，
    /// 让映射本身能在不碰进程全局 env 的情况下被单测。
    pub fn from_env_with<F>(get: F) -> Option<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        use base64::Engine as _;

        let raw = get(Self::ENV_VAR)?;
        if raw.is_empty() {
            // 上游：`if raw == "" { return error("… is not set") }`。
            return None;
        }
        let decoded = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
        let key: [u8; Self::KEY_SIZE] = decoded.try_into().ok()?;
        Some(Self { key })
    }

    /// 密钥字节（交给 `mc-plugin-host` 的派生函数；不要外发、不要落日志）。
    pub fn as_bytes(&self) -> &[u8; Self::KEY_SIZE] {
        &self.key
    }
}

impl std::fmt::Debug for PluginSecretKey {
    /// 手写脱敏实现（不派生）：密钥字节**绝不能**进日志/panic backtrace。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PluginSecretKey(<redacted, 32 bytes>)")
    }
}

/// 渠道部署密钥的**唯一**读取口（M7 anchor / `LUM-1765`）。
///
/// 五个平台各一把 AES-256-GCM 密钥（env 名由 `mc_core::channel::ChannelKind::secret_key_env`
/// 给出，**表只有一份**）：`MULTICA_{SLACK,LARK,DINGTALK,WECOM,TELEGRAM}_SECRET_KEY`。
/// 它们封装各安装的凭据（Lark `app_secret`、Slack bot token、WeCom corpsecret、
/// Telegram bot token、DingTalk appsecret），落 BYTEA / JSONB 里的 `nonce‖ct‖tag` 单块
/// （逐字复刻上游 `internal/util/secretbox`，实现见 `mc_secrets::secretbox`）。
///
/// # 三条纪律（与 [`PluginSecretKey`] 同款，别开后门）
///
/// 1. **没有 trim**：上游只在 `raw == ""` 时判空；`" <b64> "` 会被 base64 解码拒掉
///    （加 trim = 放行上游拒绝的输入）；
/// 2. **未配置就是未配置**：缺 / 空 / 非法 base64 / 非 32 字节 ⇒ `None`；
///    **绝不**用零密钥兑底、**绝不** panic —— 组合缺密钥时就**不装配**该平台
///    （`docs/60` §2.4 / §2.6 第 3 条）；
/// 3. **不新增 `AppState::new` 参数**：本结构在构造体内读 env（`PluginSecretKey::from_env` 先例），
///    所以 21 个调用点全不动；测试要注入就手写字面量 / 用 [`ChannelKeys::from_env_with`]。
///
/// 密钥**不进 `Debug`**（手写脱敏），且本结构**不**暴露裸密钥字节：唯一出口是
/// [`ChannelKeys::get`] 返回的 `&SecretBox`（能封/解、不能打印）。
#[derive(Clone, Default)]
pub struct ChannelKeys {
    slack: Option<SecretBox>,
    lark: Option<SecretBox>,
    dingtalk: Option<SecretBox>,
    wecom: Option<SecretBox>,
    telegram: Option<SecretBox>,
}

impl ChannelKeys {
    /// 从进程环境读五把部署密钥（生产装配点：`AppState::new`）。
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 从任意「名字 → 值」查询函数读 —— 让映射本身能在不碰进程全局 env 的情况下被单测
    /// （与 `PluginSecretKey::from_env_with` 同款）。
    pub fn from_env_with<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        use mc_core::channel::ChannelKind;

        let mut keys = Self::default();
        for kind in ChannelKind::BUILTIN {
            // `secret_key_env()` 的 `None` 只可能来自 `Custom`，而 `BUILTIN` 不含它。
            let Some(env_var) = kind.secret_key_env() else {
                continue;
            };
            if let Some(boxed) = mc_secrets::secretbox::load_key_with(env_var, |name| get(name)) {
                match kind {
                    ChannelKind::Slack => keys.slack = Some(boxed),
                    ChannelKind::Lark => keys.lark = Some(boxed),
                    ChannelKind::DingTalk => keys.dingtalk = Some(boxed),
                    ChannelKind::WeCom => keys.wecom = Some(boxed),
                    ChannelKind::Telegram => keys.telegram = Some(boxed),
                    ChannelKind::Custom => {}
                }
            }
        }
        keys
    }

    /// 该平台的封装盒；`None` = 该平台的部署密钥未配置（⇒ 该平台整体不装配）。
    pub fn get(&self, kind: mc_core::channel::ChannelKind) -> Option<&SecretBox> {
        match kind {
            mc_core::channel::ChannelKind::Slack => self.slack.as_ref(),
            mc_core::channel::ChannelKind::Lark => self.lark.as_ref(),
            mc_core::channel::ChannelKind::DingTalk => self.dingtalk.as_ref(),
            mc_core::channel::ChannelKind::WeCom => self.wecom.as_ref(),
            mc_core::channel::ChannelKind::Telegram => self.telegram.as_ref(),
            mc_core::channel::ChannelKind::Custom => None,
        }
    }

    /// 是否配了该平台的密钥（装配判据的布尔形态，`apps/mc-server` 用它）。
    pub fn is_configured(&self, kind: mc_core::channel::ChannelKind) -> bool {
        self.get(kind).is_some()
    }

    /// 已配置的平台（**字典序**，诊断与测试要确定性）。
    pub fn configured(&self) -> Vec<mc_core::channel::ChannelKind> {
        let mut kinds: Vec<mc_core::channel::ChannelKind> = mc_core::channel::ChannelKind::BUILTIN
            .into_iter()
            .filter(|kind| self.is_configured(*kind))
            .collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}

impl std::fmt::Debug for ChannelKeys {
    /// 手写脱敏实现（不派生）：五把密钥的**存在性**可以进日志（运维需要它判断哪个平台没配），
    /// 密钥字节与 base64 **绝不可以**。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelKeys")
            .field("configured", &self.configured())
            .finish()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub runtime: RuntimeHandles,
    pub config: ConfigSnapshot,
    pub storage: Storage,
    pub secrets: Secrets,
    pub feature_flags: Arc<FeatureFlagCatalog>,
    pub realtime: RealtimeHandle,
    pub ws: Arc<WsState>,
    pub auth: mc_auth::SessionStoreContainer,
    /// PAT 存储容器（`mc_auth` 内存实现）。
    ///
    /// **`/api/tokens*` 生产路径不走它**：`routes/pats.rs` 直连
    /// `mc_repos::pat::PatRepo`（`personal_access_token` 表，M1-F / LUM-1375）。
    /// 保留作为无库场景的 fallback，眼下仍被 `POST /api/cli-token`
    /// （`routes/auth.rs`）使用 —— 该路由与本 store 的收敛登记在 `docs/17` R9。
    pub pat: mc_auth::PatStoreContainer,
    pub verification: mc_auth::VerificationStoreContainer,
    /// Google OAuth 出站配置（W1-Google / LUM-1399）。
    ///
    /// 生产装配点就在 `AppState::new`（读进程环境）；测试用结构体字面量注入
    /// 指向本机 stub 的 base URL。
    pub google_oauth: GoogleOAuthConfig,
    /// daemon 面 WebSocket 连接注册表（M3-7 / LUM-1438）。
    ///
    /// 消费 LUM-1439（M3-WS-TRANSPORT）已交付的 `mc_ws::Hub`：hub 自己**不持有 DB、
    /// 不解析 token**，身份由调用方（`routes/daemon/lifecycle.rs` 的
    /// `GET /api/daemon/ws`）构造好后注入。
    ///
    /// 与 `/live-events` 的 `realtime` **是两个不同的 hub**：用户面与 daemon 面的
    /// 订阅集、帧类型、心跳都不同（`docs/37` §4.1）。
    pub daemon_hub: Arc<mc_ws::hub::Hub>,
    /// 「服务端 → runtime」异步请求的内存台账（M3-7）。
    ///
    /// 四类请求（update / models / local-skills / local-skills-import）在上游也是
    /// 进程内 store，无 DB 表 —— 本仓照搬以避开新迁移带来的 schema-drift。
    /// 语义与偏离见 `daemon_requests.rs` 模块头与 `docs/32`。
    pub daemon_requests: Arc<crate::daemon_requests::RequestStore>,
    /// 插件部署密钥（M6 anchor / LUM-1665）—— `None` = 未配置（缺 env / 非法 base64 / 非 32 字节）。
    ///
    /// 消费方**必须 fail-closed**：`None` 时按上游口径返回
    /// `plugin_disabled` / `plugin_surfaces_not_configured`（503），
    /// **不得**跳校验、**不得**用零密钥（见 [`PluginSecretKey`]）。
    pub plugin_key: Option<PluginSecretKey>,
    /// 插件 surface 的**专用**内容 origin（`MULTICA_PLUGIN_SURFACE_ORIGIN`）—— M6 anchor 落下的读取口。
    /// 语义照上游 `cmd/server/router.go:434`：`TrimRight(TrimSpace(v), "/")` ⇒ 空串即 `None`
    /// （surface 功能整体禁用）。`None` 时 M6-6 / M6-7 返回 503
    /// `plugin_surfaces_not_configured`（与 `writeFeatureDisabled` 逐字一致）。
    ///
    /// ⚠️ **本字段只做「读 + 规范化」**：origin 的合法性（必须是合法 origin、且必须与 app/API
    /// origin **不同**）由 M6-6 的 launch handler 判定，非法时返回 500
    /// `plugin_surfaces_misconfigured`（上游 `parsePluginSurfaceOrigin` +
    /// `pluginSurfaceOriginIsDedicated`）。
    ///
    /// 为什么 anchor 一并落这个字段：`state.rs` 是本片**冻结**的共享文件 ——
    /// 若留到 M6-6/M6-7，它们就得回来改锚点文件（本 anchor 存在的唯一理由就是消灭这种改动）。
    /// 已登记 `docs/32` §9。
    pub plugin_surface_origin: Option<String>,
    /// 渠道部署密钥（M7 anchor / `LUM-1765`）—— 五个 `MULTICA_<CHANNEL>_SECRET_KEY` 的**唯一**读取口。
    ///
    /// 与 [`PluginSecretKey`] 同款纪律：读 env、解析失败 = 未配置（`None`）、**绝不**用零密钥兑底、
    /// **绝不** panic；且**不新增 `AppState::new` 参数**（本字段在构造体内读 env ⇒ 21 个调用点
    /// 一个不动，测试里的字面量构造点只有 `routes/auth.rs` 一处，与 M6-0 同判例）。
    ///
    /// 消费方：`apps/mc-server/src/channels.rs` 的装配判据（缺密钥 ⇒ 该平台整体不装配，
    /// `docs/60` §2.6 第 3 条）。详见 [`ChannelKeys`]。
    pub channel_keys: ChannelKeys,
    /// GitHub App 的四类部署密钥 / 标识（M8 anchor / `LUM-1797`）。
    ///
    /// 缺省 = 全部 `None`（**不** panic、**不**用零值兑底）。“能连接”与“能浏览仓库”是
    /// **两个**独立判据（[`integrations::GithubKeys::is_connectable`] /
    /// [`integrations::GithubKeys::is_app_configured`]）⇒ 未配置语义**逐端点不同**
    /// （`docs/61` §2.5）。
    pub github_keys: integrations::GithubKeys,
    /// VCS 面的部署密钥与产品边界开关（M8 anchor / `LUM-1797`）。
    ///
    /// `MULTICA_VCS_SECRET_KEY` 解出的封装盒是每连接 PAT / webhook secret 的**唯一**加密器
    /// （`mc_secrets::secretbox`，**M7-0 建、M8 只读**）；缺密钥 ⇒ connect **绝不**落明文。
    pub vcs_keys: integrations::VcsKeys,
    /// composio 的三类部署配置（M8 anchor / `LUM-1797`）。
    ///
    /// 第四个条件是 feature flag（`mc-feature-flags`）；四者缺一 ⇒ 4 条会话路由 503
    /// （`docs/61` §2.5 的 composio 行）。
    pub composio_keys: integrations::ComposioKeys,
}

impl AppState {
    pub fn new(
        db: Db,
        runtime: RuntimeHandles,
        config: ConfigSnapshot,
        realtime: RealtimeHandle,
        ws: Arc<WsState>,
    ) -> Self {
        Self {
            db,
            runtime,
            config,
            storage: Storage::new(),
            secrets: Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
            feature_flags: Arc::new(FeatureFlagCatalog::new()),
            realtime,
            ws,
            auth: mc_auth::SessionStoreContainer::default(),
            pat: mc_auth::PatStoreContainer::default(),
            verification: mc_auth::VerificationStoreContainer::default(),
            google_oauth: GoogleOAuthConfig::from_env(),
            daemon_hub: Arc::new(mc_ws::hub::Hub::new()),
            daemon_requests: Arc::new(crate::daemon_requests::RequestStore::new()),
            plugin_key: PluginSecretKey::from_env(),
            plugin_surface_origin: plugin_surface_origin_from_env(),
            channel_keys: ChannelKeys::from_env(),
            github_keys: integrations::GithubKeys::from_env(),
            vcs_keys: integrations::VcsKeys::from_env(),
            composio_keys: integrations::ComposioKeys::from_env(),
        }
    }
}

/// 读 `MULTICA_PLUGIN_SURFACE_ORIGIN`（上游 `cmd/server/router.go:434` 的逐字口径）。
///
/// `TrimSpace` → `TrimRight("/")` → 空串即 `None`。**不**校验 origin 合法性
/// （`parsePluginSurfaceOrigin` 是 M6-6 的事，见 [`AppState::plugin_surface_origin`]）。
fn plugin_surface_origin_from_env() -> Option<String> {
    let raw = std::env::var("MULTICA_PLUGIN_SURFACE_ORIGIN").ok()?;
    let normalized = raw.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

impl Default for ConfigSnapshot {
    /// ⚠️ 这些是**语义默认**（取的是 M0/M1 真实装配值，不是结构零值）；
    /// 改这里之前先读本 struct 的文档注释。
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 3500,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            dev_mode: true,
            session_ttl_secs: 60 * 60 * 24 * 30,
            verification_code_ttl_secs: 600,
            send_code_per_email_per_min: 5,
            // None → 调用方按 50/h 兜底（routes/invitations.rs `unwrap_or(50)`）。
            invitation_per_workspace_per_hour: None,
            // 与 `mc_config::Config::default().runtime` 同源（§7.6 接线）。
            runtime: mc_config::RuntimeConfig::default(),
        }
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
    fn config_snapshot_defaults_are_semantic() {
        // M3 anchor scaffold（LUM-1406）：锁住 `ConfigSnapshot::default()` 的语义值。
        // 若有人把手写 impl 换成 `#[derive(Default)]`，本用例会红 —— 那是**故意的**：
        // derive 会把 host/port/session_ttl_secs 清零、把 dev_mode 翻成 false，
        // 静默改变 send-code 与 conformance 回放结论（docs/15 §7.2 第 6 项）。
        let cfg = ConfigSnapshot::default();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 3500);
        assert_eq!(cfg.session_cookie, "multica_session");
        assert_eq!(cfg.api_key_header, "X-Multica-Api-Key");
        assert_eq!(cfg.csrf_header, "X-Multica-Csrf");
        assert!(
            cfg.dev_mode,
            "dev_mode 默认必须是 true（send-code 的 dev_code）"
        );
        assert_eq!(cfg.session_ttl_secs, 60 * 60 * 24 * 30);
        assert_eq!(cfg.verification_code_ttl_secs, 600);
        assert_eq!(cfg.send_code_per_email_per_min, 5);
        assert_eq!(
            cfg.invitation_per_workspace_per_hour, None,
            "None ⇒ 调用方按 50/h 兜底（routes/invitations.rs）"
        );
        // M3-2（§7.6 接线）：runtime 段与 `mc_config` 的默认值逐字一致，
        // 不在这里复制字面量，避免两处默认值漂移。
        let runtime = mc_config::RuntimeConfig::default();
        assert_eq!(cfg.runtime.default_runtime, runtime.default_runtime);
        assert_eq!(
            cfg.runtime.max_concurrent_tasks_per_agent,
            runtime.max_concurrent_tasks_per_agent
        );
        assert_eq!(cfg.runtime.lease_secs, runtime.lease_secs);
        assert_eq!(cfg.runtime.retry_max, runtime.retry_max);
        assert_eq!(cfg.runtime.allow_local_daemon, runtime.allow_local_daemon);
        assert_eq!(cfg.runtime.allow_cloud_runtime, runtime.allow_cloud_runtime);
    }

    #[test]
    fn google_config_defaults_to_upstream_endpoints() {
        let cfg = GoogleOAuthConfig::from_env_with(env(&[]));
        assert!(!cfg.is_configured());
        assert_eq!(cfg.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(
            cfg.userinfo_url,
            "https://www.googleapis.com/oauth2/v2/userinfo"
        );
    }

    #[test]
    fn google_config_reads_and_trims_env() {
        let cfg = GoogleOAuthConfig::from_env_with(env(&[
            ("GOOGLE_CLIENT_ID", "  cid  "),
            ("GOOGLE_CLIENT_SECRET", "secret"),
            ("GOOGLE_REDIRECT_URI", "https://app.example/auth/callback"),
            ("MC_GOOGLE_TOKEN_URL", "http://127.0.0.1:9/token"),
            ("MC_GOOGLE_USERINFO_URL", "http://127.0.0.1:9/userinfo"),
        ]));
        assert!(cfg.is_configured());
        assert_eq!(cfg.client_id.as_deref(), Some("cid"));
        assert_eq!(cfg.token_url, "http://127.0.0.1:9/token");
        assert_eq!(cfg.userinfo_url, "http://127.0.0.1:9/userinfo");
    }

    #[test]
    fn google_config_ignores_blank_env() {
        let cfg = GoogleOAuthConfig::from_env_with(env(&[
            ("GOOGLE_CLIENT_ID", ""),
            ("GOOGLE_CLIENT_SECRET", "  "),
            ("MC_GOOGLE_TOKEN_URL", "   "),
        ]));
        assert!(!cfg.is_configured());
        assert_eq!(cfg.token_url, GoogleOAuthConfig::DEFAULT_TOKEN_URL);
    }

    #[test]
    fn google_redirect_uri_prefers_request() {
        let mut cfg = GoogleOAuthConfig {
            redirect_uri: Some("https://env.example/cb".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.redirect_uri_for(Some(" https://req.example/cb ")),
            "https://req.example/cb"
        );
        assert_eq!(cfg.redirect_uri_for(Some("   ")), "https://env.example/cb");
        assert_eq!(cfg.redirect_uri_for(None), "https://env.example/cb");
        cfg.redirect_uri = None;
        assert_eq!(cfg.redirect_uri_for(None), "");
    }

    /// M6 anchor（LUM-1665）：`PluginSecretKey::from_env_with` 的解析口径。
    ///
    /// 锁两件事：① 合法 base64 的 32 字节才拿到 key；② 未设置/空串/非法 base64/长度不对
    /// 都归 `None`（**不是**错误、**不是**零密钥、**不** panic）。
    #[test]
    fn plugin_secret_key_parsing_matches_upstream_loadkey() {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode([7_u8; 32]);
        let key = PluginSecretKey::from_env_with(env(&[("MULTICA_PLUGIN_SECRET_KEY", &encoded)]))
            .expect("合法的 base64 + 32 字节应解析出 key");
        assert_eq!(key.as_bytes(), &[7_u8; 32]);

        // 未设置 / 空串 ⇒ 未配置（上游：`if raw == "" { return error(… is not set) }`）。
        assert!(PluginSecretKey::from_env_with(env(&[])).is_none());
        assert!(
            PluginSecretKey::from_env_with(env(&[("MULTICA_PLUGIN_SECRET_KEY", "")])).is_none(),
            "空串 = 未配置"
        );

        // 非法 base64 ⇒ 未配置（不是 panic）。
        assert!(PluginSecretKey::from_env_with(env(&[(
            "MULTICA_PLUGIN_SECRET_KEY",
            "not base64 !!"
        )]))
        .is_none());

        // 长度不对（31 / 33 字节）⇒ 未配置。
        let short = base64::engine::general_purpose::STANDARD.encode([0_u8; 31]);
        let long = base64::engine::general_purpose::STANDARD.encode([0_u8; 33]);
        assert!(
            PluginSecretKey::from_env_with(env(&[("MULTICA_PLUGIN_SECRET_KEY", &short)])).is_none()
        );
        assert!(
            PluginSecretKey::from_env_with(env(&[("MULTICA_PLUGIN_SECRET_KEY", &long)])).is_none()
        );

        // **不做 trim**：上游只在 `raw == ""` 时判空，带空白的值会走到 base64 解码被拒。
        let padded = format!(" {encoded} ");
        assert!(
            PluginSecretKey::from_env_with(env(&[("MULTICA_PLUGIN_SECRET_KEY", &padded)]))
                .is_none(),
            "加 trim 会放宽上游拒绝的输入（docs/32 §9 登记的不拓宽原则）"
        );

        // 密钥不进 Debug（脱敏）。
        assert_eq!(format!("{key:?}"), "PluginSecretKey(<redacted, 32 bytes>)");
    }
}
