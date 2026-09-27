//! `download.rs` 的**纯函数层**用例（门 ⑤，零库）。
//!
//! 拆出来是门 ⑩ 的 800 行硬上限要求，先例 = `routes/onboarding/tests.rs`（M9-3）
//! 与 `routes/probes/ready/tests.rs`（M10-2）。
//!
//! 这一层全部是**无 I/O 的判定**：HMAC（对 RFC 4231 向量）、能力消息与签名的
//! fail-closed 六条、内容处置的文件名消毒、预览白名单、对象引用切分。
//! 真库那一半在 `tests/db.rs`。

use crate::routes::attachments::download::*;
use sha2::{Digest, Sha256};

/// 本片的「配了根密钥」夹具：**注入**而不是改进程 env。
///
/// 判据 = `routes/config/tests.rs:15-19` 的注释：并行测试改全局 env 本身就是竞态。
fn key() -> [u8; 32] {
    derive_capability_key("unit-test-secret")
}

// ---- 形态 / 能力 ----

#[test]
fn capability_message_has_no_intent_suffix_for_load() {
    assert_eq!(capability_message("abc", 42, ""), "v1|abc|42");
}

#[test]
fn capability_message_appends_intent() {
    assert_eq!(
        capability_message("abc", 42, CAPABILITY_DOWNLOAD_INTENT),
        "v1|abc|42|attachment"
    );
}

