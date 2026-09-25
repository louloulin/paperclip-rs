//! `media_crypt` 的用例：**NIST 已知答案向量**（正确性不靠嘴说）+ 填充陷阱 + 篡改反例 +
//! 凭据不回声。

use base64::Engine as _;
use pretty_assertions::assert_eq;

use super::*;

/// 十六进制字面量的块（用例里到处都是 `00 11 22 …`）。
fn block(hex_text: &str) -> [u8; AES_BLOCK_BYTES] {
    let raw = hex::decode(hex_text).expect("hex");
    let mut out = [0u8; AES_BLOCK_BYTES];
    out.copy_from_slice(&raw);
    out
}

/// 十六进制字面量的密钥。
fn key_bytes(hex_text: &str) -> [u8; MEDIA_AES_KEY_BYTES] {
    let raw = hex::decode(hex_text).expect("hex");
    let mut out = [0u8; MEDIA_AES_KEY_BYTES];
    out.copy_from_slice(&raw);
    out
}

/// 32 字节的 base64（`StdEncoding`，带填充）。
fn b64(key: &[u8; MEDIA_AES_KEY_BYTES]) -> String {
    base64::engine::general_purpose::STANDARD.encode(key)
}

// FIPS-197 附录 C.3 的 AES-256 密钥（也是 SP 800-38A 的向量用的那一把之前的常见向量）。
const FIPS_KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const FIPS_PLAIN: &str = "00112233445566778899aabbccddeeff";
const FIPS_CIPHER: &str = "8ea2b7ca516745bfeafc49904b496089";

// NIST SP 800-38A §F.2.5（CBC-AES256.Encrypt）的一条四块向量。
const CBC_KEY: &str = "603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4";
const CBC_IV: &str = "000102030405060708090a0b0c0d0e0f";
const CBC_PLAIN_BLOCKS: [&str; 4] = [
    "6bc1bee22e409f96e93d7e117393172a",
    "ae2d8a571e03ac9c9eb76fac45af8e51",
    "30c81c46a35ce411e5fbc1191a0a52ef",
    "f69f2445df4f9b17ad2b417be66c3710",
];
const CBC_CIPHER_BLOCKS: [&str; 4] = [
    "f58c4c04d6e5f1ba779eabfb5f7bfbd6",
    "9cfc4e967edb808d679f777bc6702c7d",
    "39f23369a9d9bacfa530e26304231461",
    "b2eb05e2c39be9fcda6c19078c6a9d1b",
];

// =====================================================================
// AES-256 本身
// =====================================================================

/// S 盒是**算出来**的（GF(2⁸) 逆元 + 仿射），所以这条用例是"公式没写错"的第一道证据。
#[test]
fn sbox_is_computed_from_its_definition() {
    // 逆元为自身的 {0x01} 与四个已知格子（FIPS-197 的 S 盒首行）。
    assert_eq!(SBOX[0x00], 0x63);
    assert_eq!(SBOX[0x01], 0x7c);
    assert_eq!(SBOX[0x53], 0xed);
    assert_eq!(SBOX[0xff], 0x16);
    // 逆表真的是逆表。
    for value in 0u16..256 {
        #[allow(clippy::cast_possible_truncation)]
        let index = usize::from(SBOX[value as usize]);
        assert_eq!(usize::from(INV_SBOX[index]), value as usize);
    }
}

/// FIPS-197 附录 C.3：单块正向加密。
#[test]
fn aes256_known_answer_vector_fips197_c3() {
    let key = key_bytes(FIPS_KEY);
    let plain = block(FIPS_PLAIN);
    let expected = block(FIPS_CIPHER);
    assert_eq!(encrypt_block(&key, &plain), expected);
}

/// 逆密码确实是正密码的逆（两向都测：解密那条向量 + 一个往返）。
#[test]
fn aes256_inverse_cipher_inverts() {
    let key = key_bytes(FIPS_KEY);
    let cipher = block(FIPS_CIPHER);
    assert_eq!(decrypt_block(&key, &cipher), block(FIPS_PLAIN));

    let other = key_bytes(CBC_KEY);
    for seed in 0u8..16 {
        let mut plain = [0u8; AES_BLOCK_BYTES];
        for (index, byte) in plain.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            {
                *byte = seed.wrapping_mul(17).wrapping_add(index as u8);
            }
        }
        let encrypted = encrypt_block(&other, &plain);
        assert_eq!(decrypt_block(&other, &encrypted), plain);
    }
}

