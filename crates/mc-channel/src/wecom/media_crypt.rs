//! `media_crypt.go`（102 行）的本地落点：**一把 `aeskey` 能解开什么**。
//!
//! - **写者**：M7-18（`LUM-1783` / `docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §35 的 D1）。
//! - **上游定位**（文件头逐字）：带图片 / 文件 / 视频的智能机器人消息给 bot **两个字符串**
//!   —— 一个五分钟有效的腾讯 COS URL，以及字节被它加密的那把密钥。长连接模式**每条 URL
//!   现铸一把**密钥，这正是与回调模式那把"部署级 `EncodingAESKey`"的区别：没有可配置的东西、
//!   也没有可轮换的东西，但反过来，帧上那把密钥不可用时**也没有退路**。
//! - 算法：**AES-256-CBC**，IV 取**密钥自身的前 16 字节**，明文 PKCS#7 补齐到 **32 字节**的
//!   倍数（`developer.work.weixin.qq.com/document/path/101463`）。
//!
//! # 那两个"块"不是同一个数（本文件的陷阱）
//!
//! AES 工作在 **16** 字节块上，而上游把明文补齐到 **32** 字节的倍数。所以一个按 AES 块大小
//! 写的 PKCS#7 反填充器会拒掉任何长度大于 16 的填充 —— 而长度本来就是 32 倍数的文件会被
//! 补上**整整一个 32 字节块**。普通载荷撞上这一格的频率足够高：这样的反填充器在冒烟用例里
//! 看着没问题，在生产里翻车。上游把这一条写进了常量注释，本文件用 [`MEDIA_PAD_BLOCK`]
//! 与 [`AES_BLOCK_BYTES`] 两个**不同**的常量逐字复刻，并有用例专门钉住 `pad == 32` 那一格。
//!
//! # 🔴 为什么本文件自带一份 AES 实现（登记为 `docs/32` §35 的 D2）
//!
//! `crates/mc-channel/Cargo.toml` 的依赖面在 M7-0 anchor **一次定死**，注释逐字写着
//! 「此后 M7 各切片**不得**再新增三方依赖（要加走 `docs/15` §8.4 仲裁，由集成方加）」。
//! 本片需要的两个 crate 都在那条线之外：
//!
//! | crate | 在 `Cargo.lock` 里？ | 结论 |
//! | --- | :-: | --- |
//! | `aes` 0.8.4 | **在**（`aes-gcm` 的传递依赖） | 但本 crate 与它**没有直连边**，加边要改两处 manifest |
//! | `cbc` 0.1 | **不在** | 加它 = **新外部包** |
//!
//! ⇒ 按本仓既有的判例（`docs/32` 的 **M8-2-D7**「手写公历算法」、**M7-15-D5**「没有 `rand`
//! ⇒ 两个 v4 UUID」、§?「没有 `regex` ⇒ 手写扫描器」），**写集内的手写**是这条冻结下的既有
//! 做法。本文件因此自带 AES-256（密钥扩展 + 正/逆轮函数）与 CBC 链接，而**正确性不靠嘴说**：
//!
//! - **S 盒不是抄来的 256 字节**：它由 GF(2⁸) 上的乘法逆元 + 仿射变换在 **`const fn` 里算出来**
//!   （[`SBOX`] / [`INV_SBOX`]）⇒ 没有抄错一格的余地；
//! - **NIST 已知答案向量钉住**：FIPS-197 附录 C.3 的 AES-256 单块向量（`media_crypt/tests.rs`）
//!   与 NIST SP 800-38A §F.2.5/F.2.6 的 CBC-AES256 向量，逐字比字节。
//!
//! ## 这份实现的**收缩**（诚实交代，同样登记）
//!
//! 1. **表驱动 ⇒ 不是常量时间**：S 盒查表的缓存时序侧信道在这里没有实际暴露面 ——
//!    被解密的字节来自**平台**（不是攻击者），密钥是**一次性**的（每条 URL 现铸、用在一条
//!    附件上），而唯一的"攻击者可控输入"是 URL，它由 [`super::media_guard`] 的地址闸把守。
//!    真正需要常量时间的用法（长期密钥的解封）**不**走本文件，走 `mc_secrets::secretbox`。
//! 2. **加密方向是给用例造的**（[`MediaAesKey::encrypt`]）：上游 `media_crypt.go` 只有解密
//!    （平台加密、我们解密），而 `DoD` 要求"加解密**往返** + 篡改反例" ⇒ 加密只出现在本文件
//!    与用例里，**生产路径不调用它**（模块尾部的 `#[cfg(test)]` 之外没有调用点）。
//!
//! # 凭据面（DoD 第 6 条）
//!
//! `aeskey` 是一把**能解锁附件字节的密钥** ⇒ 它只以 [`MediaAesKey`] 的形式在类型里流动，
//! 该类型**手写 `Debug`**（`<redacted, 32 bytes>`），[`MediaCryptError`] 的每个变体
//! **只带长度**、不带密钥或密文的任何一段（`docs/60` §2.3 的三条判据，逐条有用例）。

