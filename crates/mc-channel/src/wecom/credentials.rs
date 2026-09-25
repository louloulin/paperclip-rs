//! 凭据解封与**凭据探针**（上游 `internal/integrations/wecom/credentials.go` 55 行 +
//! `credential_probe.go` 205 行，两文件合一）。
//!
//! - **写者**：M7-15（`docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**：`Installation` 自己**从不**持明文（`SecretEncrypted` 是密文），
//!   需要一个真密钥的调用方一律走 [`CredentialsResolver`]；安装时"这对凭据是不是真的"
//!   由 [`CredentialProbe`] 回答。
//!
//! # 为什么两个上游文件合成一个（`docs/32` §31 的 D3）
//!
//! 本片写集只有 7 个 `wecom/*.rs`，没有 `credential_probe.rs`。合成不是图省事：
//! **探针与解封是同一件事的两半** —— `credential_probe.go` 的 `classifySubscribeAck`
//! 同时被安装探针与 `wecom_channel.go` 的重连握手调用，而两者的输入都是
//! [`InstallationCredentials`]。拆开只会多一个跨文件的私有类型。
//!
//! # 探针的 wire 一半是**端口**（不是本片不写）
//!
//! 上游的 `handshakeProbe` 自己 dial `wss://openws.work.weixin.qq.com`、发一帧
//! `aibot_subscribe`、读回自己那个 `req_id` 的 ack。**拨号/帧编解码是 `ws_frame.go`
//! 1179 行的事**，那属于 M7-16（`crates/mc-channel/src/wecom/{ws_frame.rs,ws_sender.rs,
//! stream_store.rs}`），**不在本片写集里** ⇒ 本片把"拨号 + 一帧往返"抽成
//! [`ProbeTransport`]，由 M7-16 提供实现。于是本片能交付并**完整测到**的是判据那一半：
//!
//! - 0 ⇒ 通过；
//! - `40001` / `40013` ⇒ [`ProbeError::Rejected`]（**唯一**可以告诉管理员"你的凭据不对"的答案）；
//! - 其它任何非零（限频 `45009`、并发 `45033`、平台自己故障、未收录的新码）
//!   ⇒ [`ProbeError::Unverifiable`]（**fail-closed**：拒绝这次安装、**不**动已存的凭据、
//!   说"没能验证"）。
//!
//! 这条白名单是**故意**的：把任意非零码读成"密钥错了"会推着管理员去轮换一条本来没问题的
//! 长连接密钥，而轮换后的 `WeCom` 密钥**取不回来** —— 正是本文件要防的那种损失。
//! `45009` 在安装时是"没验证成，等会儿"，在重连路径上就**不能**是"去修这个安装"
//! ⇒ 分类只有一个函数、一个问题只有一份答案。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - [`PlaintextSecret`] **手写 `Debug` / `Display`**（都输出 `<redacted>`），
//!   `expose()` 是**唯一**的明文出口；
//! - [`CredentialsError`] / [`ProbeError`] 的每个变体只带**`WeCom` 自己的 errcode**，
//!   绝不带 secret、密文或密钥字节；
//! - 本文件**没有**任何 `tracing::*` 插值凭据字段（唯一的 `tracing` 是"认不出的 errcode"
//!   告警，只带码与平台文案）。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mc_secrets::secretbox::SecretBox;

use super::types::Installation;

/// 探针的总时限（上游 `credentialProbeTimeout = handshakeTimeout + subscribeTimeout`）。
///
/// ⚠️ 上游那两个常量在 `wecom_channel.go` / `ws_frame.go`（= M7-16 的写集）⇒ 本片先钉一个
/// **本文件自用**的值（15s，覆盖一次拨号 + 一次往返，且是管理员盯着转圈的时长），
/// M7-16 落那两个常量时若不等值，按 `docs/32` §31 的 D3 对齐（不是"顺手改大"）。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// 被 `WeCom` 判为"这对凭据本身不对"的 errcode 白名单（上游 `rejectionErrCodes`，**逐字两条**）。
///
/// - `40001` 不合法的 secret 参数 —— secret 与这个 bot 不匹配；
/// - `40013` 不合法的 `CorpID` —— 这一对身份的一半不是有效值。
///
/// 只比码、**不比** `errmsg` 文本：那是给中文读者看的散文，`WeCom` 随时可以改措辞。
pub const REJECTION_ERR_CODES: [i32; 2] = [40001, 40013];

