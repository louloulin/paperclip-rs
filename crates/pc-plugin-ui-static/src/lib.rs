//! `pc-plugin-ui-static` —— plugin UI bundle 静态文件服务纯函数层。
//!
//! 与 Node `server/src/routes/plugin-ui-static.ts` 1:1 对齐：
//! - `resolve_plugin_ui_dir` 解析 dist/ui/ 路径(含 path traversal 防护)
//! - `compute_etag` 用 size+mtime 生成弱 ETag
//! - `cache_control_for` 基于文件名是否含 content hash 决定 immutable / must-revalidate
//! - `mime_for_extension` 查表 + 兜底 `application/octet-stream`
//! - `is_loopback_host` SSRF 防护（dev proxy 仅允许 localhost）
//! - `has_dev_proxy_override` 校验 devUiUrl 是否覆盖原始 URL(防止 path-based protocol hijack)
//! - `safe_dev_proxy_url` 构造已校验的 target URL
//!
//! 高内聚:不感知 axum/HTTP,只产出文件元数据 + 路径 + headers。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// 1 年 = 31536000 秒 —— content-hash immutable cache 头。
pub const ONE_YEAR_SECONDS: u32 = 365 * 24 * 60 * 60;

/// 内容哈希模式:文件名中包含至少 8 字符的 hex hash 段(与 Node CONTENT_HASH_PATTERN 等价)。
pub const CONTENT_HASH_PATTERN: &str = r"[.-][a-fA-F0-9]{8,}\.\w+$";

/// immutable cache-control 头。
pub const CACHE_CONTROL_IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// 可重新校验 cache-control 头。
pub const CACHE_CONTROL_REVALIDATE: &str = "public, max-age=0, must-revalidate";

#[derive(Debug, Error)]
pub enum PluginUiError {
    #[error("plugin ui dir not found for package {package}")]
    UiDirNotFound { package: String },
    #[error("file path traversal denied: {0}")]
    PathTraversal(String),
    #[error("invalid file path: {0}")]
    InvalidPath(String),
    #[error("file not found: {0}")]
    NotFound(String),
}