use std::fmt;

/// 解码后的密钥长度（上游 `mediaAESKeyBytes`）：AES-256 要的字节数。
pub const MEDIA_AES_KEY_BYTES: usize = 32;

/// 明文补齐到的块大小（上游 `mediaPadBlock`）。**不是** AES 的块大小 —— 见模块文档。
pub const MEDIA_PAD_BLOCK: usize = 32;

/// AES 自己的块大小（上游直接用 `aes.BlockSize`）。
pub const AES_BLOCK_BYTES: usize = 16;

/// 一次解密失败。每个变体**只带长度**：密钥与密文的任何一段都不进错误路径。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaCryptError {
    /// 上游 `errors.New("wecom: media aeskey is empty")`。
    #[error("wecom: media aeskey is empty")]
    KeyEmpty,
    /// 上游 `fmt.Errorf("wecom: media aeskey does not decode to %d bytes", mediaAESKeyBytes)`。
    ///
    /// 四种 base64 变体**全试过**才落到这里；密钥本身不进消息（上游也不进）。
    #[error("wecom: media aeskey does not decode to {MEDIA_AES_KEY_BYTES} bytes")]
    KeyNotDecodable,
    /// 上游 `errors.New("wecom: media ciphertext is empty")`。
    #[error("wecom: media ciphertext is empty")]
    CiphertextEmpty,
    /// 上游 `fmt.Errorf("wecom: media ciphertext is %d bytes, not a multiple of the %d-byte AES block", …)`。
    #[error("wecom: media ciphertext is {len} bytes, not a multiple of the {AES_BLOCK_BYTES}-byte AES block")]
    CiphertextNotBlockAligned { len: usize },
    /// 上游 `fmt.Errorf("wecom: media plaintext is empty")`。
    #[error("wecom: media plaintext is empty")]
    PlaintextEmpty,
    /// 上游 `fmt.Errorf("wecom: media padding length %d is out of range (1..%d)", pad, mediaPadBlock)`。
    #[error("wecom: media padding length {pad} is out of range (1..={MEDIA_PAD_BLOCK})")]
    PaddingOutOfRange { pad: usize },
    /// 上游 `errors.New("wecom: media padding bytes disagree; wrong key or truncated body")`。
    #[error("wecom: media padding bytes disagree; wrong key or truncated body")]
    PaddingDisagrees,
}

/// 一把解码好的 `aeskey`。
///
/// **手写 `Debug`**：密钥字节绝不进日志 / `tracing` 插值 / panic backtrace（`docs/60` §2.3 判据 1）。
pub struct MediaAesKey {
    bytes: [u8; MEDIA_AES_KEY_BYTES],
}

impl fmt::Debug for MediaAesKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted, 32 bytes>")
    }
}

impl MediaAesKey {
    /// 由**已经解码好的** 32 字节建一把（用例与流式路径用；wire 上的是 [`Self::decode`]）。
    #[must_use]
    pub fn from_bytes(bytes: [u8; MEDIA_AES_KEY_BYTES]) -> Self {
        Self { bytes }
    }

