//! SLACK 安装配置的解码与解密（上游 `internal/integrations/slack/config.go`，144 行）。
//!
//! - **写者**：M7-3（`docs/60-M7-PLAN.md` §3.3）。
//! - **BYO 模型**（上游注释逐字）：每个 agent 的 Slack app 由 workspace 管理员创建并安装，
//!   把 **bot token**（`xoxb-`）与 **app-level token**（`xapp-`）贴进 Multica。所以
//!   `channel_installation` 的**每一行**带自己的两个令牌、拿自己的 Socket Mode 连接。
//! - **路由键 = 真实的 Slack app id**（`config->>'app_id'`，等于入站事件的 `api_app_id`）；
//!   `team_id` 只作展示 —— 注意 BYO 下 `app_id != team_id`。
//!
//! # 凭据纪律（`docs/60` §2.3 的四条判据）
//!
//! 本文件的三个类型都可能持有明文令牌（[`Credentials::bot_token`]、[`InstallConfig`] 的
//! 两个密文列、[`Decrypter`] 的密钥），所以：
//!
//! 1. **手写 `Debug`**：[`Credentials`] / [`InstallConfig`] / [`Decrypter`] / [`SlackDeps`]
//!    都不派生，只打印 `<redacted>` 或字段的存在性；
//! 2. [`ConfigError`] 的**每个变体只带结构信息**（长度 / 字段名 / 来源），**绝不**带明文、
//!    密文或密钥字节 —— 这是「错误路径不回显凭据」的结构性保证；
//! 3. 解密只经 [`mc_secrets::secretbox`]（`nonce(12) ‖ ct ‖ tag` 的单块字节形态，
//!    **不** base64 后再进 `mc_secrets::cipher` 的 JSON payload 形态）。

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use mc_secrets::secretbox::SecretBox;
use serde::{Deserialize, Serialize};

// =====================================================================
// 配置 blob（`channel_installation.config` 的 Slack 形态）
// =====================================================================

/// 一个**手写脱敏**的明文字符串（承载令牌）：`Debug` 只输出 `<redacted>`，
/// 明文只能经 [`Sensitive::expose`] 显式取出 —— 于是「不小心把它插进日志」在**类型层面**
/// 就做不到（`tracing` 的 `?x` / `{}` 都不会吐出令牌）。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Sensitive(String);

impl fmt::Debug for Sensitive {
    /// 手写脱敏（`docs/60` §2.3 第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Sensitive(<redacted>)")
    }
}

impl Sensitive {
    /// 包一个明文。
    #[must_use]
    pub fn new(plaintext: impl Into<String>) -> Self {
        Self(plaintext.into())
    }

    /// 取出明文（**唯一**出口：调用点因此总是显式可见的）。
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 是否为空（含空串 = 未配置）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `channel_installation.config` 的 Slack 形状（上游 `installConfig`）。
///
/// 跨平台列保持扁平，Slack 专有的一切在这个不透明 blob 里（`docs/60` 的 config 边界）。
///
/// `app_id` 是**真实**的 Slack app id（从 `xapp-` 解出来的），也是 per-installation 的
/// 路由键：泛化的 `find_active_by_app_id`（`config->>'app_id'`）与
/// `(channel_type, app_id)` 唯一索引把入站事件的 `api_app_id` 映射到安装行，
/// 所以同一个 Slack workspace 里的多个 app（多个 agent）**彼此不混**。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct InstallConfig {
    /// 真实 Slack app id（路由键）。
    #[serde(default)]
    pub app_id: String,
    /// 工作区 id（**只作展示**；BYO 下与 `app_id` 不同）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub team_id: String,
    /// 本 app 的 bot user id（解析出安装后由工厂赋给 adapter 的 `bot_user_id`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_user_id: String,
    /// `xoxb-`（出站 Web API：`chat.postMessage`），base64 的 `secretbox` 密文，**绝不明文**。
    #[serde(default)]
    pub bot_token_encrypted: String,
    /// `xapp-`（本安装自己的 Socket Mode 连接鉴权），同样只存密文。
    #[serde(default)]
    pub app_token_encrypted: String,
}

