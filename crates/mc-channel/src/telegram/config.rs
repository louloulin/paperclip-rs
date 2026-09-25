//! TELEGRAM 安装配置的解码与解密（上游 `internal/integrations/telegram/config.go`，155 行）。
//!
//! - **写者**：M7-5（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §17.1）。
//! - **BYO 模型**（上游注释逐字）：workspace 管理员用 `@BotFather` 建一个 bot，把 **bot
//!   token** 贴进 Multica；安装键是 **bot 的数值 id**（令牌的 `<id>:<secret>` 前缀）。
//!   入站是**每安装一条** `getUpdates` 长轮询（`telegram/mod.rs`）—— Telegram 没有
//!   WebSocket 传输，长轮询是不需要公网 HTTPS 端点的部署形态。
//! - **路由键 = bot 的数值 id**（`config->>'app_id'`）：入站回路**不需要**逐事件路由查询
//!   （一条轮询回路只服务一个安装），但 `(channel_type, app_id)` 唯一索引仍然保证
//!   "一个 bot 只属于一个 agent、跨全部 workspace 唯一"。
//!
//! # 凭据纪律（`docs/60` §2.3 的四条判据）
//!
//! 本文件的三个类型都可能持有明文令牌（[`Credentials::bot_token`]、
//! [`InstallConfig::bot_token_encrypted`]、[`Decrypter`] 的密钥），所以：
//!
//! 1. **手写 `Debug`**：[`Credentials`] / [`InstallConfig`] / [`Decrypter`] / [`TelegramDeps`]
//!    都不派生，只打印 `<redacted>` 或字段的存在性；
//! 2. [`ConfigError`] 的**每个变体只带结构信息**（字段名 / serde 位置 / 来源），**绝不**带
//!    明文、密文或密钥字节 —— 这是「错误路径不回显凭据」的结构性保证；
//! 3. 解密只经 [`mc_secrets::secretbox`]（`nonce(12) ‖ ct ‖ tag` 的单块字节形态，
//!    **不** base64 后再进 `mc_secrets::cipher` 的 JSON payload 形态）。
//!
//! # 与 slack 面的关系（**有意**的本地副本，登记 `docs/32` §17.2）
//!
//! [`Decrypter`] / [`Sensitive`] 与 `crate::slack::config` 里同名的两个类型**形态相同**，
//! 但**故意各持一份**：上游同样是 `slack/config.go` 与 `telegram/config.go` 各自声明
//! `credentials` / `Decrypter`；且两者的错误前缀（`slack:` / `telegram:`）与字段集不同
//! （Slack 两个密文列、Telegram 一个）。收敛成共享件意味着动 M7-3/M7-4 的已合文件
//! （**不在本片写集**）⇒ 本片只新增，登记为后续收敛项。

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use mc_secrets::secretbox::SecretBox;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// =====================================================================
// 配置 blob（`channel_installation.config` 的 Telegram 形态）
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

/// `channel_installation.config` 的 Telegram 形状（上游 `installConfig`）。
///
/// `app_id` 是 bot 的**数值 id**（从令牌的 `<id>:<secret>` 前缀解出来），也是
/// per-installation 的路由键：唯一索引 `(channel_type, app_id)` 保证一个 bot 只映射到一个
/// agent（跨全部 workspace）。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct InstallConfig {
    /// bot 的数值 id（路由键；字符串形态，对齐 `config->>'app_id'` 的表达式索引）。
    #[serde(default)]
    pub app_id: String,
    /// bot 的用户名（`@-mention` 判定要用；上游 `bot_username,omitempty`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_username: String,
    /// bot token 的 base64(`secretbox`) 密文，**绝不明文**（上游注释逐字：mirroring
    /// Slack's `bot_token_encrypted`）。
    #[serde(default)]
    pub bot_token_encrypted: String,
}