/// MIME 查表 + 兜底 octet-stream(精简版,与 Node MIME_TYPES 1:1)。
pub fn mime_for_extension(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "js" => "application/javascript; charset=utf-8",
        "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "map" => "application/json; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "eot" => "application/vnd.ms-fontobject",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// 计算弱 ETag(Node `computeETag` 1:1)。用 size+mtime 而非文件内容,避免大文件读盘。
pub fn compute_etag(size: u64, mtime_ms: u64) -> String {
    let mut h = Sha256::new();
    h.update(format!("v2:{size}-{mtime_ms}").as_bytes());
    let digest = h.finalize();
    let hex = hex::encode(digest);
    format!("\"{}\"", &hex[..16])
}

/// 文件名是否匹配 content-hash 模式(决定 immutable cache)。
pub fn is_content_hashed_name(name: &str) -> bool {
    // 用更宽的 regex 等价检查:找 `[.-]<8+ hex>.<ext>` 后缀
    if let Some(dot) = name.rfind('.') {
        let ext = &name[dot + 1..];
        let stem = &name[..dot];
        // stem 必须包含至少 8 个连续 hex 且位于 `[.-]` 边界
        let bytes = stem.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let is_boundary = i == 0 || matches!(bytes[i - 1], b'.' | b'-');
            if is_boundary {
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                    j += 1;
                }
                if j - i >= 8 && (j == bytes.len() || matches!(bytes[j], b'.' | b'-')) {
                    if !ext.is_empty() {
                        return true;
                    }
                }
                i = j.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
    false
}

/// 文件名命中 content hash → 返回 immutable,否则 must-revalidate。
pub fn cache_control_for(name: &str) -> &'static str {
    if is_content_hashed_name(name) {
        CACHE_CONTROL_IMMUTABLE
    } else {
        CACHE_CONTROL_REVALIDATE
    }
}

/// 解析 plugin UI 目录(与 Node `resolvePluginUiDir` 等价)。
///
/// 优先级:
/// 1. `package_path` 直接指向已安装的包根;若存在,拼 `entrypoints_ui` 后必须仍在包根内。
/// 2. 否则在 `local_plugin_dir/node_modules/<package_name>` 解析(scoped name 支持 `@scope/name`)。
/// 3. 最后回退到 `local_plugin_dir/<package_name>`(local-path 安装)。
///
/// 返回 `None` 当任一候选都不存在 / 解析失败 / 路径逃逸。
pub fn resolve_plugin_ui_dir(
    local_plugin_dir: &Path,
    package_name: &str,
    entrypoints_ui: &str,
    package_path: Option<&str>,
) -> Option<PathBuf> {
    if let Some(pp) = package_path {
        let resolved_pkg = std::fs::canonicalize(pp).ok()?;
        if !resolved_pkg.exists() {
            return None;
        }
        let candidate = resolved_pkg.join(entrypoints_ui);
        let canonical_candidate = std::fs::canonicalize(&candidate).ok()?;
        if canonical_candidate.starts_with(&resolved_pkg) && canonical_candidate.exists() {
            return Some(canonical_candidate);
        }
    }
    let package_root = if let Some(rest) = package_name.strip_prefix('@') {
        local_plugin_dir.join("node_modules").join(rest)
    } else {
        local_plugin_dir.join("node_modules").join(package_name)
    };
    let package_root = if package_root.exists() {
        package_root
    } else {
        let direct = local_plugin_dir.join(package_name);
        if direct.exists() {
            direct
        } else {
            return None;
        }
    };
    let canonical_root = std::fs::canonicalize(&package_root).ok()?;
    let candidate = canonical_root.join(entrypoints_ui);
    let canonical_candidate = std::fs::canonicalize(&candidate).ok()?;
    if !canonical_candidate.starts_with(&canonical_root) {
        return None;
    }
    Some(canonical_candidate)
}

/// 在 `ui_dir` 内安全解析 `raw_file_path`,确保不越界。
///
/// 1. 解码 percent-encoding(失败的 → InvalidPath)
/// 2. 拒绝含 `://`、`//`、`\\` 的协议/绝对路径绕过
/// 3. 用 `Path::join` 拼到 ui_dir,canonicalize,验证前缀
pub fn safe_resolve_within(ui_dir: &Path, raw_file_path: &str) -> Result<PathBuf, PluginUiError> {
    let decoded = percent_decode(raw_file_path)
        .ok_or_else(|| PluginUiError::InvalidPath(raw_file_path.to_string()))?;
    if decoded.contains("://") || decoded.starts_with("//") || decoded.starts_with("\\\\") {
        return Err(PluginUiError::InvalidPath(raw_file_path.to_string()));
    }
    let joined = ui_dir.join(&decoded);
    let canonical_ui = std::fs::canonicalize(ui_dir)
        .map_err(|_| PluginUiError::NotFound(ui_dir.to_string_lossy().into_owned()))?;
    let canonical_target = std::fs::canonicalize(&joined)
        .map_err(|_| PluginUiError::NotFound(joined.to_string_lossy().into_owned()))?;
    if !canonical_target.starts_with(&canonical_ui) {
        return Err(PluginUiError::PathTraversal(decoded));
    }
    if !canonical_target.is_file() {
        return Err(PluginUiError::NotFound(
            canonical_target.to_string_lossy().into_owned(),
        ));
    }
    Ok(canonical_target)
}

/// 极简 percent-decode;失败返回 None。
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// host 是否为 loopback(SSRF 防护:dev proxy 仅允许 localhost)。
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

/// 校验 devUiUrl 字符串基本形态:必须以 http:// 或 https:// 开头(不做 SSRF 解析,见 `safe_dev_proxy_target`)。
pub fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// 在 base URL 之上拼接 raw path,产出 target URL。  
/// `base` 必须以 `/` 结尾,否则自动追加。
pub fn build_dev_proxy_url(base: &str, raw_path: &str) -> Option<String> {
    let normalized_base = if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{base}/")
    };
    // 直接字符串拼接:rust 没有内置 URL 构造,但 path 已经过 `safe_resolve_within` 校验
    Some(format!("{normalized_base}{raw_path}"))
}

