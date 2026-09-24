//! `secretbox`：渠道部署密钥的**封装盒**（AES-256-GCM，逐字复刻上游
//! `server/internal/util/secretbox/secretbox.go`）。
//!
//! ## 为什么在 `mc-secrets` 另开一个模块（而不是复用 `cipher.rs`）
//!
//! | | `mc_secrets::cipher`（已有） | `mc_secrets::secretbox`（本模块） |
//! | --- | --- | --- |
//! | 出处 | better-auth 兼容层 | 上游 `internal/util/secretbox` |
//! | 线格式 | **JSON payload**：`{"nonce": base64, "ciphertext": base64}` | **单块字节**：`nonce(12) ‖ ciphertext ‖ tag(16)` |
//! | 落点 | `account.password` 一类文本列 | `BYTEA` 列（`channel_installation.config.app_secret_encrypted`） |
//!
//! 两者**不兼容**（JSON 里没有 tag 字段、密文只能整体 base64），所以渠道面不能复用
//! `cipher`。`cipher.rs` 本片**不动**。
//!
//! ## 密钥的来源（**唯一读取口**）
//!
//! 环境变量本身在 `mc-http::state::ChannelKeys` 里读（M7-0 anchor 落的配置入口，
//! 照 `PluginSecretKey::from_env` 先例）：`mc-channel` 与 route 层**不得**各自
//! `std::env::var`，否则「配置入口」会有两处、测试也无法注入。[`load_key`] /
//! [`load_key_with`] 是这条纪律的**实现**，只被那个入口调用。
//!
//! ## 上游口径（逐条对齐，别"顺手放宽"）
//!
//! 1. key **恰好 32 字节**（AES-256），env 值是 **base64（`StdEncoding`，带填充）**；
//! 2. **不做 trim**：上游只在 `raw == ""` 时判空，`" <b64> "` 会走到 base64 解码被拒
//!    —— 本地加 trim 就是放行上游拒绝的输入（**拓宽**而不是照搬）；
//! 3. 未设置 / 空串 / 非法 base64 / 长度不对 ⇒ 一律**当作未配置**（`None`）：
//!    **绝不**用零密钥兜底、**绝不** panic；
//! 4. `seal` 的 nonce **每次随机**，同一条明文两次封装的输出不同（防止拿密文当指纹）；
//! 5. 不做密钥轮换（上游注释：等生产真的要多把 key 时，再在密文前加一字节 key id）。
//!
//! ## 凭据纪律（`docs/60` §2.3 / 本片 `DoD` 第 6 条）
//!
//! - [`SecretBox`] **手写 `Debug`**：只打印 `<redacted, 32 bytes>`，密钥字节**绝不**进
//!   日志 / `tracing` 插值 / panic backtrace；
//! - 所有错误变体**只带长度、不带载荷**（[`SecretBoxError`]），错误路径**不回显**
//!   明文、密文或密钥 —— 有专门用例钉住。
//!
//! ## 已知重复（登记为偏离，**不合并**）
//!
//! `mc-plugin-host::credentials::SecretBox`（M6-1）是同一算法的另一份实现，方位是
//! `mc-plugin-host → mc-secrets`，所以本模块**不能**复用它（会成环），反过来它也不必
//! 复用本模块（M6 面已合、冻结）。收敛票与逐条理由见 `docs/32` §10（R-M7-4）。

use std::fmt;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use rand::RngCore;

/// 主密钥字节数（上游 `secretbox.KeySize`，AES-256）。
pub const KEY_SIZE: usize = 32;

/// GCM nonce 的字节数（上游 `cipher.NewGCM` 的默认 nonce 长）。
pub const NONCE_SIZE: usize = 12;

/// GCM tag 的字节数。
pub const TAG_SIZE: usize = 16;

/// 一条封装块的**最短**长度：`nonce(12) + tag(16)`（上游 `ErrCiphertextTooShort` 的判据）。
pub const MIN_SEALED_LEN: usize = NONCE_SIZE + TAG_SIZE;