impl fmt::Debug for InstallConfig {
    /// 手写脱敏：只打印**哪个**密文列非空，绝不打印它的值。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallConfig")
            .field("app_id", &self.app_id)
            .field("bot_username", &self.bot_username)
            .field(
                "bot_token_encrypted",
                &Self::redaction_of(&self.bot_token_encrypted),
            )
            .finish()
    }
}

impl InstallConfig {
    /// 密文列的脱敏说明（`<redacted>` / `<empty>`）—— 运维需要知道"配没配"，不需要值。
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
/// **不含** `bot_token_encrypted`（上游注释逐字）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicConfig {
    pub bot_id: String,
    pub bot_username: String,
}

/// 解码后的**明文**凭据（上游 `credentials`）。
///
/// 安装的**身份**（workspace / agent / installer）故意不在结构体里：它由 Router 的安装解析器
/// 逐条消息解析（与 Feishu / Slack adapter 同形）。
#[derive(Clone)]
pub struct Credentials {
    /// bot 的数值 id（路由键的字符串形态）。
    pub bot_id: String,
    /// bot 用户名（`@-mention` 判定 + `getMe` 校验用）。
    pub bot_username: String,
    /// 明文 bot token。**绝不**进日志 / `Debug` / 错误文案。
    pub bot_token: String,
}

impl fmt::Debug for Credentials {
    /// 手写脱敏（`docs/60` §2.3 第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("bot_id", &self.bot_id)
            .field("bot_username", &self.bot_username)
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
/// 所以 [`Decrypter::fail_closed`] 存在 —— 未经显式注入的工厂（[`crate::telegram::register`]）
/// 用它，**宁可拒装配也不假装连上**。
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
    #[error("telegram: stored secret failed authentication")]
    Authentication,
    /// 这一条装配路径**没有**接入解密器（`fail_closed`）：拒装配，别把密文当明文。
    #[error("telegram: no credential decrypter is wired for this build path")]
    NotWired,
    /// 解出来的字节不是 UTF-8（令牌必须是文本）。
    #[error("telegram: decrypted secret is not valid UTF-8")]
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