// =====================================================================
// 明文凭据（唯一出口 = `expose()`）
// =====================================================================

/// 一份明文密钥（上游 `InstallationCredentials.Secret string`）。
///
/// 手写 `Debug` **与** `Display`：两者都只输出 `<redacted>` —— 默认派生会把明文写进任何
/// `{:?}` 插值、`assert_eq!` 失败回显与 panic backtrace（`docs/60` §2.3 第 1 条）。
#[derive(Clone, PartialEq, Eq)]
pub struct PlaintextSecret(String);

impl PlaintextSecret {
    /// 从明文建（**唯一**入口；调用方是解封器与探针的接缝）。
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// 明文出口。**命名故意刺眼**：每一次 `expose()` 都是一个要审的凭据出口。
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 是否为空（空密钥一律拒装，见 [`super::installation`] 的参数校验）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for PlaintextSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlaintextSecret(<redacted>)")
    }
}

impl fmt::Display for PlaintextSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// 一次连接所需的凭据（上游 `InstallationCredentials`）。
///
/// 由解封器**每次按需**铸出，所以明文从不活在持久的 [`Installation`] 上。
#[derive(Clone, PartialEq, Eq)]
pub struct InstallationCredentials {
    /// 机器人标识（`aibot_subscribe` 帧里的认证身份）。
    pub bot_id: String,
    /// 明文密钥（手写脱敏类型）。
    pub secret: PlaintextSecret,
}

impl fmt::Debug for InstallationCredentials {
    /// 手写脱敏（凭据纪律第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallationCredentials")
            .field("bot_id", &self.bot_id)
            .field("secret", &self.secret)
            .finish()
    }
}

// =====================================================================
// 解封
// =====================================================================

/// 解封失败。
///
/// 每个变体只带**结构信息**（长度 / 不透明原因）—— 明文、密文、密钥字节都不进错误值。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialsError {
    /// 密文解不开（被改过 / 换过部署密钥 / 不是 `secretbox` 单块）。
    #[error("wecom: decrypt secret: {reason}")]
    Decrypt { reason: String },
    /// 解出来的明文不是合法 UTF-8（上游是 `string(secret)`，Go 不检查；本仓必须检查，
    /// 否则 `String::from_utf8_lossy` 会**静默**改掉密钥字节）。
    #[error("wecom: decrypted secret is not valid utf-8")]
    NotUtf8,
}

/// 把 [`Installation`] 的密文解成一次连接要用的明文（上游 `CredentialsResolver`）。
pub trait CredentialsResolver: Send + Sync {
    /// 铸一份明文凭据。
    ///
    /// # Errors
    ///
    /// 密文坏 / 明文不是 UTF-8。
    fn credentials(
        &self,
        installation: &Installation,
    ) -> Result<InstallationCredentials, CredentialsError>;
}

/// `secretbox` 解封器（上游 `SecretboxCredentialsResolver`）：
/// 全部署**一把** `MULTICA_WECOM_SECRET_KEY` 封所有安装。
///
/// 轮换 = 换环境变量 + 给每一行做一次重加密迁移（与 Feishu / Slack 同一个故事）。
///
/// **与上游的一处形态差异**：上游构造时要 `box == nil` ⇒ 报错；本仓 [`SecretBox`] 是**值**
/// 类型，`Option` 在类型层面就不存在"半成品解封器"（`docs/32` §31 的 D3）。
#[derive(Clone)]
pub struct SecretboxCredentialsResolver {
    boxed: SecretBox,
}

impl SecretboxCredentialsResolver {
    /// 装配。**必填**封装盒：没有它就不该有解封器（上游逐字：
    /// `the wire-up cannot fall back to plaintext`）。
    #[must_use]
    pub fn new(boxed: SecretBox) -> Self {
        Self { boxed }
    }