impl fmt::Debug for InstallConfig {
    /// 手写脱敏：只打印**哪个**密文列非空，绝不打印它们的值。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallConfig")
            .field("app_id", &self.app_id)
            .field("team_id", &self.team_id)
            .field("bot_user_id", &self.bot_user_id)
            .field(
                "bot_token_encrypted",
                &Self::redaction_of(&self.bot_token_encrypted),
            )
            .field(
                "app_token_encrypted",
                &Self::redaction_of(&self.app_token_encrypted),
            )
            .finish()
    }
}

impl InstallConfig {
    /// 某一列的脱敏说明（`<redacted>` / `<empty>`）—— 运维需要知道"配没配"，不需要值。
    fn redaction_of(value: &str) -> &'static str {
        if value.is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        }
    }
}

/// 公开（非密）子集：管理 API 可以安全暴露的字段（上游 `PublicConfig`）。
///
/// **不含** `*_token_encrypted`（上游注释逐字）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicConfig {
    pub app_id: String,
    pub team_id: String,
    pub bot_user_id: String,
}

/// 解码后的**明文**凭据（上游 `credentials`）。
///
/// 安装的**身份**（workspace / agent / installer）**故意不在**这里：它由 Router 的安装解析器
/// 逐条消息解析（与 Feishu adapter 同形）。
#[derive(Clone)]
pub struct Credentials {
    /// 展示与"同一 Slack 工作区"判据用；空时**回退到** `app_id`（上游 `decodeCredentials`
    /// 的赋值序；注意 [`install_team_id`] **不**回退）。
    pub team_id: String,
    pub bot_user_id: String,
    /// 明文 `xoxb-` 令牌。**绝不**进日志 / `Debug` / 错误文案。
    pub bot_token: String,
}

impl fmt::Debug for Credentials {
    /// 手写脱敏（`docs/60` §2.3 第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("team_id", &self.team_id)
            .field("bot_user_id", &self.bot_user_id)
            .field("bot_token", &"<redacted>")
            .finish()
    }
}

// =====================================================================
// 解密器
// =====================================================================

/// 解密函数的裸形态（抽成别名是为了让 `clippy::type_complexity` 满意）。
pub type DecryptFn = dyn Fn(&[u8]) -> Result<Vec<u8>, DecryptError> + Send + Sync;

/// 解密密文的函数值（上游 `Decrypter`）。
///
/// 生产装配注入 [`mc_secrets::secretbox`] 的实现（[`Decrypter::secret_box`]）；测试注入
/// 身份解密器（[`Decrypter::plaintext`]，上游 `nil` 的等价物："把存下来的字节当明文"）。
///
/// ⚠️ [`Decrypter::plaintext`] 是**测试便利**，不是生产降级：拿它跑生产会把密文当令牌用。
/// 所以 [`Decrypter::fail_closed`] 存在 —— 未经显式注入的工厂（[`crate::slack::register`]）
/// 用它，**宁可拒装配也不假装连上**（与 anchor 的"密钥配了但端口没接线"同一条纪律）。
#[derive(Clone)]
pub struct Decrypter {
    inner: Arc<DecryptFn>,
    label: &'static str,
}

impl fmt::Debug for Decrypter {
    /// 手写脱敏：函数值不可打印，只打印**类别**（生产排查要能区分"接的是哪个解密器"）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Decrypter")
            .field(&self.label)
            .finish()
    }
}

/// 解密失败（上游 `Decrypter` 返回的 `error`）。
///
/// 变体**不带载荷**：既不回显密文也不回显明文（判据 2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecryptError {
    /// `secretbox` 认证失败 / 长度不对（细节见 `mc_secrets::secretbox::SecretBoxError`）。
    #[error("slack: stored secret failed authentication")]
    Authentication,
    /// 这一条装配路径**没有**接入解密器（`fail_closed`）：拒装配，别把密文当明文。
    #[error("slack: no credential decrypter is wired for this build path")]
    NotWired,
    /// 解出来的字节不是 UTF-8（令牌必须是文本）。
    #[error("slack: decrypted secret is not valid UTF-8")]
    NotUtf8,
}

