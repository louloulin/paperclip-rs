//! 全局 AppState：所有 router 共享。

use std::sync::Arc;

use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use mc_secrets::Secrets;
use mc_storage::Storage;
use serde::Serialize;

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
}

#[derive(Clone)]
pub struct RuntimeHandles {
    pub actors: ActorRegistry,
    pub adapters: Arc<AdapterRegistryStub>,
}

#[derive(Default)]
pub struct AdapterRegistryStub {
    // Stub for runtime adapter registration; full version in M3.
    pub names: parking_lot::RwLock<Vec<String>>,
}

impl AdapterRegistryStub {
    pub fn register(&self, name: impl Into<String>) {
        self.names.write().push(name.into());
    }

    pub fn names(&self) -> Vec<String> {
        self.names.read().clone()
    }
}

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
        }
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
}