/// NIST SP 800-38A §F.2.5 / §F.2.6：CBC-AES256 的四块向量（正反两向）。
#[test]
fn cbc_aes256_known_answer_vector_sp800_38a() {
    let key = key_bytes(CBC_KEY);
    let iv = block(CBC_IV);
    let mut plain = Vec::new();
    for chunk in CBC_PLAIN_BLOCKS {
        plain.extend_from_slice(&block(chunk));
    }
    let mut expected = Vec::new();
    for chunk in CBC_CIPHER_BLOCKS {
        expected.extend_from_slice(&block(chunk));
    }
    assert_eq!(cbc_encrypt(&key, &iv, &plain), expected);
    assert_eq!(cbc_decrypt(&key, &iv, &expected), plain);
}

// =====================================================================
// 密钥解码
// =====================================================================

/// 四种 base64 变体都要收（腾讯自己的界面上填充与不填充都出现过）。
#[test]
fn decode_accepts_all_four_base64_alphabets() {
    let key = key_bytes(FIPS_KEY);
    let padded = base64::engine::general_purpose::STANDARD.encode(key);
    let raw = base64::engine::general_purpose::STANDARD_NO_PAD.encode(key);
    assert_eq!(padded.len(), 44);
    assert_eq!(raw.len(), 43);
    for encoded in [padded, raw] {
        assert_eq!(
            MediaAesKey::decode(&encoded).expect("decode").bytes,
            key,
            "encoding {encoded}"
        );
    }
    // 用字节造一把**真的**需要 URL-safe 表的密钥（含 0xfb/0xff 这类会产出 `+/` 的字节）。
    let mut url_key = [0u8; MEDIA_AES_KEY_BYTES];
    for (index, byte) in url_key.iter_mut().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        {
            *byte = 0xf0u8.wrapping_add(index as u8);
        }
    }
    for encoded in [
        base64::engine::general_purpose::URL_SAFE.encode(url_key),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url_key),
    ] {
        assert_eq!(
            MediaAesKey::decode(&encoded).expect("decode").bytes,
            url_key,
            "encoding {encoded}"
        );
    }
}

/// 长度不对的一律拒，**不补齐也不截断**（上游逐字）。
#[test]
fn decode_refuses_anything_that_is_not_exactly_32_bytes() {
    for length in [1usize, 16, 31, 33, 64] {
        let mut short = [0u8; 64];
        for (index, byte) in short.iter_mut().enumerate().take(length) {
            #[allow(clippy::cast_possible_truncation)]
            {
                *byte = index as u8;
            }
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&short[..length]);
        assert_eq!(
            MediaAesKey::decode(&encoded).unwrap_err(),
            MediaCryptError::KeyNotDecodable,
            "length {length}"
        );
    }
    assert_eq!(
        MediaAesKey::decode("").unwrap_err(),
        MediaCryptError::KeyEmpty
    );
    assert_eq!(
        MediaAesKey::decode("   ").unwrap_err(),
        MediaCryptError::KeyEmpty
    );
    // 不是 base64。
    assert_eq!(
        MediaAesKey::decode("not*base64*at*all").unwrap_err(),
        MediaCryptError::KeyNotDecodable
    );
}

/// 上游用 `strings.TrimSpace` ⇒ 两端的空白（换行 / 空格）要能吃掉。
#[test]
fn decode_trims_surrounding_whitespace() {
    let key = key_bytes(FIPS_KEY);
    let encoded = b64(&key);
    let wrapped = format!("\n  {encoded}\t\n");
    assert_eq!(MediaAesKey::decode(&wrapped).expect("decode").bytes, key);
}

// =====================================================================
// 往返与填充陷阱
// =====================================================================

/// 一个**只认 16 字节上界**的反填充器：上游注释点名的那个坑的对照物。
fn unpad_with_an_aes_block_ceiling(plain: &[u8]) -> Result<Vec<u8>, MediaCryptError> {
    let pad = usize::from(plain[plain.len() - 1]);
    if !(1..=AES_BLOCK_BYTES).contains(&pad) || pad > plain.len() {
        return Err(MediaCryptError::PaddingOutOfRange { pad });
    }
    Ok(plain[..plain.len() - pad].to_vec())
}

/// 上游没有加密方向；本用例用它造出密文，再走**生产的**解密路径。
#[test]
fn round_trip_recovers_every_length() {
    let key = MediaAesKey::from_bytes(key_bytes(FIPS_KEY));
    for length in 0usize..=96 {
        let plain: Vec<u8> = (0..length)
            .map(|index| {
                #[allow(clippy::cast_possible_truncation)]
                {
                    (index as u8).wrapping_mul(31).wrapping_add(7)
                }
            })
            .collect();
        let ciphertext = key.encrypt(&plain);
        assert_eq!(ciphertext.len() % AES_BLOCK_BYTES, 0, "length {length}");
        assert_eq!(
            key.decrypt(&ciphertext).expect("decrypt"),
            plain,
            "length {length}"
        );
    }
}