    /// 上游 `decodeMediaAESKey`：解 base64，**恰好 32 字节**才算数。
    ///
    /// 填充的 44 字符形式与不填充的 43 字符形式在腾讯自己的界面里**都出现**，所以四种变体
    /// （`StdEncoding` / `RawStdEncoding` / `URLEncoding` / `RawURLEncoding`）全试；解出来
    /// 不是正好 32 字节的**一律拒**，而不是补齐或截断成 32。
    ///
    /// # Errors
    ///
    /// [`MediaCryptError::KeyEmpty`] / [`MediaCryptError::KeyNotDecodable`]。
    pub fn decode(raw: &str) -> Result<Self, MediaCryptError> {
        use base64::Engine as _;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(MediaCryptError::KeyEmpty);
        }
        for engine in [
            &base64::engine::general_purpose::STANDARD,
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            &base64::engine::general_purpose::URL_SAFE,
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ] {
            if let Ok(decoded) = engine.decode(trimmed) {
                if decoded.len() == MEDIA_AES_KEY_BYTES {
                    let mut bytes = [0u8; MEDIA_AES_KEY_BYTES];
                    bytes.copy_from_slice(&decoded);
                    return Ok(Self { bytes });
                }
            }
        }
        Err(MediaCryptError::KeyNotDecodable)
    }

    /// AES-CBC 的 IV：**密钥自身的前 16 字节**。
    ///
    /// 把密钥材料当 IV 用是 `WeCom` 的选择、不是我们的；我们只需要与它一致。
    #[must_use]
    pub fn iv(&self) -> [u8; AES_BLOCK_BYTES] {
        let mut iv = [0u8; AES_BLOCK_BYTES];
        iv.copy_from_slice(&self.bytes[..AES_BLOCK_BYTES]);
        iv
    }

    /// 加密方向（**只为用例造出可解密的密文**，见模块文档的收缩 2）。
    #[must_use]
    pub fn encrypt(&self, plain: &[u8]) -> Vec<u8> {
        let padded = pad_media(plain);
        cbc_encrypt(&self.bytes, &self.iv(), &padded)
    }

    /// 上游 `decryptMedia`：把一个下载回来的 body 还原成用户发出的那个文件。
    ///
    /// # Errors
    ///
    /// [`MediaCryptError`]。**每一次失败都是错误、不是一个最好的猜测**（上游逐字）：这里
    /// 猜错的后果是一个打不开的附件，比一个诚实地缺失的附件更糟。
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, MediaCryptError> {
        if ciphertext.is_empty() {
            return Err(MediaCryptError::CiphertextEmpty);
        }
        if !ciphertext.len().is_multiple_of(AES_BLOCK_BYTES) {
            return Err(MediaCryptError::CiphertextNotBlockAligned {
                len: ciphertext.len(),
            });
        }
        let plain = cbc_decrypt(&self.bytes, &self.iv(), ciphertext);
        unpad_media(&plain)
    }
}

/// **流式** CBC 解密器（上游 `media_stream.go` 里那个 `cipher.NewCBCDecrypter` 的状态机形态）。
///
/// 存在的理由：流式路径按块喂，而 CBC 的每块要拿**上一块密文**当 IV —— 一个"每次调用都用
/// 密钥前 16 字节当 IV"的逐块函数在第二块上就错了。状态在这里，链条就不会断。
pub struct MediaCbcDecryptor {
    key: [u8; MEDIA_AES_KEY_BYTES],
    chain: [u8; AES_BLOCK_BYTES],
}

impl MediaCbcDecryptor {
    /// 用一把密钥开一条链（IV = 密钥前 16 字节）。
    #[must_use]
    pub fn new(key: &MediaAesKey) -> Self {
        Self {
            key: key.bytes,
            chain: key.iv(),
        }
    }

    /// 就地把整块密文解成明文。`ciphertext.len()` 必须是 [`AES_BLOCK_BYTES`] 的倍数
    /// （调用方负责切块；流式路径的 `carry` 保证这一点）。
    pub fn decrypt_blocks(&mut self, ciphertext: &mut [u8]) {
        for chunk in ciphertext.as_chunks_mut::<AES_BLOCK_BYTES>().0 {
            // 先把这一块密文抄下来：它既是"要解的那一块"，也是**下一块**的 IV。
            let mut block = [0u8; AES_BLOCK_BYTES];
            block.copy_from_slice(chunk);
            let plain = decrypt_block(&self.key, &block);
            for (index, byte) in chunk.iter_mut().enumerate() {
                *byte = plain[index] ^ self.chain[index];
            }
            self.chain = block;
        }
    }
}