    /// 借出封装盒（route 层装配 `InstallationService` 时要把它交给同一个盒）。
    #[must_use]
    pub fn boxed(&self) -> &SecretBox {
        &self.boxed
    }
}

impl fmt::Debug for SecretboxCredentialsResolver {
    /// 手写脱敏（`SecretBox` 自己脱敏，但"能被打印"本身就不该存在）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretboxCredentialsResolver")
    }
}

impl CredentialsResolver for SecretboxCredentialsResolver {
    fn credentials(
        &self,
        installation: &Installation,
    ) -> Result<InstallationCredentials, CredentialsError> {
        let plaintext = self
            .boxed
            .open(&installation.secret_encrypted)
            .map_err(|error| CredentialsError::Decrypt {
                // `SecretBoxError` 的文案只带长度 ⇒ 这条转发仍然不回显载荷。
                reason: error.to_string(),
            })?;
        let secret = String::from_utf8(plaintext).map_err(|_| CredentialsError::NotUtf8)?;
        Ok(InstallationCredentials {
            bot_id: installation.bot_id.clone(),
            secret: PlaintextSecret::new(secret),
        })
    }
}

// =====================================================================
// 探针
// =====================================================================

/// 探针的判决（上游两个哨兵）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    /// `WeCom` 明确拒了这对凭据 —— **唯一**可以告诉管理员"Bot ID 或密钥不对"的答案，
    /// 也是唯一能靠回管理后台解决的答案。
    #[error("wecom: WeCom rejected this bot id and secret (errcode {errcode})")]
    Rejected { errcode: i32 },
    /// 其它一切：拨号失败、握手超时、网络不通、限频、平台自己故障、未收录的码。
    ///
    /// 与 [`ProbeError::Rejected`] 分开是**故意**的：部署只是够不着 `WeCom` 时告诉管理员
    /// "凭据不对"，会推着他去轮换一条本来没问题的密钥。
    #[error("wecom: could not reach WeCom to verify this bot (errcode {errcode})")]
    Unverifiable { errcode: i32 },
}

impl ProbeError {
    /// 稳定错误码（HTTP 层映射用；**不含**凭据）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Rejected { .. } => "wecom_credentials_rejected",
            Self::Unverifiable { .. } => "wecom_credentials_unverifiable",
        }
    }

    /// HTTP 状态码（上游 `writeWecomInstallError`：400 / 503）。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Rejected { .. } => 400,
            Self::Unverifiable { .. } => 503,
        }
    }

    /// 平台自己的错误码（诊断用；0 表示"不是平台的码，是够不着"）。
    #[must_use]
    pub fn errcode(&self) -> i32 {
        match *self {
            Self::Rejected { errcode } | Self::Unverifiable { errcode } => errcode,
        }
    }
}

/// 证明安装者**确实控制**这个机器人（上游 `CredentialProbe`）。
///
/// 为什么必须有它（上游注释逐字的两条后果，都不需要内部人）：路由槽是**全局**的
/// （`idx_channel_installation_type_appid` 里没有 workspace），所以任何人都能用捡来的
/// `bot_id` 加一个垃圾密钥占住别人的槽位；更糟的是"回收死主"按同一个未经证明的 id 跑，
/// 于是一个旁观者贴一个他在列表里读到的 `bot_id`，就能把别人 workspace 的绑定**硬删**。
#[async_trait]
pub trait CredentialProbe: Send + Sync {
    /// 探一次。
    ///
    /// # Errors
    ///
    /// [`ProbeError::Rejected`] / [`ProbeError::Unverifiable`]。
    async fn probe(&self, bot_id: &str, secret: &PlaintextSecret) -> Result<(), ProbeError>;
}