/// **填充陷阱**：明文长度本来就是 32 的倍数时，补的是**整整一个 32 字节块** ⇒
/// 一个只认 16 字节上界的反填充器会把这份完全正常的载荷拒掉。
#[test]
fn a_32_byte_plaintext_is_padded_with_a_whole_32_byte_block() {
    let key = MediaAesKey::from_bytes(key_bytes(FIPS_KEY));
    let plain = vec![0x41u8; MEDIA_PAD_BLOCK];
    let padded = pad_media(&plain);
    assert_eq!(padded.len(), 2 * MEDIA_PAD_BLOCK);
    assert_eq!(&padded[MEDIA_PAD_BLOCK..], &[0x20u8; MEDIA_PAD_BLOCK]);

    let ciphertext = key.encrypt(&plain);
    assert_eq!(ciphertext.len(), 2 * MEDIA_PAD_BLOCK);
    assert_eq!(key.decrypt(&ciphertext).expect("decrypt"), plain);

    // 同一份字节，换成一个"只认 16"的上界就会被拒 —— 这就是上游注释点名的那个坑。
    assert_eq!(
        unpad_media(&padded).expect("32 的上界收得下"),
        plain,
        "32 字节的填充必须被接受"
    );
    // 一个只认 16 字节上界的反填充器会把上面那份**完全正常**的载荷拒掉。
    assert_eq!(
        unpad_with_an_aes_block_ceiling(&padded),
        Err(MediaCryptError::PaddingOutOfRange {
            pad: MEDIA_PAD_BLOCK
        })
    );
}

/// 补齐与反补齐是互逆的，`pad == 32` 那一格要在。
#[test]
fn pad_and_unpad_are_inverse() {
    for length in 0usize..=70 {
        let plain: Vec<u8> = vec![0x5au8; length];
        let padded = pad_media(&plain);
        assert_eq!(padded.len() % MEDIA_PAD_BLOCK, 0);
        assert!(padded.len() > length);
        assert_eq!(unpad_media(&padded).expect("unpad"), plain);
    }
    // 空明文补**一个整块**（32），不是 0 字节。
    assert_eq!(pad_media(&[]).len(), MEDIA_PAD_BLOCK);
}

// =====================================================================
// 反例
// =====================================================================

/// 尾部被改一个字节 ⇒ **填充校验**抓住它（CBC 没有 MAC，这是唯一的结构性检查）。
#[test]
fn tampering_with_the_tail_is_refused() {
    let key = MediaAesKey::from_bytes(key_bytes(FIPS_KEY));
    let plain = vec![0x11u8; 100];
    let mut ciphertext = key.encrypt(&plain);
    let last = ciphertext.len() - 1;
    ciphertext[last] ^= 0x01;
    let error = key.decrypt(&ciphertext).expect_err("tampered");
    assert!(
        matches!(
            error,
            MediaCryptError::PaddingDisagrees
                | MediaCryptError::PaddingOutOfRange { .. }
                | MediaCryptError::PlaintextEmpty
        ),
        "{error:?}"
    );
}

/// **诚实交代**：改中间那一段不会被动到**最后一个块**，所以填充仍然合法、解密不报错 ——
/// 解出来的字节是垃圾。上游同样只有 CBC + PKCS#7，**没有**认证标签 ⇒ 这不是本仓的退化，
/// 而是这套算法的性质；把它钉住，免得后来者以为"解密成功"等于"内容可信"。
#[test]
fn tampering_before_the_last_block_is_not_detectable_by_design() {
    let key = MediaAesKey::from_bytes(key_bytes(FIPS_KEY));
    let plain = vec![0x22u8; 96];
    let mut ciphertext = key.encrypt(&plain);
    ciphertext[3] ^= 0x01;
    let recovered = key.decrypt(&ciphertext).expect("填充仍然合法");
    assert_eq!(recovered.len(), 96);
    assert_ne!(recovered, plain);
    // 改动落在**第一个块**里 ⇒ 只有那一个块的明文被污染，其余逐字节不变。
    assert_eq!(recovered[AES_BLOCK_BYTES], plain[AES_BLOCK_BYTES]);
}

