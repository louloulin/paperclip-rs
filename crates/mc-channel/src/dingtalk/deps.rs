use super::*;

/// 安装配置里的**本片所需字段**（上游 `installConfig` 的收窄形态）。
///
/// ⚠️ 完整的 `installConfig`（含 `robot_code` 的显式语义、`secretbox` 密文的读写、
/// `token.go` 的访问令牌缓存）归 **M7-9** 的 `config.rs`（上游 `config.go` + `token.go`）。
/// 本片只落 Stream 连接真正要的三件事：路由键 `app_id`、明文 `app_secret`、以及
/// **失败关闭**要认出来的密文列名。
///
/// `Debug` 手写脱敏（两个 secret 字段只打印"非空与否"）。
#[derive(Clone, Default, Deserialize)]
pub struct StreamInstallConfig {
    /// `AppKey`（= 路由键；上游把它明文放在 `config->>'app_id'`）。
    #[serde(default)]
    pub app_id: String,
    /// 显式 robot code（上游 `robot_code`；Stream 机器人下它等于 `app_id`）。
    #[serde(default)]
    pub robot_code: String,
    /// `secretbox` 密文（base64）；生产形态，**必须**有解密器才能装配。
    #[serde(default)]
    pub app_secret_encrypted: String,
    /// 明文 `AppSecret`：**只**给本地 / 用例（生产形态一律走密文列）。登记在 `docs/32` §19。
    #[serde(default)]
    pub app_secret: String,
}

impl std::fmt::Debug for StreamInstallConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamInstallConfig")
            .field("app_id", &self.app_id)
            .field("robot_code", &self.robot_code)
            .field(
                "app_secret_encrypted",
                &redaction_of(&self.app_secret_encrypted),
            )
            .field("app_secret", &redaction_of(&self.app_secret))
            .finish()
    }
}

/// 只报告"这个凭据字段**有没有**"，绝不打印它的值。
fn redaction_of(value: &str) -> &'static str {
    if value.is_empty() {
        "<empty>"
    } else {
        "<redacted>"
    }
}

impl StreamInstallConfig {
    /// robot code（上游 `robotCodeOrAppID`：显式值优先，退到 `app_id`）。
    #[must_use]
    pub fn robot_code_or_app_id(&self) -> &str {
        if self.robot_code.is_empty() {
            &self.app_id
        } else {
            &self.robot_code
        }
    }
}

/// 密文解密函数（宿主交进来的那个）。
pub type DecryptFn = dyn Fn(&str) -> Result<String, String> + Send + Sync;

/// 密文解密接缝（上游 `ChannelDeps.Decrypt`）。
///
/// 与 `slack::config::Decrypter` / `telegram::config::Decrypter` 同形（本 crate 的**第三份**；
/// 收敛不在本片写集 —— 见 [`AppSecret`] 的注释与 `docs/32` §19 的 D 项）。
#[derive(Clone)]
pub struct Decrypter {
    inner: Arc<DecryptFn>,
    label: &'static str,
}

impl std::fmt::Debug for Decrypter {
    /// 手写脱敏：函数值不可打印，只打印**类别**（生产排查要能区分接的是哪个解密器）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Decrypter")
            .field("inner", &"<fn>")
            .field("label", &self.label)
            .finish()
    }
}

impl Decrypter {
    /// 就位构造（`label` 只用于诊断）。
    #[must_use]
    pub fn new(label: &'static str, decrypt: Arc<DecryptFn>) -> Self {
        Self {
            inner: decrypt,
            label,
        }
    }

    /// 一个**总是拒绝**的解密器（失败关闭的默认值：`register()` 用它）。
    #[must_use]
    pub fn fail_closed() -> Self {
        Self::new(
            "fail-closed",
            Arc::new(|_ciphertext: &str| {
                Err("no credential decrypter wired for this process".to_string())
            }),
        )
    }

    /// 解密；失败 ⇒ **不带密文**的配置错误（`docs/60` §2.3 第 3 条）。
    ///
    /// # Errors
    ///
    /// 解密器报错 ⇒ [`ChannelError::InvalidConfig`]。
    pub fn decrypt(&self, ciphertext: &str) -> ChannelResult<String> {
        (self.inner)(ciphertext).map_err(|error| ChannelError::InvalidConfig {
            kind: TYPE_DINGTALK.as_str().to_string(),
            reason: format!("decrypt app secret: {error}"),
        })
    }

    /// 解密器类别（诊断）。
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.label
    }
}

/// 工厂关闭需要的三件共享件（上游 `ChannelDeps`）。
#[derive(Clone)]
pub struct DingTalkDeps {
    /// 密文解密器；[`register`] 用失败关闭的那一个，[`register_with`] 用宿主交的这一个。
    pub decrypt: Decrypter,
    /// 连接引导端口（生产 = `reqwest`）。
    pub opener: Arc<dyn ConnectionOpener>,
    /// 拨号端口（生产 = `tokio-tungstenite`）。
    pub dialer: Arc<dyn WsDialer>,
    /// bot 名来源（M7-9；默认 [`NoBotName`] = 失败关闭）。
    pub bot_names: Arc<dyn BotNameSource>,
    /// 时间旋钮（生产默认 = 上游的 30s / 90s / 10s）。
    pub knobs: StreamKnobs,
    /// 队列旋钮（生产默认 = 上游的 8 / 256 / 2048 / 120s）。
    pub limits: DispatchLimits,
    /// **出站**端口（M7-8）：OpenAPI 的令牌铸造 + JSON POST。
    pub outbound: Arc<dyn outbound::OpenApiTransport>,
}

impl std::fmt::Debug for DingTalkDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkDeps")
            .field("decrypt", &self.decrypt)
            .field("opener", &"<dyn ConnectionOpener>")
            .field("dialer", &"<dyn WsDialer>")
            .field("bot_names", &"<dyn BotNameSource>")
            .field("knobs", &self.knobs)
            .field("limits", &self.limits)
            .field("outbound", &"<dyn OpenApiTransport>")
            .finish()
    }
}

impl Default for DingTalkDeps {
    /// 生产默认值（**但凭据面失败关闭**：`register()` 的形态）。
    fn default() -> Self {
        Self {
            decrypt: Decrypter::fail_closed(),
            opener: Arc::new(ReqwestOpener::new()),
            dialer: Arc::new(TungsteniteDialer),
            bot_names: Arc::new(NoBotName),
            knobs: StreamKnobs::default(),
            limits: DispatchLimits::default(),
            outbound: Arc::new(outbound::HttpOpenApi::new()),
        }
    }
}

impl DingTalkDeps {
    /// 接上真正的凭据解密器（宿主把部署密钥交给它）。
    #[must_use]
    pub fn with_decrypter(mut self, decrypt: Decrypter) -> Self {
        self.decrypt = decrypt;
        self
    }

    /// 接上 bot 名来源（M7-9 的 `bot_identity.go` 面）。
    #[must_use]
    pub fn with_bot_names(mut self, bot_names: Arc<dyn BotNameSource>) -> Self {
        self.bot_names = bot_names;
        self
    }

    /// 换时间旋钮（用例用）。
    #[must_use]
    pub fn with_knobs(mut self, knobs: StreamKnobs) -> Self {
        self.knobs = knobs;
        self
    }

    /// 换队列旋钮（用例用）。
    #[must_use]
    pub fn with_limits(mut self, limits: DispatchLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 换出站端口（用例注入替身；生产一般不动）。
    #[must_use]
    pub fn with_outbound(mut self, outbound: Arc<dyn outbound::OpenApiTransport>) -> Self {
        self.outbound = outbound;
        self
    }
}