impl Decrypter {
    /// 身份解密器：把存下来的字节当明文（上游 `decrypt == nil` 的语义，**测试便利**）。
    #[must_use]
    pub fn plaintext() -> Self {
        Self {
            inner: Arc::new(|plaintext: &[u8]| Ok(plaintext.to_vec())),
            label: "plaintext(test)",
        }
    }

    /// `secretbox` 解密器（生产形态）。
    #[must_use]
    pub fn secret_box(boxed: SecretBox) -> Self {
        Self {
            inner: Arc::new(move |ciphertext: &[u8]| {
                boxed
                    .open(ciphertext)
                    .map_err(|_| DecryptError::Authentication)
            }),
            label: "secretbox",
        }
    }

    /// 未接线：任何非空密文都**拒绝**（[`crate::slack::register`] 的默认值）。
    #[must_use]
    pub fn fail_closed() -> Self {
        Self {
            inner: Arc::new(|_| Err(DecryptError::NotWired)),
            label: "fail-closed(not-wired)",
        }
    }

    /// 自定义解密器（测试与将来的密钥轮换用）。
    pub fn custom<F>(label: &'static str, f: F) -> Self
    where
        F: Fn(&[u8]) -> Result<Vec<u8>, DecryptError> + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(f),
            label,
        }
    }

    /// 解一块密文。
    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, DecryptError> {
        (self.inner)(ciphertext)
    }
}

// =====================================================================
// 解码
// =====================================================================

/// 配置解码 / 解密失败（上游 `decodeCredentials` 的那几条 `fmt.Errorf`）。
///
/// **每个变体只带字段名或长度**（凭据纪律第 2 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// 空的安装配置（上游 `errors.New("slack: empty installation config")`）。
    #[error("slack: empty installation config")]
    Empty,
    /// 配置 blob 不是合法 JSON 或字段类型不对。
    #[error("slack: decode installation config failed at {location}")]
    Decode { location: String },
    /// `*_token_encrypted` 不是合法 base64。
    #[error("slack: {field} is not valid base64")]
    Base64 { field: &'static str },
    /// 解密失败（原因见 [`DecryptError`]）。
    #[error("slack: decrypt {field} failed: {source}")]
    Decrypt {
        field: &'static str,
        source: DecryptError,
    },
}

/// `bot_token_encrypted` 的字段名（错误文案与解密共用一处字面量）。
pub const FIELD_BOT_TOKEN: &str = "bot_token_encrypted";
/// `app_token_encrypted` 的字段名。
pub const FIELD_APP_TOKEN: &str = "app_token_encrypted";

/// 解析安装配置 blob 并解密两个令牌 —— Slack 配置 JSON 的**唯一**解释点（上游
/// `decodeCredentials`）。
pub fn decode_credentials(
    raw: &serde_json::Value,
    decrypt: &Decrypter,
) -> Result<Credentials, ConfigError> {
    if raw.is_null() {
        return Err(ConfigError::Empty);
    }
    let cfg: InstallConfig =
        serde_json::from_value(raw.clone()).map_err(|error| ConfigError::Decode {
            location: error.to_string(),
        })?;
    let bot_token = decrypt_token(&cfg.bot_token_encrypted, FIELD_BOT_TOKEN, decrypt)?;
    let team_id = if cfg.team_id.is_empty() {
        cfg.app_id.clone()
    } else {
        cfg.team_id.clone()
    };
    Ok(Credentials {
        team_id,
        bot_user_id: cfg.bot_user_id,
        bot_token,
    })
}

/// 解码**非密**子集（上游 `DecodePublicConfig`）：解码失败**不报错**，回零值 —— 管理列表
/// 仍要能渲染这一行的身份列。
#[must_use]
pub fn decode_public_config(raw: &serde_json::Value) -> PublicConfig {
    let cfg: InstallConfig = serde_json::from_value(raw.clone()).unwrap_or_default();
    let team_id = if cfg.team_id.is_empty() {
        cfg.app_id.clone()
    } else {
        cfg.team_id
    };
    PublicConfig {
        app_id: cfg.app_id,
        team_id,
        bot_user_id: cfg.bot_user_id,
    }
}