/// 探针的 **wire 一半**（拨号 → 一帧 `aibot_subscribe` → 读回自己 `req_id` 的 ack）。
///
/// 实现归 M7-16（`ws_frame.rs` / `ws_sender.rs`）；本片只依赖这一个方法，于是判据那一半
/// 可测、可复用。返回值是 ack 的 **errcode**（0 = 通过）—— 帧的编解码不越过这条边界。
#[async_trait]
pub trait ProbeTransport: Send + Sync {
    /// 发一次订阅并读回它的 ack。
    ///
    /// # Errors
    ///
    /// 拨号失败 / 写失败 / 读失败 / 超时 —— 一律是**够不着**（不是"凭据不对"）。
    async fn subscribe_ack(
        &self,
        bot_id: &str,
        secret: &PlaintextSecret,
    ) -> Result<i32, TransportError>;
}

/// 传输层失败（上游 `handshakeProbe.Probe` 里那些 `fmt.Errorf("%w: dial: …")`）。
///
/// **不带平台文案**：上游把 `err` 拼进 Go 的错误串，本仓只留一个静态原因 ——
/// 拨号层的错误文本可能带上 URL 或请求体（凭据面）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// 拨号 / 写 / 读失败（含超时）。
    #[error("wecom: probe transport failed: {stage}")]
    Failed { stage: &'static str },
    /// 超出 [`PROBE_TIMEOUT`]。
    #[error("wecom: probe timed out")]
    Timeout,
}

/// 生产探针：一次连接、一次订阅、不注册、不发布 —— 进程里没有别人知道它发生过。
#[derive(Clone)]
pub struct HandshakeProbe {
    transport: Arc<dyn ProbeTransport>,
    timeout: Duration,
}