/// 上游 `decryptMedia(aesKey string, ciphertext []byte)`：字符串形态的入口。
///
/// # Errors
///
/// [`MediaCryptError`]。
pub fn decrypt_media(raw_key: &str, ciphertext: &[u8]) -> Result<Vec<u8>, MediaCryptError> {
    MediaAesKey::decode(raw_key)?.decrypt(ciphertext)
}

/// 上游 `decodeMediaAESKey` 的裸字节版本（只有"要一把密钥"的调用方用得上）。
///
/// # Errors
///
/// [`MediaCryptError::KeyEmpty`] / [`MediaCryptError::KeyNotDecodable`]。
pub fn decode_media_aes_key(raw: &str) -> Result<MediaAesKey, MediaCryptError> {
    MediaAesKey::decode(raw)
}

/// 上游 `unpadMedia`：剥掉 PKCS#7 尾巴，并**逐字节校验**它。
///
/// 尾巴对不上就是"密钥错了"或"body 被截断了"，两种情况下它前面的字节都不能被当成文件。
///
/// # Errors
///
/// [`MediaCryptError::PlaintextEmpty`] / [`MediaCryptError::PaddingOutOfRange`] /
/// [`MediaCryptError::PaddingDisagrees`]。
pub fn unpad_media(plain: &[u8]) -> Result<Vec<u8>, MediaCryptError> {
    let len = plain.len();
    if len == 0 {
        return Err(MediaCryptError::PlaintextEmpty);
    }
    let pad = usize::from(plain[len - 1]);
    // 上界是 `mediaPadBlock`（32）而不是 AES 块大小（16）—— 见模块文档的陷阱一节。
    if !(1..=MEDIA_PAD_BLOCK).contains(&pad) || pad > len {
        return Err(MediaCryptError::PaddingOutOfRange { pad });
    }
    for byte in &plain[len - pad..] {
        if usize::from(*byte) != pad {
            return Err(MediaCryptError::PaddingDisagrees);
        }
    }
    Ok(plain[..len - pad].to_vec())
}

/// 上游 `pkcs7Pad` 的等价物（上游在**平台侧**补齐；本文件只在加密方向需要它）。
#[must_use]
pub fn pad_media(plain: &[u8]) -> Vec<u8> {
    let pad = MEDIA_PAD_BLOCK - (plain.len() % MEDIA_PAD_BLOCK);
    // `pad` 落在 1..=32：长度本来就是 32 倍数时补**整整一个块**（上游注释点名的这一格）。
    let pad = if pad == 0 { MEDIA_PAD_BLOCK } else { pad };
    let mut out = Vec::with_capacity(plain.len() + pad);
    out.extend_from_slice(plain);
    #[allow(clippy::cast_possible_truncation)] // pad ∈ 1..=32
    out.extend(std::iter::repeat_n(pad as u8, pad));
    out
}

// =====================================================================
// AES-256（FIPS-197）+ CBC
// =====================================================================

/// GF(2⁸) 上的乘法（`xtime` 迭代版；`const fn` 才能在编译期算出 S 盒）。
const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0u8;
    let mut i = 0;
    while i < 8 {
        if b & 1 != 0 {
            product ^= a;
        }
        let high = a & 0x80;
        a <<= 1;
        if high != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
        i += 1;
    }
    product
}

/// GF(2⁸) 上的乘法逆元（0 的逆元按定义取 0）。暴力搜索，只在编译期跑 256×256 次。
const fn gf_inv(a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    let mut candidate = 1u16;
    while candidate < 256 {
        #[allow(clippy::cast_possible_truncation)]
        if gf_mul(a, candidate as u8) == 1 {
            return candidate as u8;
        }
        candidate += 1;
    }
    0
}

/// 左循环移位（仿射变换用）。
const fn rotl8(value: u8, shift: u32) -> u8 {
    value.rotate_left(shift)
}

/// FIPS-197 的仿射变换：`b = x ⊕ rotl(x,1) ⊕ rotl(x,2) ⊕ rotl(x,3) ⊕ rotl(x,4) ⊕ 0x63`。
const fn affine(x: u8) -> u8 {
    x ^ rotl8(x, 1) ^ rotl8(x, 2) ^ rotl8(x, 3) ^ rotl8(x, 4) ^ 0x63
}

/// S 盒：**算出来的**，不是抄来的（4.2.1 的合成域定义：逆元 + 仿射）。
const fn build_sbox() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        #[allow(clippy::cast_possible_truncation)]
        {
            table[i] = affine(gf_inv(i as u8));
        }
        i += 1;
    }
    table
}