/// 读**真实**的 Slack team id（上游 `installTeamID`）：缺失 / 解不开 ⇒ 空串。
///
/// 与 [`decode_credentials`] / [`decode_public_config`] 不同，它**不**回退到 `app_id`：
/// team 路由与身份复用必须匹配真实的 Slack 工作区。
#[must_use]
pub fn install_team_id(raw: &serde_json::Value) -> String {
    serde_json::from_value::<InstallConfig>(raw.clone())
        .map(|cfg| cfg.team_id)
        .unwrap_or_default()
}

/// 该安装（它存下来的配置）是否可以服务来自 `event_team_id` 的事件（上游
/// `installationServesTeam`）。
///
/// 入站路由按 `api_app_id` 走，而它标识的是 Slack **app**（不是工作区）：一个 BYO app 被
/// 分发 / 安装到另一个工作区时，事件带的是**同一个** app id。所以还要要求事件的工作区与
/// 安装 bot 所属的工作区一致。**没记录 team 的安装（遗留）一律放行**。
#[must_use]
pub fn installation_serves_team(raw: &serde_json::Value, event_team_id: &str) -> bool {
    let team_id = install_team_id(raw);
    team_id.is_empty() || team_id == event_team_id
}

/// base64 解出存储的密文（容忍 PostgreSQL `encode(...,'base64')` 打的 MIME 换行），再经注入的
/// [`Decrypter`] 解密（上游 `decryptToken`）。
///
/// 空串 ⇒ 空令牌（上游："empty stored value decodes to an empty token"）。
pub fn decrypt_token(
    enc: &str,
    field: &'static str,
    decrypt: &Decrypter,
) -> Result<String, ConfigError> {
    if enc.is_empty() {
        return Ok(String::new());
    }
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(strip_whitespace(enc))
        .map_err(|_| ConfigError::Base64 { field })?;
    let plaintext = decrypt
        .decrypt(&ciphertext)
        .map_err(|source| ConfigError::Decrypt { field, source })?;
    String::from_utf8(plaintext).map_err(|_| ConfigError::Decrypt {
        field,
        source: DecryptError::NotUtf8,
    })
}

/// 去掉 ASCII 空白，使 MIME 折行（每 64 字符一个换行）与不折行的 base64 解出同样的字节。
fn strip_whitespace(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '\t' | '\n' | '\r'))
        .collect()
}

/// base64 编码（BYO 安装落库时的**唯一**写入形态：先 `secretbox.seal`，再 base64）。
#[must_use]
pub fn encode_ciphertext(sealed: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(sealed)
}

// =====================================================================
// 装配袋
// =====================================================================

/// Slack adapter 的注入面（上游 `ChannelDeps`）。
///
/// 上游还有 `Logger`（本仓统一走 `tracing`）与 `Slash`（`/issue` 命令处理器，归 **M7-4**）。
#[derive(Clone)]
pub struct SlackDeps {
    /// 存储密文 → 明文的解密器。
    pub decrypt: Decrypter,
}

impl fmt::Debug for SlackDeps {
    /// 手写脱敏：只打印解密器的**类别**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlackDeps")
            .field("decrypt", &self.decrypt)
            .finish()
    }
}

impl Default for SlackDeps {
    /// 默认**失败关闭**：未显式接线时拒装配，而不是把密文当明文（见 [`Decrypter::fail_closed`]）。
    fn default() -> Self {
        Self {
            decrypt: Decrypter::fail_closed(),
        }
    }
}

impl SlackDeps {
    /// 生产形态：`secretbox` 解密器。
    #[must_use]
    pub fn with_secret_box(boxed: SecretBox) -> Self {
        Self {
            decrypt: Decrypter::secret_box(boxed),
        }
    }

