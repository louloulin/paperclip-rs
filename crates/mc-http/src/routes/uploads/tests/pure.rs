//! M10-B2 的**纯函数层**用例（门 ⑤，不需要磁盘也不需要库 ⇒ 可以精确到字节）。
//!
//! 覆盖：键守卫的三条路径穿越反例 + 内部路径、百分号解码、魔数嗅探、扩展名覆盖。
//! 端到端那三条反例的**活**判据在 `tests.rs`（真 router + 真磁盘 + 真符号链接）；
//! 本文件把同一批规则的**每一支**都钉住，避免端到端只覆盖到其中一支。

use super::super::{detect_content_type, guard_static_key, percent_decode, KeyError};

#[test]
fn key_guard_accepts_ordinary_keys() {
    for key in [
        "users/u1/a.png",
        "workspaces/11111111-1111-1111-1111-111111111111/0123abcd.png",
        "a.txt",
    ] {
        assert_eq!(guard_static_key(key), Ok(()), "{key} 应当放行");
    }
}

#[test]
fn key_guard_rejects_dot_dot() {
    for key in [
        "..",
        "../secret",
        "users/../../secret",
        "users/u1/..",
        "a/./b",
        "users/u1/../../../etc/passwd",
    ] {
        assert_eq!(guard_static_key(key), Err(KeyError::DotDot), "{key}");
    }
}

#[test]
fn key_guard_rejects_absolute_paths() {
    for key in [
        "/etc/passwd",
        "/",
        r"\windows\system32",
        "C:/windows",
        "c:\\windows",
    ] {
        assert_eq!(guard_static_key(key), Err(KeyError::Absolute), "{key}");
    }
}

#[test]
fn key_guard_rejects_empty_and_empty_segments() {
    for key in ["", "users//a.png", "users/u1/"] {
        assert_eq!(guard_static_key(key), Err(KeyError::Empty), "{key:?}");
    }
}

#[test]
fn key_guard_rejects_internal_paths() {
    // 上游 `storage/local.go::isInternalLocalPath` 逐字的两个后缀。
    assert_eq!(guard_static_key("a.png.meta.json"), Err(KeyError::Internal));
    assert_eq!(
        guard_static_key("users/u1/a.png.meta.json"),
        Err(KeyError::Internal)
    );
    // 暂存文件：basename 以 `.` 开头且以 `.tmp` 结尾。
    assert_eq!(guard_static_key(".a.png.tmp"), Err(KeyError::Internal));
    assert_eq!(guard_static_key("users/u1/.x.tmp"), Err(KeyError::Internal));
    // 只以 `.` 开头但不是暂存文件的键**照常放行**（别把判据写得过宽）。
    assert_eq!(guard_static_key("users/u1/.hidden"), Ok(()));
}

#[test]
fn percent_decode_matches_the_go_url_path_semantics() {
    assert_eq!(
        percent_decode("users/u1/a.png").as_deref(),
        Some("users/u1/a.png")
    );
    assert_eq!(
        percent_decode("users/u1/%E4%B8%AD%E6%96%87.png").as_deref(),
        Some("users/u1/中文.png")
    );
    assert_eq!(percent_decode("a%20b.png").as_deref(), Some("a b.png"));
    // 解码**之后**才出现的 `..` 正是那条反例的落点。
    assert_eq!(percent_decode("%2e%2e/x").as_deref(), Some("../x"));
    assert!(guard_static_key(&percent_decode("%2e%2e/x").expect("decoded")).is_err());
    // 畸形转义 ⇒ `None`（⇒ 404）。
    assert_eq!(percent_decode("%zz"), None);
    assert_eq!(percent_decode("%2"), None);
    assert_eq!(percent_decode("trailing%"), None);
    // 非 UTF-8 序列 ⇒ `None`（`String::from_utf8` 失败）。
    assert_eq!(percent_decode("%FF%FE"), None);
}

#[test]
fn sniffing_covers_the_upstream_magic_table_subset() {
    assert_eq!(detect_content_type(b"\x89PNG\r\n\x1a\n...."), "image/png");
    assert_eq!(detect_content_type(b"\xFF\xD8\xFF\xE0...."), "image/jpeg");
    assert_eq!(detect_content_type(b"GIF89a...."), "image/gif");
    assert_eq!(detect_content_type(b"GIF87a...."), "image/gif");
    assert_eq!(
        detect_content_type(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
        "image/webp"
    );
    assert_eq!(detect_content_type(b"%PDF-1.7\n"), "application/pdf");
    assert_eq!(detect_content_type(b"PK\x03\x04rest"), "application/zip");
    assert_eq!(detect_content_type(b"\x1F\x8B\x08rest"), "application/gzip");
    // 文本兜底 / 二进制兜底（Go 的兜底值逐字）。
    assert_eq!(detect_content_type(b"hello"), "text/plain; charset=utf-8");
    assert_eq!(detect_content_type(b""), "text/plain; charset=utf-8");
    assert_eq!(
        detect_content_type(b"\x00\x01\x02\x03binary"),
        "application/octet-stream"
    );
    // 半个魔数不算命中（`RIFF` 但不是 `WEBP` ⇒ 落到二进制兜底）。
    assert_eq!(
        detect_content_type(b"RIFF\x00\x00\x00\x00AVI "),
        "application/octet-stream"
    );
}