/// 逆 S 盒：由 [`SBOX`] 反查（4.2.2 的 `SBOX⁻¹`）。
const fn build_inv_sbox(sbox: &[u8; 256]) -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        #[allow(clippy::cast_possible_truncation)]
        {
            table[sbox[i] as usize] = i as u8;
        }
        i += 1;
    }
    table
}

/// RCON 表（`01,02,04,08,10,20,40` —— AES-256 的 `i/Nk` 只走到 7）。
const RCON: [u8; 8] = [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40];

const SBOX: [u8; 256] = build_sbox();
const INV_SBOX: [u8; 256] = build_inv_sbox(&SBOX);

/// 轮数（AES-256）。
const ROUNDS: usize = 14;

/// 一个 4×4 的状态（列主序：`state[r + 4c]`）。
type State = [u8; 16];

/// 密钥扩展（5.2）：`w[0..60]`。
fn expand_key(key: &[u8; MEDIA_AES_KEY_BYTES]) -> [[u8; 4]; 4 * (ROUNDS + 1)] {
    const NK: usize = 8;
    let mut w = [[0u8; 4]; 4 * (ROUNDS + 1)];
    for (index, word) in w.iter_mut().take(NK).enumerate() {
        word.copy_from_slice(&key[index * 4..index * 4 + 4]);
    }
    for index in NK..w.len() {
        let mut temp = w[index - 1];
        if index % NK == 0 {
            temp = [
                SBOX[usize::from(temp[1])],
                SBOX[usize::from(temp[2])],
                SBOX[usize::from(temp[3])],
                SBOX[usize::from(temp[0])],
            ];
            temp[0] ^= RCON[index / NK];
        } else if index % NK == 4 {
            temp = [
                SBOX[usize::from(temp[0])],
                SBOX[usize::from(temp[1])],
                SBOX[usize::from(temp[2])],
                SBOX[usize::from(temp[3])],
            ];
        }
        for (byte, previous) in temp.iter_mut().zip(w[index - NK]) {
            *byte ^= previous;
        }
        w[index] = temp;
    }
    w
}

/// 轮密钥 → 状态（`AddRoundKey`）。
fn add_round_key(state: &mut State, round_key: &[[u8; 4]], round: usize) {
    for column in 0..4 {
        for row in 0..4 {
            state[row + 4 * column] ^= round_key[4 * round + column][row];
        }
    }
}

/// 行循环左移（4.2.1 的 `ShiftRows`），第 r 行移 r 格。
fn shift_rows(state: &mut State) {
    for row in 1..4 {
        let mut row_bytes = [0u8; 4];
        for (column, byte) in row_bytes.iter_mut().enumerate() {
            *byte = state[row + 4 * column];
        }
        row_bytes.rotate_left(row);
        for (column, byte) in row_bytes.iter().enumerate() {
            state[row + 4 * column] = *byte;
        }
    }
}

/// 行循环右移（4.2.2 的 `InvShiftRows`）—— 与 [`shift_rows`] 方向相反、格数相同。
fn inv_shift_rows(state: &mut State) {
    for row in 1..4 {
        let mut row_bytes = [0u8; 4];
        for (column, byte) in row_bytes.iter_mut().enumerate() {
            *byte = state[row + 4 * column];
        }
        row_bytes.rotate_right(row);
        for (column, byte) in row_bytes.iter().enumerate() {
            state[row + 4 * column] = *byte;
        }
    }
}

/// 每一列过 `MixColumns`（4.2.1，模 `x⁴+1` 的多项式乘）。
fn mix_columns(state: &mut State) {
    for column in 0..4 {
        let base = 4 * column;
        let (a0, a1, a2, a3) = (
            state[base],
            state[base + 1],
            state[base + 2],
            state[base + 3],
        );
        state[base] = gf_mul(a0, 2) ^ gf_mul(a1, 3) ^ a2 ^ a3;
        state[base + 1] = a0 ^ gf_mul(a1, 2) ^ gf_mul(a2, 3) ^ a3;
        state[base + 2] = a0 ^ a1 ^ gf_mul(a2, 2) ^ gf_mul(a3, 3);
        state[base + 3] = gf_mul(a0, 3) ^ a1 ^ a2 ^ gf_mul(a3, 2);
    }
}