    /// 测试形态：身份解密器（上游 `nil`）。
    #[must_use]
    pub fn plaintext() -> Self {
        Self {
            decrypt: Decrypter::plaintext(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    fn boxed() -> SecretBox {
        SecretBox::new(&KEY).expect("32 字节密钥")
    }

    /// 上游 `TestDecodeCredentials` 的逐条移植：`app_id` 就是 `team_id` 路由键，bot token
    /// 以 base64 明文存储（身份解密器）。
    #[test]
    fn decode_credentials_with_plaintext_decrypter() {
        let raw = serde_json::json!({
            "app_id": "T1",
            "bot_user_id": "UBOT",
            "bot_token_encrypted": "eG94Yi1ib3Q="
        });
        let creds = decode_credentials(&raw, &Decrypter::plaintext()).expect("decode");
        assert_eq!(creds.team_id, "T1");
        assert_eq!(creds.bot_user_id, "UBOT");
        assert_eq!(creds.bot_token, "xoxb-bot");
        assert!(decode_credentials(&serde_json::Value::Null, &Decrypter::plaintext()).is_err());
    }

    /// `secretbox` 往返：seal → base64 → decode 出原文；且 `app_token_encrypted` 走同一条路。
    #[test]
    fn decode_credentials_round_trips_secretbox_ciphertext() {
        let bot = boxed().seal(b"xoxb-real-token").expect("seal");
        let app = boxed().seal(b"xapp-real-token").expect("seal");
        let raw = serde_json::json!({
            "app_id": "A1",
            "team_id": "T9",
            "bot_user_id": "U9",
            "bot_token_encrypted": encode_ciphertext(&bot),
            "app_token_encrypted": encode_ciphertext(&app),
        });
        let creds = decode_credentials(&raw, &Decrypter::secret_box(boxed())).expect("decode");
        assert_eq!(creds.bot_token, "xoxb-real-token");
        assert_eq!(creds.team_id, "T9", "有 team_id 就不回退到 app_id");
        assert_eq!(install_team_id(&raw), "T9");
        assert!(installation_serves_team(&raw, "T9"));
        assert!(!installation_serves_team(&raw, "T8"));
    }

    /// MIME 折行的 base64 与不折行的解出同样字节（上游 `stripWhitespace` 的用例）。
    #[test]
    fn mime_wrapped_base64_decodes_identically() {
        let sealed = boxed().seal(b"xoxb-token").expect("seal");
        let flat = encode_ciphertext(&sealed);
        let wrapped = flat
            .as_bytes()
            .chunks(64)
            .map(|chunk| String::from_utf8_lossy(chunk).to_string())
            .collect::<Vec<_>>()
            .join("\r\n");
        let raw_flat = serde_json::json!({ "app_id": "A1", "bot_token_encrypted": flat });
        let raw_wrapped = serde_json::json!({ "app_id": "A1", "bot_token_encrypted": wrapped });
        let decrypt = Decrypter::secret_box(boxed());
        assert_eq!(
            decode_credentials(&raw_flat, &decrypt)
                .expect("flat")
                .bot_token,
            "xoxb-token"
        );
        assert_eq!(
            decode_credentials(&raw_wrapped, &decrypt)
                .expect("wrapped")
                .bot_token,
            "xoxb-token"
        );
    }

    /// 空令牌列 ⇒ 空令牌（不是错误）；缺 `team_id` ⇒ 回退 `app_id`。
    #[test]
    fn empty_tokens_and_missing_team_fall_back() {
        let raw = serde_json::json!({ "app_id": "A7" });
        let creds = decode_credentials(&raw, &Decrypter::fail_closed()).expect("decode");
        assert_eq!(creds.bot_token, "");
        assert_eq!(creds.team_id, "A7");
        assert_eq!(install_team_id(&raw), "", "`install_team_id` 不回退");
        assert!(
            installation_serves_team(&raw, "T-ANY"),
            "无 team 记录的安装一律放行"
        );
    }

    /// `fail_closed`：有密文就拒（不把密文当明文），且错误文案**不回显**密文。
    #[test]
    fn fail_closed_decrypter_refuses_instead_of_passing_ciphertext_through() {
        let secret = boxed().seal(b"xoxb-DO-NOT-LOG").expect("seal");
        let encoded = encode_ciphertext(&secret);
        let raw = serde_json::json!({ "app_id": "A1", "bot_token_encrypted": encoded });
        let error = decode_credentials(&raw, &Decrypter::fail_closed()).unwrap_err();
        assert_eq!(
            error,
            ConfigError::Decrypt {
                field: FIELD_BOT_TOKEN,
                source: DecryptError::NotWired
            }
        );
        // 错误路径不回显任何凭据材料（判据 3）：明文、密文、base64 都不出现。
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(
                !rendered.contains("xoxb-"),
                "错误文案回显了明文：{rendered}"
            );
            assert!(
                !rendered.contains(&encoded),
                "错误文案回显了密文：{rendered}"
            );
        }
    }

    /// 坏 base64 / 不是 JSON / 空配置：三种失败各自可辨，且都只带结构信息。
    #[test]
    fn malformed_configs_are_classified() {
        let decrypt = Decrypter::plaintext();
        let bad_b64 = serde_json::json!({ "app_id": "A1", "bot_token_encrypted": "not base64 !!" });
        assert_eq!(
            decode_credentials(&bad_b64, &decrypt).unwrap_err(),
            ConfigError::Base64 {
                field: FIELD_BOT_TOKEN
            }
        );
        // 字段类型错 ⇒ Decode（`location` 只带 serde 的位置，不带值）。
        let wrong_type = serde_json::json!({ "app_id": 42 });
        let error = decode_credentials(&wrong_type, &decrypt).unwrap_err();
        assert!(matches!(error, ConfigError::Decode { .. }));
        assert_eq!(
            decode_credentials(&serde_json::Value::Null, &decrypt).unwrap_err(),
            ConfigError::Empty
        );
        assert!(decode_credentials(&serde_json::json!({}), &decrypt).is_ok());
    }

    /// 非 UTF-8 明文 ⇒ 拒绝（令牌必须是文本），错误不带载荷。
    #[test]
    fn non_utf8_plaintext_is_refused() {
        let decrypt = Decrypter::custom("test-binary", |_| Ok(vec![0xff, 0xfe]));
        let raw = serde_json::json!({ "app_id": "A1", "bot_token_encrypted": "AAEC" });
        assert_eq!(
            decode_credentials(&raw, &decrypt).unwrap_err(),
            ConfigError::Decrypt {
                field: FIELD_BOT_TOKEN,
                source: DecryptError::NotUtf8
            }
        );
    }

    /// 公开子集：解不开也回零值；`team_id` 回退 `app_id`；**不含**任何密文列。
    #[test]
    fn public_config_never_carries_ciphertext() {
        let raw = serde_json::json!({
            "app_id": "A1",
            "bot_user_id": "U1",
            "bot_token_encrypted": "eG94Yi1ib3Q=",
            "app_token_encrypted": "eGFwcA==",
        });
        let public = decode_public_config(&raw);
        assert_eq!(public.app_id, "A1");
        assert_eq!(public.team_id, "A1", "缺 team_id ⇒ 回退 app_id");
        assert_eq!(public.bot_user_id, "U1");
        assert_eq!(
            decode_public_config(&serde_json::json!("garbage")),
            PublicConfig::default()
        );
    }

    /// `DoD` 第 6 条：承载凭据的三个类型**手写 `Debug`**，输出不含任何令牌字节。
    #[test]
    fn debug_never_echoes_token_material() {
        let secret = boxed().seal(b"xoxb-DO-NOT-LOG").expect("seal");
        let encoded = encode_ciphertext(&secret);
        let raw = serde_json::json!({
            "app_id": "A1",
            "bot_token_encrypted": encoded,
            "app_token_encrypted": encoded,
        });
        let cfg: InstallConfig = serde_json::from_value(raw.clone()).expect("config");
        let creds = decode_credentials(&raw, &Decrypter::secret_box(boxed())).expect("decode");
        let rendered = [
            format!("{cfg:?}"),
            format!("{creds:?}"),
            format!("{:?}", Decrypter::secret_box(boxed())),
            format!("{:?}", SlackDeps::with_secret_box(boxed())),
        ];
        for text in &rendered {
            assert!(!text.contains("xoxb-DO-NOT-LOG"), "回显了明文：{text}");
            assert!(!text.contains(&encoded), "回显了密文：{text}");
        }
        assert!(rendered[1].contains("<redacted>"));
        assert!(rendered[0].contains("<redacted>"));
        assert!(rendered[2].contains("secretbox"));
        assert!(format!("{:?}", Decrypter::fail_closed()).contains("fail-closed"));
    }
}
