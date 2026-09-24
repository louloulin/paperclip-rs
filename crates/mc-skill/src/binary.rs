//! 「这个文件是不是二进制」的判定（导入时静默跳过）。
//!
//! - **写者**：M6-2（`docs/57` §3.2）。
//! - **上游**：`internal/skill/binary.go` 的 `IsLikelyBinaryFilePath`（按扩展名黑名单）。
//!   **上游只有这一个函数** —— stub 曾写「与 `IsLikelyBinaryContent`（按 NUL / 不可解码字节）」，
//!   该函数在 `internal/skill/binary.go`（40 行全文）里不存在，本仓**不实现**
//!   （登记见 `docs/32` §9.6；`skill_file.content` 的 NUL 由入库前的 `sanitize_null_bytes`
//!   处理，见 `mc-repos/src/skill/write.rs`）。
//! - **为什么必须有**：`skill_file.content` 是 `TEXT` 列。二进制字节（PNG / 字体 / 内层
//!   归档）写进去会撞 SQLSTATE 22021；上游的选择是**跳过并留日志**，而不是让整包导入失败。
//! - **逐字对齐**：`filepath.Ext` 的语义（**最后一个点**、只在最后一个路径段内找；
//!   `.gitignore` ⇒ `.gitignore`、`Makefile` ⇒ `""`）必须手工移植 —— Rust 的
//!   `Path::extension` 不一样（`.gitignore` ⇒ `None`），不要改用它。
//! - **不做什么**：不做 MIME 探测（不引新依赖）、不做内容转码（跳过就是跳过）。

/// 上游 `IsLikelyBinaryFilePath`：扩展名命中黑名单即视为二进制。
pub fn is_likely_binary_file_path(path: &str) -> bool {
    let ext = extension(path).to_lowercase();
    matches!(
        ext.as_str(),
        // images
        ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp" | ".bmp" | ".tiff" | ".ico" | ".heic"
        // fonts
        | ".ttf" | ".otf" | ".woff" | ".woff2" | ".eot"
        // archives
        | ".zip" | ".gz" | ".tar" | ".bz2" | ".7z" | ".rar"
        // documents (binary office)
        | ".pdf" | ".docx" | ".xlsx" | ".pptx" | ".doc" | ".xls" | ".ppt"
        // media
        | ".mp3" | ".mp4" | ".wav" | ".avi" | ".mov" | ".webm" | ".m4a" | ".flac"
        // compiled / executable
        | ".exe" | ".dll" | ".so" | ".dylib" | ".class" | ".jar" | ".wasm"
        // db / cache
        | ".db" | ".sqlite" | ".sqlite3" | ".pyc"
    )
}

/// Go `filepath.Ext` 的 Unix 移植：从末尾往回找**最后一个点**，遇路径分隔符即停。
fn extension(path: &str) -> &str {
    for (index, byte) in path.bytes().enumerate().rev() {
        if byte == b'/' {
            return "";
        }
        if byte == b'.' {
            return &path[index..];
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_upstream_blacklist() {
        for path in [
            "logo.png",
            "a/b/Logo.PNG",
            "font.WOFF2",
            "pkg.tar.gz",
            "report.pdf",
            "docs/old.docx",
            "clip.mp4",
            "lib/thing.so",
            "cache.db",
            "mod.pyc",
            "archive/thing.zip",
            "C:\\dir\\file.png", // 反斜杠不是 Unix 分隔符，但点仍在末段
        ] {
            assert!(is_likely_binary_file_path(path), "{path} should be binary");
        }
    }

    #[test]
    fn text_paths_pass() {
        for path in [
            "SKILL.md",
            "README.md",
            "src/main.rs",
            "Makefile",
            ".gitignore",
            "no-extension",
            "notes.txt",
            "dir/",
            "",
            "weird.name.unknown",
        ] {
            assert!(
                !is_likely_binary_file_path(path),
                "{path} should not be binary"
            );
        }
    }

    /// 黑名单只按**最后一个点**判：`pkg.tar.gz` 是归档，`a.png.txt` 是文本。
    #[test]
    fn only_the_last_extension_counts() {
        assert!(is_likely_binary_file_path("pkg.tar.gz"));
        assert!(!is_likely_binary_file_path("a.png.txt"));
        assert!(!is_likely_binary_file_path("a.zip.md"));
    }

    /// Go `filepath.Ext` 与 Rust `Path::extension` 的两处已知分歧 —— 钉住我们移植的那一版。
    #[test]
    fn extension_matches_go_filepath_ext() {
        assert_eq!(extension(".gitignore"), ".gitignore");
        assert_eq!(extension("Makefile"), "");
        assert_eq!(extension("dir.d/file"), "");
        assert_eq!(extension("a/b.c"), ".c");
        assert_eq!(extension("a.b/c.d"), ".d");
    }
}