/// `InvMixColumns`（4.2.2 的等价逆变换，系数 `14,11,13,9`）。
fn inv_mix_columns(state: &mut State) {
    for column in 0..4 {
        let base = 4 * column;
        let (a0, a1, a2, a3) = (
            state[base],
            state[base + 1],
            state[base + 2],
            state[base + 3],
        );
        state[base] = gf_mul(a0, 14) ^ gf_mul(a1, 11) ^ gf_mul(a2, 13) ^ gf_mul(a3, 9);
        state[base + 1] = gf_mul(a0, 9) ^ gf_mul(a1, 14) ^ gf_mul(a2, 11) ^ gf_mul(a3, 13);
        state[base + 2] = gf_mul(a0, 13) ^ gf_mul(a1, 9) ^ gf_mul(a2, 14) ^ gf_mul(a3, 11);
        state[base + 3] = gf_mul(a0, 11) ^ gf_mul(a1, 13) ^ gf_mul(a2, 9) ^ gf_mul(a3, 14);
    }
}

/// 一个块的正向加密（5.1 的 `Cipher`）。
fn encrypt_block(key: &[u8; MEDIA_AES_KEY_BYTES], block: &[u8; AES_BLOCK_BYTES]) -> State {
    let round_keys = expand_key(key);
    let mut state: State = *block;
    add_round_key(&mut state, &round_keys, 0);
    for round in 1..ROUNDS {
        for byte in &mut state {
            *byte = SBOX[usize::from(*byte)];
        }
        shift_rows(&mut state);
        mix_columns(&mut state);
        add_round_key(&mut state, &round_keys, round);
    }
    for byte in &mut state {
        *byte = SBOX[usize::from(*byte)];
    }
    shift_rows(&mut state);
    add_round_key(&mut state, &round_keys, ROUNDS);
    state
}

/// 一个块的解密（5.3 的 `InvCipher`，直接形态而不是等价逆密码）。
fn decrypt_block(key: &[u8; MEDIA_AES_KEY_BYTES], block: &[u8; AES_BLOCK_BYTES]) -> State {
    let round_keys = expand_key(key);
    let mut state: State = *block;
    add_round_key(&mut state, &round_keys, ROUNDS);
    for round in (1..ROUNDS).rev() {
        inv_shift_rows(&mut state);
        for byte in &mut state {
            *byte = INV_SBOX[usize::from(*byte)];
        }
        add_round_key(&mut state, &round_keys, round);
        inv_mix_columns(&mut state);
    }
    inv_shift_rows(&mut state);
    for byte in &mut state {
        *byte = INV_SBOX[usize::from(*byte)];
    }
    add_round_key(&mut state, &round_keys, 0);
    state
}

/// CBC 解密（`cipher.NewCBCDecrypter` 的等价物）。调用方保证 `ciphertext` 是块对齐的。
fn cbc_decrypt(key: &[u8; MEDIA_AES_KEY_BYTES], iv: &[u8; AES_BLOCK_BYTES], ct: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ct.len());
    let mut previous = *iv;
    for chunk in ct.as_chunks::<AES_BLOCK_BYTES>().0 {
        let mut block = [0u8; AES_BLOCK_BYTES];
        block.copy_from_slice(chunk);
        let plain = decrypt_block(key, &block);
        for (byte, mask) in plain.iter().zip(previous) {
            out.push(*byte ^ mask);
        }
        previous = block;
    }
    out
}

/// CBC 加密（只在加密方向用；见模块文档的收缩 2）。
fn cbc_encrypt(
    key: &[u8; MEDIA_AES_KEY_BYTES],
    iv: &[u8; AES_BLOCK_BYTES],
    plain: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(plain.len());
    let mut previous = *iv;
    for chunk in plain.as_chunks::<AES_BLOCK_BYTES>().0 {
        let mut block = [0u8; AES_BLOCK_BYTES];
        for (byte, (value, mask)) in block.iter_mut().zip(chunk.iter().zip(previous)) {
            *byte = *value ^ mask;
        }
        let encrypted = encrypt_block(key, &block);
        out.extend_from_slice(&encrypted);
        previous = encrypted;
    }
    out
}

#[cfg(test)]
mod tests;
