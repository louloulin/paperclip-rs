//! [`super`]（配置解码 + `app_secret` 解密）的用例。
//!
//! 四组：base64 密文列的解码（含 MIME 换行包装）/ 凭据解密与 region 透传 /
//! **凭据纪律**（三条错误路径都不回显密文或明文，且 `Debug` 全脱敏）/
//! **`union_id` 与 `region` 在入站路径上的可测性**（注入解密器与客户端，不依赖真实回填）。

use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;

use super::*;

fn installation_with(secret: &[u8], region: &str, union_id: Option<&str>) -> LarkInstallation {
    LarkInstallation {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        app_id: "cli_test".to_string(),
        app_secret_encrypted: secret.to_vec(),
        tenant_key: Some("tenant-1".to_string()),
        bot_open_id: OpenId::new("ou_bot"),
        bot_union_id: union_id.map(str::to_string),
        region: Region::or_default(region),
        installer_user_id: Id::new(),
        status: "active".to_string(),
    }
}

// =====================================================================
// 一、base64 密文列的解码
// =====================================================================

/// 标准 base64 往返，且 **MIME 换行包装**（SQL 回填的形态）能被剥掉。
#[test]
fn secret_column_accepts_plain_and_mime_wrapped_base64() {
    let ciphertext = vec![0u8, 1, 2, 3, 250, 251, 252, 253];
    let plain = base64::engine::general_purpose::STANDARD.encode(&ciphertext);
    assert_eq!(decode_secret(&plain).expect("标准 base64"), ciphertext);

    // 每 4 个字符插一个换行 + 缩进（MIME 包装的形态）。
    let mut wrapped = String::new();
    for chunk in plain.as_bytes().chunks(4) {
        let _ = std::fmt::Write::write_fmt(
            &mut wrapped,
            format_args!("  {}\r\n", std::str::from_utf8(chunk).unwrap()),
        );
    }
    assert_eq!(
        decode_secret(&wrapped).expect("MIME 包装的 base64 应被剥空白后解开"),
        ciphertext
    );
}

/// 空串 ⇒ 空字节（"这一行没有密文"，由工厂负责拒装配）。
#[test]
fn empty_secret_column_decodes_to_no_bytes() {
    assert_eq!(decode_secret("").expect("空串不是错误"), Vec::<u8>::new());
}

/// 坏 base64 ⇒ 只报**长度**，不回显内容。
#[test]
fn bad_base64_error_reports_length_only() {
    let error = decode_secret("not base64 !!!").expect_err("应当拒");
    assert_eq!(
        error,
        ConfigError::SecretNotBase64 {
            length: "not base64 !!!".len()
        }
    );
    let rendered = error.to_string();
    assert!(!rendered.contains("not base64"), "回显了密文：{rendered}");
}

// =====================================================================
// 二、解密与 region 透传
// =====================================================================

/// `secretbox` 往返：密文经解密器拿到明文 `app_secret`，`region` / `tenant_key` 一并透传。
#[test]
fn credentials_carry_region_and_tenant_key() {
    let key = [7u8; 32];
    let boxed = SecretBox::new(&key).expect("32 字节密钥");
    let sealed = boxed.seal(b"app-secret-value").expect("封");
    let installation = installation_with(&sealed, "lark", Some("on_bot"));

    let credentials = installation_credentials_for(&installation, &Decrypter::secret_box(boxed))
        .expect("解密成功");
    assert_eq!(credentials.app_id, "cli_test");
    assert_eq!(credentials.app_secret.expose(), "app-secret-value");
    assert_eq!(credentials.tenant_key.as_deref(), Some("tenant-1"));
    assert_eq!(credentials.region, Region::Lark);
    assert!(credentials.is_complete());
}

/// 身份解密器（用例便利）把存下来的字节当明文：**不依赖任何真实回填**就能测入站路径。
#[test]
fn login_plaintext_decrypter_treats_stored_bytes_as_the_secret() {
    let installation = installation_with(b"plain-app-secret", "feishu", None);
    let credentials = installation_credentials_for(&installation, &Decrypter::login_plaintext())
        .expect("身份解密");
    assert_eq!(credentials.app_secret.expose(), "plain-app-secret");
    assert_eq!(credentials.region, Region::Feishu);
}

/// 认不出的 `region` 字串回落飞书（`Region::or_default`）⇒ 一条坏行永远不会解析出空 host
/// （**入站路径上的 `region` 可测性**：这条判决不依赖 `region_backfill` 跑过）。
#[test]
fn unknown_region_falls_back_to_feishu_on_the_inbound_path() {
    for raw in ["", "feishu", "lark", "totally-new-cloud"] {
        let installation = installation_with(b"x", raw, None);
        let credentials =
            installation_credentials_for(&installation, &Decrypter::login_plaintext())
                .expect("解密");
        match raw {
            "lark" => assert_eq!(credentials.region, Region::Lark, "{raw}"),
            _ => assert_eq!(credentials.region, Region::Feishu, "{raw}"),
        }
    }
}