impl fmt::Debug for HandshakeProbe {
    /// 手写：端口不可打印 ⇒ 只说明端口的**存在性**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HandshakeProbe")
            .field("transport", &"<dyn ProbeTransport>")
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl HandshakeProbe {
    /// 装配（时限 = [`PROBE_TIMEOUT`]）。
    #[must_use]
    pub fn new(transport: Arc<dyn ProbeTransport>) -> Self {
        Self {
            transport,
            timeout: PROBE_TIMEOUT,
        }
    }

    /// 注入时限（用例钉住超时分支）。
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl CredentialProbe for HandshakeProbe {
    async fn probe(&self, bot_id: &str, secret: &PlaintextSecret) -> Result<(), ProbeError> {
        let ack =
            tokio::time::timeout(self.timeout, self.transport.subscribe_ack(bot_id, secret)).await;
        match ack {
            // 超时 / 传输失败 ⇒ **够不着**（errcode 0 = "不是平台的码"）。
            Err(_) | Ok(Err(TransportError::Timeout | TransportError::Failed { .. })) => {
                Err(ProbeError::Unverifiable { errcode: 0 })
            }
            Ok(Ok(errcode)) => classify_subscribe_ack(errcode),
        }
    }
}

/// 把 `aibot_subscribe` ack 的非零 errcode 翻成两个哨兵之一（上游 `classifySubscribeAck`）。
///
/// **两个读者共用这一个函数** —— 本文件的安装探针，以及 M7-16 的重连握手：同一个码
/// 在两处必须得到同一个答案（"限频，等等"不能在一处是 `Unverifiable`、在另一处变成
/// "去修这个安装"）。
///
/// # Errors
///
/// 白名单内 ⇒ [`ProbeError::Rejected`]；其它非零 ⇒ [`ProbeError::Unverifiable`]（fail-closed）。
pub fn classify_subscribe_ack(errcode: i32) -> Result<(), ProbeError> {
    if errcode == 0 {
        return Ok(());
    }
    if REJECTION_ERR_CODES.contains(&errcode) {
        return Err(ProbeError::Rejected { errcode });
    }
    // 不认识的非零码：限频、平台侧故障、或 `WeCom` 新加的码。fail-closed，且**不**怀疑密钥。
    tracing::warn!(
        errcode,
        "wecom: unrecognized subscribe errcode, treating as unverifiable"
    );
    Err(ProbeError::Unverifiable { errcode })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wecom::types::Installation;
    use chrono::Utc;
    use mc_core::channel::InstallationStatus;
    use mc_core::id::Id;
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    /// 固定密钥：`0x00..0x1f`（`mc_secrets::secretbox` 用例里的同一把）。
    const KEY_BYTES: [u8; 32] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    fn boxed() -> SecretBox {
        SecretBox::new(&KEY_BYTES).expect("32 字节密钥")
    }

    fn installation_with(sealed: Vec<u8>) -> Installation {
        let now = Utc::now();
        Installation {
            id: Id::new(),
            workspace_id: Id::new(),
            agent_id: Id::new(),
            installer_user_id: Id::new(),
            status: InstallationStatus::Active,
            bot_id: "bot_5f1c9a".into(),
            secret_encrypted: sealed,
            bot_display_name: String::new(),
            config: Value::Null,
            installed_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    /// 解封往返：`seal → credentials()` 拿回同一份明文。
    #[test]
    fn credentials_round_trip_through_the_secretbox() {
        let resolver = SecretboxCredentialsResolver::new(boxed());
        let sealed = boxed().seal(b"wecom-long-connection-secret").expect("seal");
        let credentials = resolver
            .credentials(&installation_with(sealed))
            .expect("credentials");
        assert_eq!(credentials.bot_id, "bot_5f1c9a");
        assert_eq!(credentials.secret.expose(), "wecom-long-connection-secret");
        assert!(!credentials.secret.is_empty());
    }

    /// 坏密文（太短 / 被改）⇒ 结构化的错误，且**错误文案不含载荷**。
    #[test]
    fn credential_errors_are_structural_only() {
        let resolver = SecretboxCredentialsResolver::new(boxed());
        let short = resolver
            .credentials(&installation_with(vec![1, 2, 3]))
            .unwrap_err();
        assert!(matches!(short, CredentialsError::Decrypt { .. }));
        assert!(short.to_string().contains("too short"), "{short}");

        let mut tampered = boxed().seal(b"DO-NOT-LOG").expect("seal");
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let error = resolver
            .credentials(&installation_with(tampered.clone()))
            .unwrap_err();
        let rendered = format!("{error:?}{error}");
        assert!(!rendered.contains("DO-NOT-LOG"), "{rendered}");
        assert!(!rendered.contains(&hex::encode(&tampered)), "{rendered}");

        // 非 UTF-8 明文单列（上游 Go 不检查；本仓不许静默 lossy 替换密钥字节）。
        let raw = SecretBox::new(&KEY_BYTES)
            .expect("box")
            .seal(&[0xff, 0xfe])
            .expect("seal");
        assert_eq!(
            resolver.credentials(&installation_with(raw)),
            Err(CredentialsError::NotUtf8)
        );
    }

    /// 凭据纪律：明文类型的 `Debug` 与 `Display` 都不回显明文。
    #[test]
    fn plaintext_secret_never_prints_itself() {
        let secret = PlaintextSecret::new("secret-DO-NOT-LOG");
        assert_eq!(format!("{secret:?}"), "PlaintextSecret(<redacted>)");
        assert_eq!(format!("{secret}"), "<redacted>");
        let credentials = InstallationCredentials {
            bot_id: "bot_1".into(),
            secret: secret.clone(),
        };
        let rendered = format!("{credentials:?}");
        assert!(rendered.contains("bot_1"));
        assert!(!rendered.contains("DO-NOT-LOG"), "{rendered}");
        assert_eq!(
            format!("{:?}", SecretboxCredentialsResolver::new(boxed())),
            "SecretboxCredentialsResolver"
        );
    }

    /// 分类白名单（上游 `classifySubscribeAck`）：0 通过、两条拒、（尤其）**45009 不拒**。
    #[test]
    fn only_the_two_documented_codes_blame_the_credentials() {
        assert_eq!(classify_subscribe_ack(0), Ok(()));
        for code in REJECTION_ERR_CODES {
            assert_eq!(
                classify_subscribe_ack(code),
                Err(ProbeError::Rejected { errcode: code })
            );
        }
        // 限频 45009 / 并发 45033 / 平台故障 / 新码 ⇒ 一律"没验证成"，**不**怀疑密钥。
        for code in [45009, 45033, 41001, 999_999] {
            assert_eq!(
                classify_subscribe_ack(code),
                Err(ProbeError::Unverifiable { errcode: code }),
                "errcode {code} 不得被判成凭据错"
            );
        }
        assert_eq!(ProbeError::Rejected { errcode: 40001 }.http_status(), 400);
        assert_eq!(
            ProbeError::Unverifiable { errcode: 45009 }.http_status(),
            503
        );
    }

    /// 探针的**正面控制**：同一个 bot 换密钥（轮换）⇒ 老密钥被拒、新密钥放行。
    ///
    /// 这就是本片专属验收里「轮换后能连 / 不能连的判别」那一条：判别的**唯一**依据是
    /// `WeCom` 自己的 errcode，不是本地缓存，也不是"上次成功过"。
    #[tokio::test]
    async fn rotation_is_decided_by_the_ack_code_not_by_local_state() {
        let probe =
            HandshakeProbe::new(Arc::new(RotatingTransport::new("bot_5f1c9a", "new-secret")));
        assert_eq!(
            probe
                .probe("bot_5f1c9a", &PlaintextSecret::new("new-secret"))
                .await,
            Ok(())
        );
        assert_eq!(
            probe
                .probe("bot_5f1c9a", &PlaintextSecret::new("rotated-away-secret"))
                .await,
            Err(ProbeError::Rejected { errcode: 40001 })
        );
        // 限频：**不**落到 Rejected（否则会推着管理员轮换一条好密钥）。
        assert_eq!(
            probe
                .probe("bot_throttled", &PlaintextSecret::new("any"))
                .await,
            Err(ProbeError::Unverifiable { errcode: 45009 })
        );
    }

    /// 拨号失败 / 超时 ⇒ `Unverifiable`，且**不是** `Rejected`。
    #[tokio::test]
    async fn transport_failures_are_unverifiable_never_rejected() {
        let failing = HandshakeProbe::new(Arc::new(FailingTransport));
        assert_eq!(
            failing.probe("bot_1", &PlaintextSecret::new("s")).await,
            Err(ProbeError::Unverifiable { errcode: 0 })
        );

        let slow = HandshakeProbe::new(Arc::new(FailingTransport)).with_timeout(Duration::ZERO);
        assert_eq!(
            slow.probe("bot_1", &PlaintextSecret::new("s")).await,
            Err(ProbeError::Unverifiable { errcode: 0 })
        );
        // 期号：超时与拨号失败在 HTTP 上同一格（503），文案只说"没验证成"。
        assert_eq!(
            ProbeError::Unverifiable { errcode: 0 }.code(),
            "wecom_credentials_unverifiable"
        );
        assert_eq!(
            ProbeError::Rejected { errcode: 40013 }.code(),
            "wecom_credentials_rejected"
        );
    }

    /// 替身：只认一个 `(bot_id, secret)` 对；`bot_throttled` 回 45009。
    struct RotatingTransport {
        bot_id: &'static str,
        secret: &'static str,
    }

    impl RotatingTransport {
        fn new(bot_id: &'static str, secret: &'static str) -> Self {
            Self { bot_id, secret }
        }
    }

    #[async_trait]
    impl ProbeTransport for RotatingTransport {
        async fn subscribe_ack(
            &self,
            bot_id: &str,
            secret: &PlaintextSecret,
        ) -> Result<i32, TransportError> {
            if bot_id == "bot_throttled" {
                return Ok(45009);
            }
            if bot_id == self.bot_id && secret.expose() == self.secret {
                return Ok(0);
            }
            Ok(40001)
        }
    }

    /// 替身：拨号就失败。
    struct FailingTransport;

    #[async_trait]
    impl ProbeTransport for FailingTransport {
        async fn subscribe_ack(
            &self,
            _bot_id: &str,
            _secret: &PlaintextSecret,
        ) -> Result<i32, TransportError> {
            Err(TransportError::Failed { stage: "dial" })
        }
    }
}