#[test]
fn hmac_sha256_matches_rfc4231_test_case_2() {
    // RFC 4231 test case 2：key="Jefe", data="what do ya want for nothing?"
    let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
        hex::encode(mac),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[test]
fn hmac_sha256_matches_rfc4231_test_case_1() {
    let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
    assert_eq!(
        hex::encode(mac),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

#[test]
fn capability_paths_are_site_relative_and_carry_exp_and_sig() {
    let k = key();
    let p = capability_path(Some(&k), "11111111-1111-1111-1111-111111111111", 1_000);
    assert!(p.starts_with(
        "/api/attachments/11111111-1111-1111-1111-111111111111/signed-download?exp=1060&sig="
    ));
    let d = download_capability_path(Some(&k), "11111111-1111-1111-1111-111111111111", 1_000);
    assert!(d.ends_with("&dl=1"));
}

#[test]
fn load_and_download_intents_sign_differently() {
    let k = key();
    let id = "11111111-1111-1111-1111-111111111111";
    assert_ne!(
        sign_capability(Some(&k), id, 10, ""),
        sign_capability(Some(&k), id, 10, CAPABILITY_DOWNLOAD_INTENT)
    );
}

#[test]
fn verify_capability_round_trips() {
    let k = key();
    let id = "11111111-1111-1111-1111-111111111111";
    let sig = sign_capability(Some(&k), id, 1_060, "");
    assert!(verify_capability(Some(&k), id, "1060", &sig, "", 1_000));
}

#[test]
fn verify_capability_fails_closed_on_every_path() {
    let k = key();
    let id = "11111111-1111-1111-1111-111111111111";
    let sig = sign_capability(Some(&k), id, 1_060, "");
    // 过期
    assert!(!verify_capability(Some(&k), id, "1060", &sig, "", 1_061));
    // 缺字段
    assert!(!verify_capability(Some(&k), id, "", &sig, "", 1_000));
    assert!(!verify_capability(Some(&k), id, "1060", "", "", 1_000));
    assert!(!verify_capability(Some(&k), "", "1060", &sig, "", 1_000));
    // exp 不可解析
    assert!(!verify_capability(Some(&k), id, "abc", &sig, "", 1_000));
    // 签名畸形（奇数长度 / 非 hex）
    assert!(!verify_capability(Some(&k), id, "1060", "abc", "", 1_000));
    assert!(!verify_capability(Some(&k), id, "1060", "zzzz", "", 1_000));
    // 给别的附件签的
    let other = sign_capability(Some(&k), "22222222-2222-2222-2222-222222222222", 1_060, "");
    assert!(!verify_capability(Some(&k), id, "1060", &other, "", 1_000));
    // 🔴 延长 exp 作废签名（签名覆盖 exp）
    assert!(!verify_capability(Some(&k), id, "9999999", &sig, "", 1_000));
    // 🔴 load 链接不能自升格成 dl=1
    assert!(!verify_capability(
        Some(&k),
        id,
        "1060",
        &sig,
        CAPABILITY_DOWNLOAD_INTENT,
        1_000
    ));
}

#[test]
fn verify_capability_fails_closed_without_key() {
    // 没配根密钥 ⇒ 铸造侧空串（降级）、兑换侧一律拒。
    assert!(sign_capability(None, "x", 10, "").is_empty());
    assert!(capability_path(None, "x", 1_000).is_empty());
    assert!(download_capability_path(None, "x", 1_000).is_empty());
    assert!(!verify_capability(
        None,
        "x",
        "1060",
        &"a".repeat(64),
        "",
        1_000
    ));
}

#[test]
fn capability_key_is_domain_separated() {
    let k = derive_capability_key("shared-root");
    let mut plain = Sha256::new();
    plain.update(b"shared-root");
    let plain: [u8; 32] = plain.finalize().into();
    assert_ne!(k, plain, "域分隔前缀必须真的进了哈希");
}

#[test]
fn attachment_download_path_is_stable() {
    assert_eq!(
        attachment_download_path("abc"),
        "/api/attachments/abc/download"
    );
}

// ---- Content-Disposition ----

#[test]
fn svg_is_never_inline() {
    assert!(!is_inline_content_type("image/svg+xml"));
    assert!(!is_inline_content_type("IMAGE/SVG+XML; charset=utf-8"));
    assert!(is_inline_content_type("image/png"));
    assert!(is_inline_content_type("video/mp4"));
    assert!(is_inline_content_type("audio/mpeg"));
    assert!(is_inline_content_type("application/pdf"));
    assert!(!is_inline_content_type("application/zip"));
    assert!(!is_inline_content_type("text/plain"));
}

#[test]
fn disposition_picks_inline_for_media() {
    assert_eq!(
        content_disposition("image/png", "a.png"),
        "inline; filename=\"a.png\""
    );
    assert_eq!(
        content_disposition("application/zip", "a.zip"),
        "attachment; filename=\"a.zip\""
    );
}

#[test]
fn disposition_sanitizes_header_injection() {
    let got = content_disposition("application/zip", "ev\"il\r\nX: y.txt");
    // 🔴 头里**不能**有 CR / LF（那是真正的响应头注入 / 拆分向量）。
    assert!(!got.contains('\r'));
    assert!(!got.contains('\n'));
    // ⚠️ 但 `filename="…"` 的那对**定界引号必须留着**（上游 `storage/util.go:85`
    // 逐字带引号）⇒ 断言落在**定界符之间**的文件名上，而不是整条头。
    let quoted = got
        .split_once("filename=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(name, _)| name)
        .unwrap_or_default();
    assert!(!quoted.contains('"'));
    assert!(!quoted.contains(';'));
    assert!(!quoted.contains('\\'));
    // 控制字符与 NUL 一律被换成 `_`。
    assert!(!quoted.contains('\0'));
    assert!(quoted.chars().all(|c| (c as u32) >= 0x20));
}

#[test]
fn disposition_uses_rfc5987_for_non_ascii() {
    let got = content_disposition("application/zip", "报告.txt");
    assert!(got.contains("filename*=UTF-8''"));
    // 旧客户端的 ASCII 回退名里不能有非 ASCII。
    let fallback = got
        .split("filename=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or_default();
    assert!(fallback.is_ascii());
}

#[test]
fn attachment_disposition_forces_attachment() {
    assert_eq!(
        attachment_content_disposition("a.png"),
        "attachment; filename=\"a.png\""
    );
}

// ---- 预览白名单 ----

#[test]
fn preview_whitelist_accepts_text_and_known_extensions() {
    assert!(is_text_previewable("text/plain", "x.bin"));
    assert!(is_text_previewable("text/markdown", "README"));
    assert!(is_text_previewable("application/json", "x.bin"));
    assert!(is_text_previewable("application/octet-stream", "main.rs"));
    assert!(is_text_previewable(
        "application/octet-stream",
        "Dockerfile"
    ));
    assert!(is_text_previewable("image/png", "notes.md"));
}

#[test]
fn preview_whitelist_rejects_binaries() {
    assert!(!is_text_previewable("image/png", "a.png"));
    assert!(!is_text_previewable("application/zip", "a.zip"));
    assert!(!is_text_previewable("application/pdf", "a.pdf"));
}

#[test]
fn preview_whitelist_handles_extensionless_source() {
    assert!(is_text_previewable("application/octet-stream", "Makefile"));
    assert!(!is_text_previewable("image/png", "logo"));
}

// ---- 对象引用切分 ----

#[test]
fn split_object_ref_handles_relative_and_absolute() {
    assert_eq!(
        split_object_ref("mc-assets/a/b/c.png"),
        Some(("mc-assets".into(), "a/b/c.png".into()))
    );
    assert_eq!(
        split_object_ref("/mc-assets/a/b.png"),
        Some(("mc-assets".into(), "a/b.png".into()))
    );
    assert_eq!(
        split_object_ref("https://cdn.example.com/mc-assets/a/b.png"),
        Some(("mc-assets".into(), "a/b.png".into()))
    );
}

#[test]
fn split_object_ref_drops_query_and_fragment() {
    assert_eq!(
        split_object_ref("mc-assets/a/b.png?v=2#frag"),
        Some(("mc-assets".into(), "a/b.png".into()))
    );
}

#[test]
fn split_object_ref_rejects_unusable_shapes() {
    assert_eq!(split_object_ref(""), None);
    assert_eq!(split_object_ref("no-bucket-separator"), None);
    assert_eq!(split_object_ref("bucket/"), None);
    assert_eq!(split_object_ref("/key-only"), None);
}

#[test]
fn object_ref_round_trips_with_local_disk_path_for() {
    // 与 `mc_storage::local::LocalDiskStorage::path_for`（root/bucket/key）互逆。
    let (bucket, key) = split_object_ref("mc-assets/2026/09/a.png").unwrap();
    assert_eq!(format!("{bucket}/{key}"), "mc-assets/2026/09/a.png");
}