/// 截断 / 非块对齐 / 空 body 三类都是错误，不是"最好的猜测"。
#[test]
fn truncated_and_misaligned_bodies_are_refused() {
    let key = MediaAesKey::from_bytes(key_bytes(FIPS_KEY));
    let ciphertext = key.encrypt(&[0x33u8; 64]);
    assert_eq!(
        key.decrypt(&ciphertext[..ciphertext.len() - 1]),
        Err(MediaCryptError::CiphertextNotBlockAligned {
            len: ciphertext.len() - 1
        })
    );
    assert_eq!(key.decrypt(&[]), Err(MediaCryptError::CiphertextEmpty));
    // 掉一个整块：块对齐还在，靠填充校验拒。
    let error = key
        .decrypt(&ciphertext[..ciphertext.len() - AES_BLOCK_BYTES])
        .expect_err("truncated");
    assert!(matches!(
        error,
        MediaCryptError::PaddingDisagrees
            | MediaCryptError::PaddingOutOfRange { .. }
            | MediaCryptError::PlaintextEmpty
    ));
}

/// 换一把密钥 ⇒ 填充几乎一定对不上（上游："密钥错了或 body 被截断"）。
#[test]
fn the_wrong_key_is_refused() {
    let right = MediaAesKey::from_bytes(key_bytes(FIPS_KEY));
    let other = MediaAesKey::from_bytes(key_bytes(CBC_KEY));
    let ciphertext = right.encrypt(&[0x44u8; 80]);
    let error = other.decrypt(&ciphertext).expect_err("wrong key");
    assert!(matches!(
        error,
        MediaCryptError::PaddingDisagrees
            | MediaCryptError::PaddingOutOfRange { .. }
            | MediaCryptError::PlaintextEmpty
    ));
}

/// 非法填充长度（0 / 33 / 大于明文长度）各自的错误变体。
#[test]
fn padding_length_bounds_are_checked() {
    assert_eq!(unpad_media(&[]), Err(MediaCryptError::PlaintextEmpty));
    assert_eq!(
        unpad_media(&[0x00]),
        Err(MediaCryptError::PaddingOutOfRange { pad: 0 })
    );
    assert_eq!(
        unpad_media(&[0x21]),
        Err(MediaCryptError::PaddingOutOfRange { pad: 33 })
    );
    assert_eq!(unpad_media(&[0x02, 0x02]), Ok(vec![]));
    assert_eq!(
        unpad_media(&[0x03, 0x01, 0x03]),
        Err(MediaCryptError::PaddingDisagrees)
    );
}

// =====================================================================
// 凭据面（DoD 第 6 条）
// =====================================================================

/// 密钥的 `Debug` 只有 `<redacted, 32 bytes>`；错误路径**不回显**密钥。
#[test]
fn the_key_and_the_error_paths_never_echo_the_key() {
    let key = key_bytes(FIPS_KEY);
    let encoded = b64(&key);
    let parsed = MediaAesKey::decode(&encoded).expect("decode");
    let debugged = format!("{parsed:?}");
    assert_eq!(debugged, "<redacted, 32 bytes>");
    assert!(!debugged.contains(&encoded));
    assert!(!debugged.contains("00010203"));

    // 解不出来的那把密钥本身不能出现在错误里（DoD 第 6 条的反例）。
    let garbage = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGk"; // 44 字符但不是 32 字节
    let error = MediaAesKey::decode(garbage).expect_err("wrong length");
    let message = error.to_string();
    assert!(!message.contains(garbage), "{message}");
    assert!(!message.contains("QUJD"), "{message}");
    // 密文也不进错误：只报长度。
    let error = parsed.decrypt(&[0x01, 0x02, 0x03]).expect_err("misaligned");
    let message = error.to_string();
    assert!(message.contains("3 bytes"), "{message}");
    assert!(!message.contains("01"), "{message}");
}

/// 字符串入口与类型入口是同一件事。
#[test]
fn the_string_entry_point_matches_the_typed_one() {
    let key = key_bytes(FIPS_KEY);
    let encoded = b64(&key);
    let typed = MediaAesKey::from_bytes(key);
    let plain = b"hello wecom";
    let ciphertext = typed.encrypt(plain);
    assert_eq!(
        decrypt_media(&encoded, &ciphertext).expect("decrypt"),
        plain.to_vec()
    );
    assert_eq!(decode_media_aes_key(&encoded).expect("decode").bytes, key);
    assert_eq!(
        decrypt_media("", &ciphertext),
        Err(MediaCryptError::KeyEmpty)
    );
}