// =====================================================================
// 三、凭据纪律（`docs/60` §2.3 的判据 1 与 2）
// =====================================================================

/// 认证失败 / 未接线 / 非 UTF-8 三条错误路径都**不**回显密文与明文。
#[test]
fn error_paths_never_echo_credentials() {
    let ciphertext = b"TOP-SECRET-CIPHERTEXT".to_vec();
    let installation = installation_with(&ciphertext, "feishu", None);

    // ① 认证失败（拿错误的密钥开）。
    let boxed = SecretBox::new(&[9u8; 32]).expect("32 字节密钥");
    let error = installation_credentials_for(&installation, &Decrypter::secret_box(boxed))
        .expect_err("应当认证失败");
    assert_eq!(
        error,
        ConfigError::Decrypt {
            source: DecryptError::Authentication
        }
    );
    let rendered = error.to_string();
    assert!(!rendered.contains("TOP-SECRET"), "回显了密文：{rendered}");

    // ② 未接线（失败关闭）。
    let error = installation_credentials_for(&installation, &Decrypter::fail_closed())
        .expect_err("未接线应当拒");
    assert_eq!(
        error,
        ConfigError::Decrypt {
            source: DecryptError::NotWired
        }
    );

    // ③ 解出来不是 UTF-8（身份解密器直接交出非法字节）。
    let invalid = Decrypter::custom("invalid-utf8(test)", |_| Ok(vec![0xff, 0xfe]));
    let error = installation_credentials_for(&installation, &invalid).expect_err("非 UTF-8 应当拒");
    assert_eq!(
        error,
        ConfigError::Decrypt {
            source: DecryptError::NotUtf8
        }
    );
    let rendered = error.to_string();
    assert!(!rendered.contains("TOP-SECRET"), "回显了密文：{rendered}");
}

/// 配置 blob 与解密器的 `Debug` **全脱敏**（判据 1）。
#[test]
fn config_and_decrypter_debug_are_redacted() {
    let config = LarkInstallConfig {
        app_id: "cli_x".to_string(),
        app_secret_encrypted: "SECRET-B64".to_string(),
        tenant_key: "tenant-1".to_string(),
        bot_open_id: "ou_bot".to_string(),
        bot_union_id: "on_bot".to_string(),
        region: "lark".to_string(),
    };
    let rendered = format!("{config:?}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(!rendered.contains("SECRET-B64"), "{rendered}");
    assert!(
        rendered.contains("cli_x"),
        "非密字段应当照常打印：{rendered}"
    );

    // 空列报 `<empty>`（运维要知道"配没配"，不需要值）。
    let empty = LarkInstallConfig {
        app_secret_encrypted: String::new(),
        bot_union_id: String::new(),
        ..LarkInstallConfig::default()
    };
    let rendered = format!("{empty:?}");
    assert!(rendered.contains("<empty>"), "{rendered}");

    // 解密器只报类别。
    assert!(format!("{:?}", Decrypter::fail_closed()).contains("fail-closed"));
    assert!(format!("{:?}", Decrypter::login_plaintext()).contains("plaintext(test)"));
}

// =====================================================================
// 四、配置 blob → 安装投影
// =====================================================================

/// `into_installation`：密文解成裸字节、空串字段变 `None`、region 归一。
#[test]
fn config_maps_to_an_installation_projection() {
    let ciphertext = vec![9u8, 8, 7];
    let config = LarkInstallConfig {
        app_id: "cli_x".to_string(),
        app_secret_encrypted: base64::engine::general_purpose::STANDARD.encode(&ciphertext),
        tenant_key: String::new(),
        bot_open_id: "ou_bot".to_string(),
        bot_union_id: String::new(),
        region: "lark".to_string(),
    };
    let installation = config
        .into_installation(Id::new(), Id::new(), Id::new(), Id::new(), "active")
        .expect("合法配置");
    assert_eq!(installation.app_secret_encrypted, ciphertext);
    assert_eq!(installation.tenant_key, None, "空串应当变 None");
    assert_eq!(installation.bot_union_id, None, "空串应当变 None");
    assert_eq!(installation.region, Region::Lark);
    assert_eq!(installation.bot_open_id.as_str(), "ou_bot");
    assert!(installation.is_active());
    // 缺 `bot_union_id` 的安装（**回填之前**）在入站路径上仍可用：判据取空串而不是 `None`。
    assert_eq!(installation.bot_union_id_or_empty(), "");
}

/// `LarkInstallation` 的 `Debug` 只报密文**长度**（判据 1 的另一半）。
#[test]
fn installation_debug_reports_the_ciphertext_length_only() {
    let installation = installation_with(b"SECRET-CIPHERTEXT", "feishu", Some("on_bot"));
    let rendered = format!("{installation:?}");
    assert!(!rendered.contains("SECRET-CIPHERTEXT"), "{rendered}");
    assert!(rendered.contains("app_secret_encrypted_len"), "{rendered}");
    assert!(rendered.contains("has_bot_union_id: true"), "{rendered}");
}