    /// 未接线：任何非空密文都**拒绝**（[`crate::telegram::register`] 的默认值）。
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

/// `bot_token_encrypted` 的字段名（错误文案与解密共用一处字面量）。
pub const FIELD_BOT_TOKEN: &str = "bot_token_encrypted";

/// 配置解码 / 解密失败（上游 `decodeCredentials` 的那几条 `fmt.Errorf`）。
///
/// **每个变体只带字段名或 serde 位置**（凭据纪律第 2 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// 空的安装配置（上游 `errors.New("telegram: empty installation config")`）。
    #[error("telegram: empty installation config")]
    Empty,
    /// 配置 blob 不是合法 JSON 或字段类型不对。
    #[error("telegram: decode installation config failed at {location}")]
    Decode { location: String },
    /// `bot_token_encrypted` 不是合法 base64。
    #[error("telegram: {field} is not valid base64")]
    Base64 { field: &'static str },
    /// 解密失败（原因见 [`DecryptError`]）。
    #[error("telegram: decrypt {field} failed: {source}")]
    Decrypt {
        field: &'static str,
        source: DecryptError,
    },
}

/// 解析安装配置 blob 并解密 bot token —— Telegram 配置 JSON 的**唯一**解释点
/// （上游 `decodeCredentials`）。
pub fn decode_credentials(raw: &Value, decrypt: &Decrypter) -> Result<Credentials, ConfigError> {
    if raw.is_null() {
        return Err(ConfigError::Empty);
    }
    let cfg: InstallConfig =
        serde_json::from_value(raw.clone()).map_err(|error| ConfigError::Decode {
            location: error.to_string(),
        })?;
    let bot_token = decrypt_token(&cfg.bot_token_encrypted, decrypt)?;
    Ok(Credentials {
        bot_id: cfg.app_id,
        bot_username: cfg.bot_username,
        bot_token,
    })
}

/// 解码**非密**子集（上游 `DecodePublicConfig`）：解码失败**不报错**，回零值 —— 管理列表
/// 仍要能渲染这一行的身份列。
#[must_use]
pub fn decode_public_config(raw: &Value) -> PublicConfig {
    let cfg: InstallConfig = serde_json::from_value(raw.clone()).unwrap_or_default();
    PublicConfig {
        bot_id: cfg.app_id,
        bot_username: cfg.bot_username,
    }
}

/// base64 解出存储的密文（容忍 PostgreSQL `encode(...,'base64')` 打的 MIME 换行），再经注入的
/// [`Decrypter`] 解密（上游 `decryptToken`）。
///
/// 空串 ⇒ 空令牌（上游："empty stored value decodes to an empty token"）。
pub fn decrypt_token(enc: &str, decrypt: &Decrypter) -> Result<String, ConfigError> {
    if enc.is_empty() {
        return Ok(String::new());
    }
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(strip_whitespace(enc))
        .map_err(|_| ConfigError::Base64 {
            field: FIELD_BOT_TOKEN,
        })?;
    let plaintext = decrypt
        .decrypt(&ciphertext)
        .map_err(|source| ConfigError::Decrypt {
            field: FIELD_BOT_TOKEN,
            source,
        })?;
    String::from_utf8(plaintext).map_err(|_| ConfigError::Decrypt {
        field: FIELD_BOT_TOKEN,
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
// bot 令牌的形状（上游 `parseBotID`）
// =====================================================================

/// 从 bot token 里解出 bot 的**数值 id**（上游 `parseBotID`，逐字）。
///
/// `BotFather` 的令牌是 `<数值 id>:<secret>`；id 那半是稳定的 per-bot 身份，用作安装的
/// 路由键（`config->>'app_id'`）。返回 `None` 就是上游的 `ErrInvalidBotToken`
/// 三种情形之一：没有 `:`、任一侧为空、id 含非数字字符。
#[must_use]
pub fn parse_bot_id(token: &str) -> Option<String> {
    let (id, secret) = token.trim().split_once(':')?;
    if id.is_empty() || secret.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(id.to_string())
}

/// 解析存储里的 `app_id`（= 数值 id 的字符串形态）为 `i64`（上游
/// `strconv.ParseInt(ic.AppID, 10, 64)`）。
#[must_use]
pub fn parse_stored_bot_id(app_id: &str) -> Option<i64> {
    if app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    app_id.parse::<i64>().ok()
}

// =====================================================================
// 装配袋
// =====================================================================

/// Telegram adapter 的注入面（上游 `ChannelDeps`）。
///
/// 上游还有 `Logger`（本仓统一走 `tracing`）与 `APIBase` / `HTTPClient`（本仓改用
/// `crate::telegram::api` 的进程内基址接缝，与 M8-1 的 `GITHUB_API_BASE` 同款）。
#[derive(Clone)]
pub struct TelegramDeps {
    /// 存储密文 → 明文的解密器。
    pub decrypt: Decrypter,
}

impl fmt::Debug for TelegramDeps {
    /// 手写脱敏：只打印解密器的**类别**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramDeps")
            .field("decrypt", &self.decrypt)
            .finish()
    }
}

impl Default for TelegramDeps {
    /// 默认**失败关闭**：未显式接线时拒装配，而不是把密文当明文（见 [`Decrypter::fail_closed`]）。
    fn default() -> Self {
        Self {
            decrypt: Decrypter::fail_closed(),
        }
    }
}

impl TelegramDeps {
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

    /// 上游 `TestDecodeCredentials` 的等价物：`app_id` 就是 bot id，token 以 base64 明文
    /// 存储（身份解密器）。
    #[test]
    fn decode_credentials_with_plaintext_decrypter() {
        let raw = serde_json::json!({
            "app_id": "123456",
            "bot_username": "acme_bot",
            "bot_token_encrypted": "MTIzNDU2OkFBQQ==",
        });
        let creds = decode_credentials(&raw, &Decrypter::plaintext()).expect("decode");
        assert_eq!(creds.bot_id, "123456");
        assert_eq!(creds.bot_username, "acme_bot");
        assert_eq!(creds.bot_token, "123456:AAA");
        assert!(decode_credentials(&Value::Null, &Decrypter::plaintext()).is_err());
        // 空的配置 blob 也能解码（令牌为空串 ⇒ 空令牌，不是错误）。
        let empty = decode_credentials(&serde_json::json!({}), &Decrypter::plaintext())
            .expect("empty config");
        assert_eq!(empty.bot_token, "");
    }

    /// `secretbox` 往返：seal → base64 → decode 出原文。
    #[test]
    fn decode_credentials_round_trips_secretbox_ciphertext() {
        let sealed = boxed()
            .seal(b"123456:REAL-BOT-TOKEN-do-not-log")
            .expect("seal");
        let raw = serde_json::json!({
            "app_id": "123456",
            "bot_username": "acme_bot",
            "bot_token_encrypted": encode_ciphertext(&sealed),
        });
        let creds = decode_credentials(&raw, &Decrypter::secret_box(boxed())).expect("decode");
        assert_eq!(creds.bot_token, "123456:REAL-BOT-TOKEN-do-not-log");
        assert_eq!(decode_public_config(&raw).bot_username, "acme_bot");
    }

    /// MIME 折行的 base64 与不折行的解出同样字节（上游 `stripWhitespace` 的用例）。
    #[test]
    fn mime_wrapped_base64_decodes_identically() {
        let sealed = boxed().seal(b"123456:TOKEN").expect("seal");
        let flat = encode_ciphertext(&sealed);
        let wrapped = flat
            .as_bytes()
            .chunks(64)
            .map(|chunk| String::from_utf8_lossy(chunk).to_string())
            .collect::<Vec<_>>()
            .join("\r\n");
        let decrypt = Decrypter::secret_box(boxed());
        assert_eq!(
            decrypt_token(&flat, &decrypt).expect("flat"),
            "123456:TOKEN"
        );
        assert_eq!(
            decrypt_token(&wrapped, &decrypt).expect("wrapped"),
            "123456:TOKEN"
        );
        assert_eq!(decrypt_token("", &decrypt).expect("empty"), "");
    }

    /// 坏 base64 / 不是 JSON / 空配置 / 非 UTF-8：四种失败各自可辨，且都只带结构信息。
    #[test]
    fn malformed_configs_are_classified() {
        let decrypt = Decrypter::plaintext();
        assert_eq!(
            decrypt_token("not base64 !!", &decrypt).unwrap_err(),
            ConfigError::Base64 {
                field: FIELD_BOT_TOKEN
            }
        );
        let wrong_type = serde_json::json!({ "app_id": 42 });
        let error = decode_credentials(&wrong_type, &decrypt).unwrap_err();
        assert!(matches!(error, ConfigError::Decode { .. }));
        assert_eq!(
            decode_credentials(&Value::Null, &decrypt).unwrap_err(),
            ConfigError::Empty
        );
        let binary = Decrypter::custom("test-binary", |_| Ok(vec![0xff, 0xfe]));
        assert_eq!(
            decrypt_token("AAEC", &binary).unwrap_err(),
            ConfigError::Decrypt {
                field: FIELD_BOT_TOKEN,
                source: DecryptError::NotUtf8
            }
        );
    }

    /// `fail_closed`：有密文就拒（不把密文当明文），且错误文案**不回显**密文。
    #[test]
    fn fail_closed_decrypter_refuses_instead_of_passing_ciphertext_through() {
        let secret = boxed().seal(b"123456:DO-NOT-LOG").expect("seal");
        let encoded = encode_ciphertext(&secret);
        let raw = serde_json::json!({ "app_id": "1", "bot_token_encrypted": encoded });
        let error = decode_credentials(&raw, &Decrypter::fail_closed()).unwrap_err();
        assert_eq!(
            error,
            ConfigError::Decrypt {
                field: FIELD_BOT_TOKEN,
                source: DecryptError::NotWired
            }
        );
        // 错误路径不回显任何凭据材料（判据 2）：明文、密文、base64 都不出现。
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("123456:DO-NOT-LOG"), "{rendered}");
            assert!(!rendered.contains(&encoded), "{rendered}");
        }
    }

    /// `DoD` 第 6 条：承载凭据的三个类型**手写 `Debug`**，输出不含任何令牌字节。
    #[test]
    fn debug_never_echoes_token_material() {
        let secret = boxed().seal(b"123456:DO-NOT-LOG").expect("seal");
        let encoded = encode_ciphertext(&secret);
        let raw = serde_json::json!({ "app_id": "1", "bot_token_encrypted": encoded });
        let cfg: InstallConfig = serde_json::from_value(raw.clone()).expect("config");
        let creds = decode_credentials(&raw, &Decrypter::secret_box(boxed())).expect("decode");
        let rendered = [
            format!("{cfg:?}"),
            format!("{creds:?}"),
            format!("{:?}", Decrypter::secret_box(boxed())),
            format!("{:?}", TelegramDeps::with_secret_box(boxed())),
            format!("{:?}", Sensitive::new("123456:DO-NOT-LOG")),
        ];
        for text in &rendered {
            assert!(!text.contains("123456:DO-NOT-LOG"), "回显了明文：{text}");
            assert!(!text.contains(&encoded), "回显了密文：{text}");
        }
        assert!(rendered[0].contains("<redacted>"));
        assert!(rendered[1].contains("<redacted>"));
        assert!(rendered[4].contains("<redacted>"));
        assert!(format!("{:?}", Decrypter::plaintext()).contains("plaintext"));
        assert!(format!("{:?}", Decrypter::fail_closed()).contains("fail-closed"));
    }

    /// `parse_bot_id` 的三种拒绝 + 一种接受（上游 `TestParseBotID` 的逐条等价物）。
    #[test]
    fn bot_id_parsing_matches_the_upstream_grammar() {
        assert_eq!(parse_bot_id("123456:ABC-DEF"), Some("123456".to_string()));
        assert_eq!(
            parse_bot_id("  123456:ABC-DEF  "),
            Some("123456".to_string()),
            "两侧空白先 trim"
        );
        assert_eq!(parse_bot_id("123456"), None, "没有 `:`");
        assert_eq!(parse_bot_id(":ABC"), None, "id 为空");
        assert_eq!(parse_bot_id("123:ABC"), Some("123".to_string()));
        assert_eq!(parse_bot_id("123456:"), None, "secret 为空");
        assert_eq!(parse_bot_id("abc:ABC"), None, "id 非数字");
        assert_eq!(parse_bot_id("123a:ABC"), None, "id 混合非数字");
        assert_eq!(parse_stored_bot_id("123456"), Some(123_456));
        assert_eq!(parse_stored_bot_id(""), None);
        assert_eq!(parse_stored_bot_id("12.5"), None);
        assert_eq!(parse_stored_bot_id("not-a-number"), None);
    }

    /// 全局唯一的敏感字符串 `Debug` 形态（凭据纪律第 1 条的回归防线）。
    #[test]
    fn sensitive_refuses_to_print_its_plaintext() {
        let sensitive = Sensitive::new("123456:DO-NOT-LOG");
        assert_eq!(format!("{sensitive:?}"), "Sensitive(<redacted>)");
        assert_eq!(sensitive.expose(), "123456:DO-NOT-LOG");
        assert!(!sensitive.is_empty());
        assert!(Sensitive::default().is_empty());
    }
}
