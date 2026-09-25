//! `config.rs` 的用例（不依赖数据库、不依赖网络）。
//!
//! 三类：① 密文列的编码/解码（含 MIME 折行的两种等价写法）；② 公开投影；
//! ③ **凭据纪律**（手写 `Debug` 的脱敏 + 错误文案不回显密文 —— `docs/60` §2.3 第 3 条）。

use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::dingtalk::Decrypter;

/// 用例用的"解密器"：把 base64 密文当成明文解（**只**给不碰 `secretbox` 的用例）。
fn identity_decrypter() -> Decrypter {
    Decrypter::new(
        "identity",
        Arc::new(|ciphertext: &str| {
            decode_ciphertext(ciphertext)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|error| error.to_string())
        }),
    )
}

#[test]
fn install_config_round_trips_through_the_config_column() {
    let sealed = [7u8, 8, 9, 10];
    let config = InstallConfig::byo("dingabc123", &sealed);
    assert_eq!(config.app_id, "dingabc123");
    assert_eq!(
        config.robot_code, "dingabc123",
        "机器人码 = AppKey（上游逐字）"
    );
    let value = config.to_config_value().expect("serialize");
    assert_eq!(value[FIELD_APP_ID], json!("dingabc123"));
    assert_eq!(value[FIELD_ROBOT_CODE], json!("dingabc123"));
    // 密文列是 base64，不是明文、也不是数组。
    let encoded = value[FIELD_APP_SECRET_ENCRYPTED]
        .as_str()
        .expect("string column")
        .to_string();
    assert_eq!(decode_ciphertext(&encoded).expect("decode"), sealed);

    let back: InstallConfig = serde_json::from_value(value).expect("deserialize");
    assert_eq!(back, config);
}

#[test]
fn robot_code_falls_back_to_app_id_and_prefers_the_explicit_value() {
    let value = json!({ FIELD_APP_ID: "k1" });
    assert_eq!(decode_public_config(&value).robot_code, "k1");
    let value = json!({ FIELD_APP_ID: "k1", FIELD_ROBOT_CODE: "r2" });
    assert_eq!(decode_public_config(&value).robot_code, "r2");
    // 空串也是"没配"（上游 `robotCodeOrAppID` 逐字）。
    let value = json!({ FIELD_APP_ID: "k1", FIELD_ROBOT_CODE: "" });
    assert_eq!(decode_public_config(&value).robot_code, "k1");
}

#[test]
fn ciphertext_accepts_mime_wrapped_base64() {
    let sealed: Vec<u8> = (0u8..200).collect();
    let wrapped = encode_ciphertext(&sealed);
    // 模拟 PostgreSQL `encode(…, 'base64')` 每 64 字符折一行的形态。
    let mime: String = wrapped
        .as_bytes()
        .chunks(64)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(mime.contains('\n'));
    assert_eq!(decode_ciphertext(&wrapped).expect("plain"), sealed);
    assert_eq!(decode_ciphertext(&mime).expect("mime"), sealed);
}

#[test]
fn strip_whitespace_removes_only_ascii_whitespace() {
    assert_eq!(strip_whitespace(" a\tb\nc\rd "), "abcd");
    // 非 ASCII 空白**不**删（上游逐字只认那四个字符）。
    assert_eq!(strip_whitespace("a\u{a0}b"), "a\u{a0}b");
}

#[test]
fn config_errors_never_echo_ciphertext_or_plaintext() {
    let secret = "app-secret-that-must-not-leak";
    // ① 非法 base64：错误只带**长度**。
    let error = decode_ciphertext(secret).expect_err("garbage base64");
    let rendered = error.to_string();
    assert!(!rendered.contains(secret), "{rendered}");
    assert!(matches!(error, ConfigError::Ciphertext { len, .. } if len == secret.len()));

    // ② 解密器拒绝：错误只带**原因类别**，不带密文。
    let sealed = encode_ciphertext(b"some-bytes");
    let refusing = Decrypter::new(
        "refusing",
        Arc::new(|_ciphertext: &str| Err("nope".to_string())),
    );
    let error = credentials_from_config(
        &json!({ FIELD_APP_ID: "k1", FIELD_APP_SECRET_ENCRYPTED: sealed }),
        &refusing,
    )
    .expect_err("decrypter refused");
    let rendered = error.to_string();
    assert!(!rendered.contains("some-bytes"), "{rendered}");
    assert!(
        !rendered.contains("nope"),
        "原因也不该透出实现细节：{rendered}"
    );

    // ③ `Debug` 不打印密文列（凭据纪律第 1 条）；`app_id` / `robot_code` **不是**秘密，
    //    它们照常打印（上游逐字：The AppKey itself is not a secret）。
    let config = InstallConfig::byo("k1", secret.as_bytes());
    let debugged = format!("{config:?}");
    assert!(
        !debugged.contains(&encode_ciphertext(secret.as_bytes())),
        "{debugged}"
    );
    assert!(!debugged.contains(secret), "{debugged}");
    assert!(debugged.contains("<redacted>"), "{debugged}");
    assert!(debugged.contains("k1"), "AppKey 不是秘密：{debugged}");
    let empty = format!("{:?}", InstallConfig::default());
    assert!(empty.contains("<empty>"), "{empty}");
}

#[test]
fn credentials_from_config_reads_the_ciphertext_column() {
    let sealed = encode_ciphertext(b"plain-app-secret");
    let value = json!({
        FIELD_APP_ID: "k1",
        FIELD_ROBOT_CODE: "r1",
        FIELD_APP_SECRET_ENCRYPTED: sealed,
    });
    let credentials = credentials_from_config(&value, &identity_decrypter()).expect("decode");
    assert_eq!(credentials.app_key, "k1");
    assert_eq!(credentials.robot_code, "r1");
    assert_eq!(credentials.app_secret.expose(), "plain-app-secret");
    // `Credentials` 的 `Debug` 同样脱敏。
    assert!(!format!("{credentials:?}").contains("plain-app-secret"));
}

#[test]
fn credentials_from_config_rejects_the_three_empty_shapes() {
    let decrypter = identity_decrypter();
    assert_eq!(
        credentials_from_config(&json!(null), &decrypter).err(),
        Some(ConfigError::Empty)
    );
    assert_eq!(
        credentials_from_config(&json!([1, 2]), &decrypter).err(),
        Some(ConfigError::NotAnObject)
    );
    assert_eq!(
        credentials_from_config(&json!({ FIELD_ROBOT_CODE: "r1" }), &decrypter).err(),
        Some(ConfigError::MissingAppId)
    );
}