/// `secretbox` 的错误。
///
/// **逐字对齐上游三个哨兵**（`ErrInvalidKey` / `ErrCiphertextTooShort` / GCM 的认证错误），
/// 并额外把熵源失败单列（上游用 `fmt.Errorf` 包 `rand.Read`）。
///
/// 每个变体**只带长度**：明文、密文、密钥字节都不进错误值 —— 这是「错误路径不回显凭据」
/// 的结构性保证（比"注意别打日志"可靠）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretBoxError {
    /// 密钥长度不是 [`KEY_SIZE`]（上游 `secretbox.ErrInvalidKey`）。
    #[error("secretbox: key must be 32 bytes, got {len}")]
    InvalidKey { len: usize },
    /// 密文短于 [`MIN_SEALED_LEN`]（上游 `secretbox.ErrCiphertextTooShort`）。
    #[error("secretbox: ciphertext too short, got {len} bytes (need at least {MIN_SEALED_LEN})")]
    CiphertextTooShort { len: usize },
    /// GCM 认证失败：被改过、换过密钥、或 nonce 不对。上游把 GCM 的错原样返回（无可读文案）。
    #[error("secretbox: ciphertext failed authentication")]
    Authentication,
    /// 熵源取 nonce 失败（上游包了 `rand.Read` 的错）。
    #[error("secretbox: read nonce failed")]
    NonceUnavailable,
}

/// AES-256-GCM 封装盒（上游 `secretbox.Box`）。
///
/// 构造一次、进程内复用：上游注释写明「每请求重建会白白重推 AES 轮密钥」。
///
/// 密钥只以 AES 轮密钥形式存在于 [`Aes256Gcm`] 内部（与上游 `Box{aead}` 同形）：本模块
/// **不**额外持一份 `[u8; 32]`，也**不**做显式 `zeroize`（`zeroize` 在本 crate 里用于
/// `SecretValue` 一类堆上的明文，不用于轮密钥）。
#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl fmt::Debug for SecretBox {
    /// 手写脱敏实现（**不派生**）：密钥字节绝不能进日志。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBox(<redacted, 32 bytes>)")
    }
}

impl SecretBox {
    /// 用一把密钥建盒。长度必须是 [`KEY_SIZE`]。
    ///
    /// # Errors
    ///
    /// [`SecretBoxError::InvalidKey`]：长度不是 32。
    pub fn new(key: &[u8]) -> Result<Self, SecretBoxError> {
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|_| SecretBoxError::InvalidKey { len: key.len() })?;
        Ok(Self { cipher })
    }

    /// 用**指定 nonce**封一条明文：`nonce(12) ‖ ciphertext ‖ tag(16)`（上游 `Box.Seal` 的排布）。
    ///
    /// 生产路径用 [`SecretBox::seal`]（随机 nonce）；本函数存在的理由是**可复现向量**
    /// （与 python / Go 逐字节比对）与需要确定性输出的调用方。
    ///
    /// # Errors
    ///
    /// [`SecretBoxError::Authentication`]（GCM 因长度溢出拒绝封装的唯一情形）。
    pub fn seal_with_nonce(
        &self,
        plaintext: &[u8],
        nonce: &[u8; NONCE_SIZE],
    ) -> Result<Vec<u8>, SecretBoxError> {
        let sealed = self
            .cipher
            .encrypt(Nonce::from_slice(nonce), plaintext)
            .map_err(|_| SecretBoxError::Authentication)?;
        let mut out = Vec::with_capacity(NONCE_SIZE + sealed.len());
        out.extend_from_slice(nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// 封一条明文，nonce **每次随机**（上游 `Box.Seal` 的默认行为）。
    ///
    /// 同一明文两次封装的输出**不同**：不要把密文当索引或指纹。
    ///
    /// # Errors
    ///
    /// [`SecretBoxError::NonceUnavailable`] / [`SecretBoxError::Authentication`]。
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SecretBoxError> {
        let mut nonce = [0u8; NONCE_SIZE];
        rand::thread_rng()
            .try_fill_bytes(&mut nonce)
            .map_err(|_| SecretBoxError::NonceUnavailable)?;
        self.seal_with_nonce(plaintext, &nonce)
    }

    /// 解一条封装块（`nonce ‖ ciphertext ‖ tag`，上游 `Box.Open`）。
    ///
    /// # Errors
    ///
    /// [`SecretBoxError::CiphertextTooShort`]（短于 [`MIN_SEALED_LEN`]）/
    /// [`SecretBoxError::Authentication`]（被改过 / 换过密钥）。
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretBoxError> {
        if sealed.len() < MIN_SEALED_LEN {
            return Err(SecretBoxError::CiphertextTooShort { len: sealed.len() });
        }
        let (nonce, ciphertext) = sealed.split_at(NONCE_SIZE);
        self.cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| SecretBoxError::Authentication)
    }
}