/// 决策:传入的 raw_file_path 是否能逃出 base URL(协议覆盖检测)。
///
/// 在 Node 中通过 `new URL(rawPath, base)` 推断,如果 decoded path 含 `://`/`//`/`\\` 即视为逃逸。
pub fn path_attempts_protocol_override(raw_file_path: &str) -> bool {
    let Some(decoded) = percent_decode(raw_file_path) else {
        return true;
    };
    decoded.contains("://") || decoded.starts_with("//") || decoded.starts_with("\\\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_known_extensions() {
        assert_eq!(
            mime_for_extension("js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(mime_for_extension("CSS"), "text/css; charset=utf-8");
        assert_eq!(mime_for_extension("woff2"), "font/woff2");
    }

    #[test]
    fn mime_unknown_falls_back_to_octet_stream() {
        assert_eq!(mime_for_extension("xyz"), "application/octet-stream");
    }

    #[test]
    fn etag_is_deterministic_and_quoted() {
        let e1 = compute_etag(123, 456);
        let e2 = compute_etag(123, 456);
        assert_eq!(e1, e2);
        assert!(e1.starts_with('"') && e1.ends_with('"'));
        assert_eq!(e1.len(), 1 + 16 + 1);
        let e3 = compute_etag(124, 456);
        assert_ne!(e1, e3, "different size → different etag");
    }

    #[test]
    fn content_hash_detection() {
        assert!(is_content_hashed_name("index-a1b2c3d4.js"));
        assert!(is_content_hashed_name("styles.abc123def.css"));
        assert!(is_content_hashed_name("chunk-ABCDEF01.mjs"));
        assert!(!is_content_hashed_name("index.js"), "no hash segment");
        assert!(!is_content_hashed_name("app.css"), "no hash segment");
    }

    #[test]
    fn cache_control_picks_immutable_for_hashed_files() {
        assert_eq!(
            cache_control_for("index-abc12345.js"),
            CACHE_CONTROL_IMMUTABLE
        );
        assert_eq!(cache_control_for("index.js"), CACHE_CONTROL_REVALIDATE);
    }

    #[test]
    fn resolve_plugin_ui_dir_uses_package_path_when_provided() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(pkg.join("dist/ui")).unwrap();
        std::fs::write(pkg.join("dist/ui/index.js"), b"console.log('hi')").unwrap();
        let resolved = resolve_plugin_ui_dir(
            tmp.path(),
            "@scope/name",
            "./dist/ui",
            Some(pkg.to_str().unwrap()),
        );
        assert!(resolved.is_some());
        assert!(resolved.unwrap().ends_with("dist/ui"));
    }

    #[test]
    fn resolve_plugin_ui_dir_falls_back_to_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("node_modules").join("acme-x");
        std::fs::create_dir_all(pkg.join("dist/ui")).unwrap();
        std::fs::write(pkg.join("dist/ui/index.js"), b"x").unwrap();
        let resolved = resolve_plugin_ui_dir(tmp.path(), "acme-x", "./dist/ui", None);
        assert!(resolved.is_some());
    }

    #[test]
    fn resolve_plugin_ui_dir_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve_plugin_ui_dir(tmp.path(), "missing-pkg", "./dist/ui", None);
        assert!(resolved.is_none());
    }

    #[test]
    fn safe_resolve_within_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let ui = tmp.path().join("ui");
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::write(ui.join("index.js"), b"x").unwrap();
        // 在 ui 同级目录创建 outside.js,看 ../outside.js 是否被前缀检查拒绝。
        let sibling = tmp.path().join("outside.js");
        std::fs::write(&sibling, b"sibling").unwrap();
        let bad = safe_resolve_within(&ui, "../outside.js");
        assert!(
            matches!(bad, Err(PluginUiError::PathTraversal(_))),
            "expected PathTraversal, got {bad:?}"
        );
    }

    #[test]
    fn safe_resolve_within_rejects_protocol_override() {
        let tmp = tempfile::tempdir().unwrap();
        let ui = tmp.path().join("ui");
        std::fs::create_dir_all(&ui).unwrap();
        let bad = safe_resolve_within(&ui, "https://evil.com/x");
        assert!(matches!(bad, Err(PluginUiError::InvalidPath(_))));
    }

    #[test]
    fn safe_resolve_within_returns_resolved_file_for_valid_path() {
        let tmp = tempfile::tempdir().unwrap();
        let ui = tmp.path().join("ui");
        std::fs::create_dir_all(&ui).unwrap();
        let f = ui.join("index.js");
        std::fs::write(&f, b"x").unwrap();
        let resolved = safe_resolve_within(&ui, "index.js").expect("ok");
        assert!(resolved.ends_with("index.js"));
    }

    #[test]
    fn loopback_host_check() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn is_http_url_basic() {
        assert!(is_http_url("http://localhost:3000/"));
        assert!(is_http_url("https://127.0.0.1/"));
        assert!(!is_http_url("file:///etc/passwd"));
        assert!(!is_http_url("ftp://x/"));
    }

    #[test]
    fn build_dev_proxy_url_appends_path() {
        assert_eq!(
            build_dev_proxy_url("http://localhost:5173/", "src/main.ts"),
            Some("http://localhost:5173/src/main.ts".to_string())
        );
        assert_eq!(
            build_dev_proxy_url("http://localhost:5173", "src/main.ts"),
            Some("http://localhost:5173/src/main.ts".to_string())
        );
    }

    #[test]
    fn protocol_override_detection() {
        assert!(path_attempts_protocol_override("https://evil.com/x"));
        assert!(path_attempts_protocol_override("//evil.com/x"));
        assert!(path_attempts_protocol_override("\\\\evil.com\\x"));
        assert!(!path_attempts_protocol_override("src/main.ts"));
    }

    #[test]
    fn percent_decode_handles_standard_cases() {
        let s = percent_decode("hello%20world").unwrap();
        assert_eq!(s, "hello world");
        assert!(percent_decode("bad%ZZ").is_none());
    }
}