/// 把 base64（`StdEncoding`，**不 trim**）解成 32 字节密钥。
///
/// 空串 / 非法 base64 / 长度不对 ⇒ `None`（未配置）。
#[must_use]
pub fn decode_key(raw: &str) -> Option<[u8; KEY_SIZE]> {
    if raw.is_empty() {
        // 上游：`if raw == "" { return error("… is not set") }` —— 空串是"未配置"，不是"坏值"。
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
    decoded.try_into().ok()
}

/// 从**进程环境**读部署密钥（生产入口：`mc-http::state::ChannelKeys`）。
///
/// 未设置 / 空串 / 非法 base64 / 长度不对 ⇒ `None`。
#[must_use]
pub fn load_key(env_var: &str) -> Option<SecretBox> {
    load_key_with(env_var, |name| std::env::var(name).ok())
}

/// 从任意「名字 → 值」查询函数读部署密钥 —— 与 `PluginSecretKey::from_env_with` 同款，
/// 让映射本身能在不碰进程全局 env 的情况下被单测。
///
/// 未设置 / 空串 / 非法 base64 / 长度不对 ⇒ `None`。
#[must_use]
pub fn load_key_with<F>(env_var: &str, get: F) -> Option<SecretBox>
where
    F: Fn(&str) -> Option<String>,
{
    let raw = get(env_var)?;
    SecretBox::new(&decode_key(&raw)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定密钥：`0x00..0x1f`（base64 见 `load_key_rejects_empty_and_wrong_length`）。
    const KEY_BYTES: [u8; KEY_SIZE] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    fn boxed() -> SecretBox {
        SecretBox::new(&KEY_BYTES).expect("32 字节密钥")
    }

    /// `SealKey = 0x00..0x1f` 的 base64（`StdEncoding`）。
    const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    /// 向量 1：`seal → open` 往返（含与 python `cryptography` AESGCM 的**交叉实现**比对）。
    ///
    /// python：`AESGCM(bytes(range(32))).encrypt(bytes(range(12)), b"lark app_secret 12345", None)`
    /// ⇒ 块首 12 字节是 nonce，其余是 `ciphertext ‖ tag`。
    #[test]
    fn seal_open_round_trip_matches_python_vector() {
        // key = 0x00..0x1f，nonce = 0x00..0x0b，明文 `lark app_secret 12345`。
        let vector = hex::decode(concat!(
            "000102030405060708090a0b",
            "2b63a470e584b26bd232f2e8c38c0c4db2e4b400c5b69cb093268a84e6c302765e3cbe304b"
        ))
        .expect("hex");

        let boxed = boxed();
        // 正向：python 产的块能被我们解开。
        assert_eq!(
            boxed.open(&vector).expect("open"),
            b"lark app_secret 12345".to_vec()
        );
        // 反向：我们产的块逐字节等于 python 的（nonce 走 `seal_with_nonce` 才可复现）。
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&vector[..NONCE_SIZE]);
        assert_eq!(
            boxed
                .seal_with_nonce(b"lark app_secret 12345", &nonce)
                .expect("seal"),
            vector
        );

        // `seal`（随机 nonce）同样往返，且块长 = nonce + 明文 + tag。
        let sealed = boxed.seal(b"hello channel secret").expect("seal");
        assert_eq!(sealed.len(), MIN_SEALED_LEN + b"hello channel secret".len());
        assert_eq!(
            boxed.open(&sealed).expect("open"),
            b"hello channel secret".to_vec()
        );
        // nonce 随机 ⇒ 同一明文的两次封装不同（上游 `TestSealIsNonDeterministic`）。
        assert_ne!(
            boxed.seal(b"repeat").expect("seal"),
            boxed.seal(b"repeat").expect("seal")
        );
    }

    /// 向量 2：篡改任意一字节（含 tag 那一字节）必失败。
    #[test]
    fn open_rejects_tampered_byte() {
        let boxed = boxed();
        let sealed = boxed.seal(b"important").expect("seal");
        for index in [NONCE_SIZE, sealed.len() - 1] {
            let mut tampered = sealed.clone();
            tampered[index] ^= 0x01;
            assert_eq!(
                boxed.open(&tampered),
                Err(SecretBoxError::Authentication),
                "翻转第 {index} 个字节必须认证失败"
            );
        }
        // 换一把密钥也失败（密文不可跨密钥读）。
        let other = SecretBox::new(&[9u8; KEY_SIZE]).expect("box");
        assert_eq!(other.open(&sealed), Err(SecretBoxError::Authentication));
    }

    /// 向量 3：密文短于 `nonce(12) + tag(16)` 必失败（且**不** panic）。
    #[test]
    fn open_rejects_short_ciphertext() {
        let boxed = boxed();
        for len in 0..MIN_SEALED_LEN {
            assert_eq!(
                boxed.open(&vec![0u8; len]),
                Err(SecretBoxError::CiphertextTooShort { len }),
                "{len} 字节必须判短"
            );
        }
        // 恰好 28 字节不是"太短"，而是认证失败（GCM 解 0 长度的密文）。
        assert_eq!(
            boxed.open(&[0u8; MIN_SEALED_LEN]),
            Err(SecretBoxError::Authentication)
        );
    }

    /// 向量 4：`load_key` 拒空串与错长度（以及非法 base64、不做 trim）。
    #[test]
    fn load_key_rejects_empty_and_wrong_length() {
        let env = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.clone())
            }
        };
        let var = "MULTICA_LARK_SECRET_KEY";

        // 合法 base64 + 32 字节 ⇒ 建盒成功，且能解开自己的封装。
        let boxed = load_key_with(var, env(&[(var, KEY_B64)])).expect("合法密钥");
        assert_eq!(
            boxed.open(&boxed.seal(b"x").expect("seal")).expect("open"),
            b"x".to_vec()
        );

        // 未设置 / 空串 ⇒ 未配置（**不是**零密钥兜底）。
        assert!(load_key_with(var, env(&[])).is_none());
        assert!(load_key_with(var, env(&[(var, "")])).is_none());

        // 非法 base64 ⇒ 未配置（不是 panic）。
        assert!(load_key_with(var, env(&[(var, "not base64 !!")])).is_none());

        // 长度不对（31 / 33 字节）⇒ 未配置。
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; KEY_SIZE - 1]);
        let long = base64::engine::general_purpose::STANDARD.encode([0u8; KEY_SIZE + 1]);
        assert!(load_key_with(var, env(&[(var, &short)])).is_none());
        assert!(load_key_with(var, env(&[(var, &long)])).is_none());

        // **不做 trim**：上游只在 `raw == ""` 时判空，带空白的值会被 base64 解码拒掉。
        let padded = format!(" {KEY_B64} ");
        assert!(
            load_key_with(var, env(&[(var, &padded)])).is_none(),
            "加 trim 会放宽上游拒绝的输入"
        );

        // `decode_key` 与 `SecretBox::new` 的裸长度校验（上游 `New` 的 16 字节用例）。
        assert_eq!(decode_key(KEY_B64), Some(KEY_BYTES));
        assert_eq!(decode_key(""), None);
        assert_eq!(
            SecretBox::new(&[0u8; 16]).unwrap_err(),
            SecretBoxError::InvalidKey { len: 16 }
        );
    }

    /// 向量 5：手写 `Debug` 不含密钥字节（也不含 base64 形式）。
    #[test]
    fn debug_does_not_leak_key_bytes() {
        let boxed = boxed();
        let rendered = format!("{boxed:?}");
        assert_eq!(rendered, "SecretBox(<redacted, 32 bytes>)");
        assert!(!rendered.contains("AAECAwQ"), "不得回显 base64 密钥");
        assert!(!rendered.contains("1, 2, 3"), "不得回显密钥字节");
    }

    /// `DoD` 第 6 条：**错误路径不回显凭据**（明文 / 密文 / 密钥都不进错误文案）。
    #[test]
    fn error_paths_never_echo_credential_material() {
        let boxed = boxed();
        let plaintext = b"dingtalk-appsecret-DO-NOT-LOG";
        let sealed = boxed.seal(plaintext).expect("seal");

        // 认证失败：只报事实，不带任何载荷。
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let auth = boxed.open(&tampered).unwrap_err();
        assert_eq!(
            auth.to_string(),
            "secretbox: ciphertext failed authentication"
        );

        // 太短：只报长度。
        let short = boxed.open(&[0u8; 3]).unwrap_err();
        assert_eq!(
            short.to_string(),
            "secretbox: ciphertext too short, got 3 bytes (need at least 28)"
        );

        // 密钥长度错：只报长度。
        let bad_key = SecretBox::new(&[0u8; 7]).unwrap_err();
        assert_eq!(
            bad_key.to_string(),
            "secretbox: key must be 32 bytes, got 7"
        );

        // 三条错误文案都不含明文 / 密文 hex / 密钥材料。
        let ciphertext_hex = hex::encode(&sealed);
        for rendered in [
            auth.to_string(),
            short.to_string(),
            bad_key.to_string(),
            format!("{auth:?}"),
            format!("{bad_key:?}"),
        ] {
            assert!(
                !rendered.contains("DO-NOT-LOG"),
                "错误文案回显了明文：{rendered}"
            );
            assert!(
                !rendered.contains(&ciphertext_hex),
                "错误文案回显了密文：{rendered}"
            );
            assert!(
                !rendered.contains(KEY_B64),
                "错误文案回显了密钥：{rendered}"
            );
        }

        // `load_key_with` 的失败路径同样静默（`None`，不是带值错误）。
        let echoed = load_key_with("MULTICA_SLACK_SECRET_KEY", |_| {
            Some("slack-bot-token-xoxb-DO-NOT-LOG".to_string())
        });
        assert!(echoed.is_none());
    }
}
