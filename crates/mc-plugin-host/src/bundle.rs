//! 插件包的**入口白名单**与体积上限校验（zip 条目级）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。M6-5 的 `packages.rs` 只读。
//! - **上游**：`pkg/plugincontract/bundle.go` —— `BundleFile`(51) / `Bundle`(57) /
//!   `surfaceModuleSyntaxVisitor`(282)。
//! - **两条硬纪律（照抄，不要放宽）**：
//!   1. **条目白名单**：包里的每个条目都要过允许列表/路径规范化（拒 `..`、拒绝对路径、
//!      拒符号链接式越界）。这不是「防御性编程」，是插件包不可信的**唯一**边界；
//!   2. **体积上限**：单文件与整包两个上限（`docs/57` §4.2 M6-1 的 `MaxBundleSize`），
//!      超限整包失败而不是截断。
//! - **本仓约定**：`zip` 版本停在 workspace 的 `2.x` 且只开 `deflate`（见根 `Cargo.toml`
//!   注释）；解包在内存里做（不落临时目录）；`sha256` 用纯 hex（`plugin_package_version.digest`
//!   有 `char_length = 64` 的 CHECK，带 `sha256:` 前缀会直接撞约束）。
//! - **不做什么**：不做 zip 加密包（`aes-crypto` 特征故意没开）；不做签名/验签（上游这一代没有）。
//!
//! **状态：M6-1 已落地。**
//!
//! # 两个入口、一套判据
//!
//! | 入口 | 来源 | 前缀 |
//! | --- | --- | --- |
//! | [`parse_bundle`] | 上传的 zip 字节 | 自动识别（`manifestPrefix`） |
//! | [`parse_bundle_from_dir`] | 本地开发目录（调用方给**已做包含性检查**的读回调） | 无 |
//!
//! 两者都汇到私有的 `build_bundle`，所以「上传的包」与「本地目录」不可能被两套标准校验
//! （上游同样的取舍，见 `bundle.go:156`）。
//!
//! # 已知差异（`docs/32` §9 已登记）
//!
//! 1. **JS 校验不是解析器**：上游用 `tdewolff/parse/v2/js` 建 AST；本 crate 不引三方依赖，
//!    改为手写词法扫描（`js.rs`），只抓词法错误与模块专用语法，详见 `js.rs` 头注；
//! 2. **不做 canonical 化**：上游 `ParseManifest` 会 `json.Marshal` 出一份 `Canonical`，
//!    本仓落库的是**原样字节**（`manifest.rs` 头注），因此 [`Bundle::manifest_raw`] 就是
//!    调用方交上来的那份；键序/空白等差异不影响后续读取（都走 `parse_manifest`）。

use std::io::{Cursor, Read};

use crate::manifest::{parse_manifest, Manifest, ManifestError, MANIFEST_FILENAME};

mod js;

/// 上传的压缩包本体的上限（上游 `MaxBundleSize`）。
pub const MAX_BUNDLE_SIZE: usize = 2 << 20;

/// 包内**单个文件**的上限（上游 `MaxBundleFileSize`）。
pub const MAX_BUNDLE_FILE_SIZE: usize = 1 << 20;

/// 版本实际保留的文件**总和**上限（按解压后的字节数累计，上游 `MaxBundleTotalSize`）。
pub const MAX_BUNDLE_TOTAL_SIZE: usize = 4 << 20;

/// 压缩包**条目**数上限（含 manifest 从不引用的条目，上游 `MaxBundleEntries`）。
pub const MAX_BUNDLE_ENTRIES: usize = 512;

/// 单个 `SKILL.md` 的上限（上游 `MaxSkillBytes`）。
pub const MAX_SKILL_BYTES: usize = 256 * 1024;

/// 包内一个被保留的文件（上游 `BundleFile`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFile {
    /// manifest 自己的相对路径（serve 时不再二次解析）。
    pub path: String,
    /// 文件内容（解压后）。
    pub content: Vec<u8>,
}

/// 解析并校验后的插件包（上游 `Bundle`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Bundle {
    /// 校验通过的 manifest。
    pub manifest: Manifest,
    /// 落库用的 manifest 字节（本仓**不做** canonical 化，见文件头注 2）。
    pub manifest_raw: Vec<u8>,
    /// **只**保留 manifest 引用到的条目，按 `path` 升序。
    ///
    /// 包里其余条目一律丢掉：没有任何读路径能取到它们，留着只是占存储（上游同义）。
    pub files: Vec<BundleFile>,
}

impl Bundle {
    /// 按 manifest 相对路径取一个文件（上游 `Bundle.File`）。
    #[must_use]
    pub fn file(&self, entry: &str) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|file| file.path == entry)
            .map(|file| file.content.as_slice())
    }

    /// 该版本保留的文件总字节数（上游 `Bundle.TotalSize`）。
    #[must_use]
    pub fn total_size(&self) -> usize {
        self.files.iter().map(|file| file.content.len()).sum()
    }
}

/// 包（或 manifest / surface / skill）层面的拒绝理由。
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// 空包。
    #[error("plugin package is empty")]
    Empty,
    /// 压缩包本体超限。
    #[error("plugin package exceeds {limit} bytes")]
    ArchiveTooLarge {
        /// [`MAX_BUNDLE_SIZE`]。
        limit: usize,
    },
    /// 不是（能读的）zip。
    #[error("plugin package must be a zip archive: {reason}")]
    NotZip {
        /// `zip` crate 的原始报错。
        reason: String,
    },
    /// 条目数超限。
    #[error("plugin package contains more than {limit} entries")]
    TooManyEntries {
        /// [`MAX_BUNDLE_ENTRIES`]。
        limit: usize,
    },
    /// 条目是符号链接。
    #[error("plugin package must not contain symlinks ({name})")]
    Symlink {
        /// 条目名。
        name: String,
    },
    /// 条目用反斜杠。
    #[error("plugin package entry {name:?} must use forward slashes")]
    Backslash {
        /// 条目名。
        name: String,
    },
    /// 条目不是纯相对路径（绝对路径 / `..` / 非规范化）。
    #[error("plugin package entry {name:?} must be a plain relative path")]
    NotRelative {
        /// 条目名。
        name: String,
    },
    /// 找不到 manifest。
    #[error("plugin package must contain {wanted}")]
    MissingManifest {
        /// 期望的（可能带目录前缀的）manifest 路径。
        wanted: String,
    },
    /// manifest 埋得太深（允许根目录或**一层**目录）。
    #[error(
        "multica.plugin.json must be at the root of the package, or one directory below it"
    )]
    ManifestTooDeep,
    /// 包里不止一个 manifest。
    #[error("plugin package contains more than one multica.plugin.json")]
    MultipleManifests,
    /// manifest 声明的条目不在包里。
    #[error("plugin package is missing {entry:?}, which the manifest declares")]
    MissingEntry {
        /// manifest 里的相对路径。
        entry: String,
    },
    /// 单文件超限。
    #[error("plugin file {entry:?} exceeds {limit} bytes")]
    FileTooLarge {
        /// 条目名。
        entry: String,
        /// [`MAX_BUNDLE_FILE_SIZE`]。
        limit: usize,
    },
    /// 累计体积超限。
    #[error("plugin package files exceed {limit} bytes")]
    TotalTooLarge {
        /// [`MAX_BUNDLE_TOTAL_SIZE`]。
        limit: usize,
    },
    /// surface 入口是空白。
    #[error("surface entry {entry:?} is empty")]
    SurfaceEmpty {
        /// 入口路径。
        entry: String,
    },
    /// surface 入口不是 UTF-8。
    #[error("surface entry {entry:?} must be UTF-8 text")]
    SurfaceNotUtf8 {
        /// 入口路径。
        entry: String,
    },
    /// surface 入口不是合法 JavaScript（词法层面）。
    #[error("surface entry {entry:?} is not valid JavaScript: {reason}")]
    SurfaceInvalidJs {
        /// 入口路径。
        entry: String,
        /// 词法扫描的报错。
        reason: String,
    },
    /// surface 入口用了模块专用语法。
    #[error(
        "surface entry {entry:?} has top-level import/export/await or import.meta; a surface is a single classic script with no module graph, so bundle its dependencies in"
    )]
    SurfaceModuleSyntax {
        /// 入口路径。
        entry: String,
    },
    /// skill 资源是空白。
    #[error("skill resource {entry:?} is empty")]
    SkillEmpty {
        /// 入口路径。
        entry: String,
    },
    /// skill 资源不是 UTF-8。
    #[error("skill resource {entry:?} must be UTF-8 text")]
    SkillNotUtf8 {
        /// 入口路径。
        entry: String,
    },
    /// skill 资源超限。
    #[error("skill resource {entry:?} exceeds {limit} bytes")]
    SkillTooLarge {
        /// 入口路径。
        entry: String,
        /// [`MAX_SKILL_BYTES`]。
        limit: usize,
    },
    /// 图标是空文件。
    #[error("plugin icon {entry:?} is empty")]
    IconEmpty {
        /// 图标路径。
        entry: String,
    },
    /// 读某个条目时出错。
    #[error("read plugin file {name:?}: {reason}")]
    Read {
        /// 条目名。
        name: String,
        /// 底层报错。
        reason: String,
    },
    /// manifest 自己的校验错误**原样**透出（调用方按 `ManifestError::code` 分类）。
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

impl BundleError {
    /// 稳定错误码（route 层据此降级，不要改）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Manifest(error) => error.code(),
            _ => "plugin_package_invalid",
        }
    }
}

/// 读一个条目的回调：`Ok(None)` = 条目不存在。
type ReadEntry<'a> = dyn FnMut(&str) -> Result<Option<Vec<u8>>, BundleError> + 'a;

/// 从上传的 zip 字节解析出已校验的包（上游 `ParseBundle`）。
///
/// # Errors
///
/// 见 [`BundleError`]：空包 / 超限 / 非 zip / 条目非法 / manifest 非法 / 引用缺文件。
pub fn parse_bundle(archive: &[u8]) -> Result<Bundle, BundleError> {
    if archive.is_empty() {
        return Err(BundleError::Empty);
    }
    if archive.len() > MAX_BUNDLE_SIZE {
        return Err(BundleError::ArchiveTooLarge {
            limit: MAX_BUNDLE_SIZE,
        });
    }
    let mut reader =
        zip::ZipArchive::new(Cursor::new(archive)).map_err(|error| BundleError::NotZip {
            reason: error.to_string(),
        })?;
    if reader.len() > MAX_BUNDLE_ENTRIES {
        return Err(BundleError::TooManyEntries {
            limit: MAX_BUNDLE_ENTRIES,
        });
    }
    let entries = zip_entry_paths(&mut reader)?;
    let prefix = manifest_prefix(&entries)?;

    // 条目**按需**解压：只有 manifest 点名的文件会被读出来，累计体积边读边查 ——
    // 一个塞满大体积废条目的包不会先被缓冲再被拒（上游同义）。
    let mut total = 0usize;
    let mut open = |entry: &str| -> Result<Option<Vec<u8>>, BundleError> {
        let name = format!("{prefix}{entry}");
        let Ok(file) = reader.by_name(&name) else {
            return Ok(None);
        };
        let content = read_zip_file(file)?;
        total += content.len();
        if total > MAX_BUNDLE_TOTAL_SIZE {
            return Err(BundleError::TotalTooLarge {
                limit: MAX_BUNDLE_TOTAL_SIZE,
            });
        }
        Ok(Some(content))
    };
    build_bundle(&mut open, &prefix)
}

/// 从本地开发目录构建同一个已校验的包（上游 `ParseBundleFromDir`）。
///
/// `read` 由调用方提供，并且**必须**自己做「不越出目录」的包含性检查 —— 本模块绝不碰文件系统。
///
/// # Errors
///
/// 见 [`BundleError`]。
pub fn parse_bundle_from_dir(
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, BundleError>,
) -> Result<Bundle, BundleError> {
    let mut total = 0usize;
    let mut open = |entry: &str| -> Result<Option<Vec<u8>>, BundleError> {
        let Some(content) = read(entry)? else {
            return Ok(None);
        };
        if content.len() > MAX_BUNDLE_FILE_SIZE {
            return Err(BundleError::FileTooLarge {
                entry: entry.to_owned(),
                limit: MAX_BUNDLE_FILE_SIZE,
            });
        }
        total += content.len();
        if total > MAX_BUNDLE_TOTAL_SIZE {
            return Err(BundleError::TotalTooLarge {
                limit: MAX_BUNDLE_TOTAL_SIZE,
            });
        }
        Ok(Some(content))
    };
    build_bundle(&mut open, "")
}

/// 两条发布路径的公共后半段：解析 manifest，然后**只**收集它引用的文件。
fn build_bundle(open: &mut ReadEntry<'_>, prefix: &str) -> Result<Bundle, BundleError> {
    let Some(raw_manifest) = open(MANIFEST_FILENAME)? else {
        return Err(BundleError::MissingManifest {
            wanted: format!("{prefix}{MANIFEST_FILENAME}"),
        });
    };
    let manifest = parse_manifest(&raw_manifest)?;

    let mut files: Vec<BundleFile> = Vec::with_capacity(
        manifest.contributes.surfaces.len() + manifest.contributes.resources.len() + 1,
    );
    for surface in &manifest.contributes.surfaces {
        let content = collect(open, &mut files, &surface.entry)?;
        validate_surface_script(&surface.entry, &content)?;
    }
    for resource in &manifest.contributes.resources {
        let content = collect(open, &mut files, &resource.entry)?;
        validate_skill_file(&resource.entry, &content)?;
    }
    if !manifest.icon.is_empty() {
        let content = collect(open, &mut files, &manifest.icon)?;
        if content.is_empty() {
            return Err(BundleError::IconEmpty {
                entry: manifest.icon.clone(),
            });
        }
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Bundle {
        manifest,
        manifest_raw: raw_manifest,
        files,
    })
}

/// 收集一个被 manifest 引用的条目；已收集过的直接返回（上游 `seen` + `findFile` 的合并）。
fn collect(
    open: &mut ReadEntry<'_>,
    files: &mut Vec<BundleFile>,
    entry: &str,
) -> Result<Vec<u8>, BundleError> {
    if let Some(existing) = files.iter().find(|file| file.path == entry) {
        // 两个贡献可以合法地指向同一个文件；存两份会撞 `(version_id, path)` 唯一索引。
        return Ok(existing.content.clone());
    }
    let Some(content) = open(entry)? else {
        return Err(BundleError::MissingEntry {
            entry: entry.to_owned(),
        });
    };
    if content.len() > MAX_BUNDLE_FILE_SIZE {
        return Err(BundleError::FileTooLarge {
            entry: entry.to_owned(),
            limit: MAX_BUNDLE_FILE_SIZE,
        });
    }
    files.push(BundleFile {
        path: entry.to_owned(),
        content: content.clone(),
    });
    Ok(content)
}

/// surface 入口：非空白、UTF-8、能过词法扫描，且**没有**模块专用语法（上游 `validateSurfaceScript`）。
fn validate_surface_script(entry: &str, content: &[u8]) -> Result<(), BundleError> {
    let Ok(source) = std::str::from_utf8(content) else {
        return Err(BundleError::SurfaceNotUtf8 {
            entry: entry.to_owned(),
        });
    };
    if source.trim().is_empty() {
        return Err(BundleError::SurfaceEmpty {
            entry: entry.to_owned(),
        });
    }
    let module_syntax = js::scan_module_syntax(source).map_err(|error| BundleError::SurfaceInvalidJs {
        entry: entry.to_owned(),
        reason: error.to_string(),
    })?;
    if module_syntax {
        return Err(BundleError::SurfaceModuleSyntax {
            entry: entry.to_owned(),
        });
    }
    Ok(())
}

/// skill 资源：非空白、UTF-8、≤ [`MAX_SKILL_BYTES`]（上游 `validateSkillFile`）。
fn validate_skill_file(entry: &str, content: &[u8]) -> Result<(), BundleError> {
    let Ok(source) = std::str::from_utf8(content) else {
        return Err(BundleError::SkillNotUtf8 {
            entry: entry.to_owned(),
        });
    };
    if source.trim().is_empty() {
        return Err(BundleError::SkillEmpty {
            entry: entry.to_owned(),
        });
    }
    if content.len() > MAX_SKILL_BYTES {
        return Err(BundleError::SkillTooLarge {
            entry: entry.to_owned(),
            limit: MAX_SKILL_BYTES,
        });
    }
    Ok(())
}

/// 收集（并拒绝）zip 里的条目**路径**：目录条目丢弃，符号链接/反斜杠/非相对路径直接拒。
fn zip_entry_paths<R: Read + std::io::Seek>(
    reader: &mut zip::ZipArchive<R>,
) -> Result<Vec<String>, BundleError> {
    let mut paths = Vec::with_capacity(reader.len());
    for index in 0..reader.len() {
        let file = reader
            .by_index(index)
            .map_err(|error| BundleError::Read {
                name: format!("#{index}"),
                reason: error.to_string(),
            })?;
        let name = file.name().to_owned();
        if name.ends_with('/') {
            continue; // 目录条目
        }
        if file.unix_mode().is_some_and(is_symlink_mode) {
            // zip 里的符号链接把目标当内容存。这里没人会去跟随它，但存下来就等于发布一个
            // 「含义取决于某个并不存在于我们这侧的文件系统」的文件。
            return Err(BundleError::Symlink { name });
        }
        if name.contains('\\') {
            return Err(BundleError::Backslash { name });
        }
        if !is_plain_relative(&name) {
            return Err(BundleError::NotRelative { name });
        }
        paths.push(name);
    }
    Ok(paths)
}

/// zip 的 mode 位是 `S_IFLNK`（上游 `fs.ModeSymlink` 判定）。
const fn is_symlink_mode(mode: u32) -> bool {
    mode & 0o170_000 == 0o120_000
}

/// 等价上游 `path.IsAbs(name) || name != path.Clean(name) || strings.HasPrefix(name, "../")`。
///
/// `path.Clean` 的语义用「按 `/` 切段、逐段检查」复现：空段（`//`）、`.`、`..` 都让结果与原文
/// 不等（末尾的 `/` 已在目录条目那一步去掉；`a/` 这类会走到这里，但 `a/` 的最后一段是空 ⇒ 拒）。
fn is_plain_relative(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') {
        return false;
    }
    name.split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// 找到 manifest 所在的目录前缀（上游 `manifestPrefix`）。
///
/// 打包时「把文件夹压成 zip」是最常见的动作，于是包里出现 `my-plugin/multica.plugin.json`；
/// 允许**恰好一层**目录前缀把这一失败变成非事件。两个不同的根则仍是错误 —— 那时「这个包是什么」
/// 没有唯一答案。
fn manifest_prefix(paths: &[String]) -> Result<String, BundleError> {
    let mut candidates: Vec<&str> = Vec::new();
    for name in paths {
        if name == MANIFEST_FILENAME {
            return Ok(String::new());
        }
        if let Some((dir, file)) = name.rsplit_once('/') {
            if file == MANIFEST_FILENAME && !candidates.contains(&dir) {
                candidates.push(dir);
            }
        }
    }
    match candidates.len() {
        0 => Err(BundleError::MissingManifest {
            wanted: MANIFEST_FILENAME.to_owned(),
        }),
        1 => {
            let dir = candidates[0];
            if dir.split('/').count() > 1 {
                return Err(BundleError::ManifestTooDeep);
            }
            Ok(format!("{dir}/"))
        }
        _ => Err(BundleError::MultipleManifests),
    }
}

/// 读一个 zip 条目，**不信**它自己声明的解压后大小：先按声明早拒，真读时再单独限长。
fn read_zip_file(file: zip::read::ZipFile<'_>) -> Result<Vec<u8>, BundleError> {
    let name = file.name().to_owned();
    if file.size() > MAX_BUNDLE_FILE_SIZE as u64 {
        return Err(BundleError::FileTooLarge {
            entry: name,
            limit: MAX_BUNDLE_FILE_SIZE,
        });
    }
    let mut content = Vec::new();
    file.take(MAX_BUNDLE_FILE_SIZE as u64 + 1)
        .read_to_end(&mut content)
        .map_err(|error| BundleError::Read {
            name: name.clone(),
            reason: error.to_string(),
        })?;
    if content.len() > MAX_BUNDLE_FILE_SIZE {
        return Err(BundleError::FileTooLarge {
            entry: name,
            limit: MAX_BUNDLE_FILE_SIZE,
        });
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    const MANIFEST: &str = r#"{
        "manifest_version": 1,
        "key": "com.example.panel",
        "name": "Example Panel",
        "version": "1.2.0",
        "author": { "name": "Example" },
        "scopes": ["issues:read"],
        "contributes": {
            "surfaces": [{ "key": "panel", "type": "issue_panel", "name": "Panel", "entry": "panel.js" }]
        }
    }"#;

    const PANEL_JS: &str = "const root = document.getElementById(\"root\");\nroot.textContent = \"ok\";\n";

    /// 打一个 zip：`(路径, 内容)` 列表，路径带 `/` 结尾就是目录条目。
    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, content) in entries {
            if name.ends_with('/') {
                writer
                    .add_directory(*name, options)
                    .expect("目录条目应当能写");
            } else {
                writer.start_file(*name, options).expect("条目应当能写");
                writer.write_all(content).expect("内容应当能写");
            }
        }
        writer.finish().expect("zip 应当能收口").into_inner()
    }

    #[test]
    fn accepts_a_minimal_package() {
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
        ]);
        let bundle = parse_bundle(&archive).expect("最小包应当合法");
        assert_eq!(bundle.manifest.key, "com.example.panel");
        assert_eq!(bundle.manifest_raw, MANIFEST.as_bytes());
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.file("panel.js"), Some(PANEL_JS.as_bytes()));
        assert_eq!(bundle.total_size(), PANEL_JS.len());
    }

    #[test]
    fn accepts_one_leading_directory() {
        let archive = zip_bytes(&[
            ("my-plugin/", &[][..]),
            ("my-plugin/multica.plugin.json", MANIFEST.as_bytes()),
            ("my-plugin/panel.js", PANEL_JS.as_bytes()),
        ]);
        let bundle = parse_bundle(&archive).expect("一层目录前缀应当被允许");
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.file("panel.js"), Some(PANEL_JS.as_bytes()));
    }

    #[test]
    fn extra_entries_are_dropped() {
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
            ("unused.js", b"/* nothing references this */"),
            (".env", b"SECRET=1"),
        ]);
        let bundle = parse_bundle(&archive).expect("多余条目应当只是被丢掉");
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.file("unused.js"), None);
        assert_eq!(bundle.file(".env"), None);
    }

    #[test]
    fn rejects_a_package_without_a_manifest() {
        let archive = zip_bytes(&[("panel.js", PANEL_JS.as_bytes())]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::MissingManifest { .. })
        ));
    }

    #[test]
    fn rejects_two_manifest_directories_but_a_root_manifest_wins() {
        // 没有根 manifest、且有两个不同的候选目录 ⇒ 没有唯一答案，拒。
        let archive = zip_bytes(&[
            ("one/multica.plugin.json", MANIFEST.as_bytes()),
            ("one/panel.js", PANEL_JS.as_bytes()),
            ("two/multica.plugin.json", MANIFEST.as_bytes()),
            ("two/panel.js", PANEL_JS.as_bytes()),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::MultipleManifests)
        ));

        // 有根 manifest 时根**赢**（上游 `manifestPrefix` 见到根 manifest 立刻返回，与顺序无关）。
        let archive = zip_bytes(&[
            ("other/multica.plugin.json", MANIFEST.as_bytes()),
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
        ]);
        let bundle = parse_bundle(&archive).expect("根 manifest 应当胜出");
        assert_eq!(bundle.file("panel.js"), Some(PANEL_JS.as_bytes()));
    }

    #[test]
    fn rejects_a_deeply_nested_manifest() {
        let archive = zip_bytes(&[("a/b/multica.plugin.json", MANIFEST.as_bytes())]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::ManifestTooDeep)
        ));
    }

    #[test]
    fn rejects_a_missing_entry_the_manifest_declares() {
        let archive = zip_bytes(&[(MANIFEST_FILENAME, MANIFEST.as_bytes())]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::MissingEntry { entry }) if entry == "panel.js"
        ));
    }

    #[test]
    fn rejects_traversal_and_absolute_entries() {
        for name in ["../evil.js", "/etc/passwd", "a/../b.js", "a//b.js", "./panel.js"] {
            let archive = zip_bytes(&[
                (MANIFEST_FILENAME, MANIFEST.as_bytes()),
                ("panel.js", PANEL_JS.as_bytes()),
                (name, b"x"),
            ]);
            assert!(
                matches!(
                    parse_bundle(&archive),
                    Err(BundleError::NotRelative { .. })
                ),
                "{name} 应当被拒"
            );
        }
    }

    #[test]
    fn rejects_backslashes_because_zip_paths_are_slashed() {
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
            ("sub\\evil.js", b"x"),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::Backslash { .. })
        ));
    }

    #[test]
    fn rejects_empty_and_oversized_archives() {
        assert!(matches!(parse_bundle(&[]), Err(BundleError::Empty)));
        let archive = vec![0u8; MAX_BUNDLE_SIZE + 1];
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::ArchiveTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_a_non_zip_body() {
        let error = parse_bundle(b"this is not a zip archive").expect_err("非 zip 必须被拒");
        assert!(matches!(error, BundleError::NotZip { .. }));
        assert_eq!(error.code(), "plugin_package_invalid");
    }

    #[test]
    fn rejects_too_many_entries() {
        let mut entries: Vec<(String, &[u8])> = vec![(MANIFEST_FILENAME.to_owned(), MANIFEST.as_bytes())];
        let filler = "x";
        for index in 0..MAX_BUNDLE_ENTRIES {
            entries.push((format!("filler-{index}.bin"), filler.as_bytes()));
        }
        let owned: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(name, content)| (name.as_str(), *content))
            .collect();
        let archive = zip_bytes(&owned);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::TooManyEntries { .. })
        ));
    }

    #[test]
    fn rejects_an_oversized_file_but_ignores_oversized_unreferenced_ones() {
        // 被 manifest 引用的文件按单文件上限拒。
        let big = vec![b'/'; MAX_BUNDLE_FILE_SIZE + 1];
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", big.as_slice()),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::FileTooLarge { .. })
        ));

        // 没被引用的文件压根不解压 ⇒ 它多大都只是被丢掉（上游「惰性读」的原意）。
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
            ("big.js", big.as_slice()),
        ]);
        let bundle = parse_bundle(&archive).expect("未引用的大文件应当只是被丢掉");
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.file("big.js"), None);
    }

    #[test]
    fn rejects_a_package_whose_referenced_files_exceed_the_total() {
        let manifest = r#"{
            "manifest_version": 1,
            "key": "com.example.big",
            "name": "Big Panel",
            "version": "1.0.0",
            "author": { "name": "Example" },
            "scopes": ["issues:read"],
            "contributes": {
                "surfaces": [
                    { "key": "one", "type": "issue_panel", "name": "One", "entry": "one.js" },
                    { "key": "two", "type": "issue_panel", "name": "Two", "entry": "two.js" },
                    { "key": "three", "type": "issue_panel", "name": "Three", "entry": "three.js" },
                    { "key": "four", "type": "issue_panel", "name": "Four", "entry": "four.js" },
                    { "key": "five", "type": "issue_panel", "name": "Five", "entry": "five.js" }
                ]
            }
        }"#;
        let chunk = 900 * 1024;
        let error = parse_bundle_from_dir(|entry| {
            if entry == MANIFEST_FILENAME {
                return Ok(Some(manifest.as_bytes().to_vec()));
            }
            let mut content = vec![b' '; chunk];
            content.extend_from_slice(b"a = 1;\n");
            Ok(Some(content))
        })
        .expect_err("累计超过 4 MiB 必须被拒");
        assert!(matches!(error, BundleError::TotalTooLarge { .. }));
        assert_eq!(error.to_string(), "plugin package files exceed 4194304 bytes");
    }

    #[test]
    fn rejects_a_manifest_that_fails_its_own_validation() {
        let broken = MANIFEST.replace("\"version\": \"1.2.0\"", "\"version\": \"1.2\"");
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, broken.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::Manifest(ManifestError::Invalid(_)))
        ));
    }

    #[test]
    fn surface_entry_rules_are_the_do_d_counterexamples() {
        // 1) 空白 surface 入口
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", b"   \n\t"),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::SurfaceEmpty { .. })
        ));

        // 2) 非 UTF-8
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", &[0xff, 0xfe, 0x00]),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::SurfaceNotUtf8 { .. })
        ));

        // 3) 词法上就不是 JS（未闭合字符串）
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", b"const a = 'oops;"),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::SurfaceInvalidJs { .. })
        ));

        // 4) 模块专用语法
        for source in [
            "import { open } from \"@multica/plugin-sdk\";",
            "export default function panel() {}",
            "await boot();",
            "console.log(import.meta.url);",
        ] {
            let archive = zip_bytes(&[
                (MANIFEST_FILENAME, MANIFEST.as_bytes()),
                ("panel.js", source.as_bytes()),
            ]);
            assert!(
                matches!(
                    parse_bundle(&archive),
                    Err(BundleError::SurfaceModuleSyntax { .. })
                ),
                "{source} 应当被拒"
            );
        }
    }

    #[test]
    fn skill_resources_are_text_and_bounded() {
        let manifest = MANIFEST.replace(
            "\"surfaces\": [{ \"key\": \"panel\", \"type\": \"issue_panel\", \"name\": \"Panel\", \"entry\": \"panel.js\" }]",
            "\"resources\": [{ \"type\": \"skill\", \"key\": \"guide\", \"entry\": \"skills/guide/SKILL.md\" }]",
        );

        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, manifest.as_bytes()),
            ("skills/guide/SKILL.md", b"# Guide\n\nSteps.\n"),
        ]);
        let bundle = parse_bundle(&archive).expect("skill 包应当合法");
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.file("skills/guide/SKILL.md"), Some(&b"# Guide\n\nSteps.\n"[..]));

        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, manifest.as_bytes()),
            ("skills/guide/SKILL.md", b"  \n"),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::SkillEmpty { .. })
        ));

        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, manifest.as_bytes()),
            ("skills/guide/SKILL.md", &[0xff, 0x00]),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::SkillNotUtf8 { .. })
        ));

        let oversized = vec![b'#'; MAX_SKILL_BYTES + 1];
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, manifest.as_bytes()),
            ("skills/guide/SKILL.md", oversized.as_slice()),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::SkillTooLarge { .. })
        ));
    }

    #[test]
    fn icon_must_exist_and_be_non_empty() {
        let manifest = MANIFEST.replace(
            "\"scopes\": [\"issues:read\"],",
            "\"scopes\": [\"issues:read\"],\n        \"icon\": \"icon.svg\",",
        );
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, manifest.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::MissingEntry { entry }) if entry == "icon.svg"
        ));

        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, manifest.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
            ("icon.svg", b""),
        ]);
        assert!(matches!(
            parse_bundle(&archive),
            Err(BundleError::IconEmpty { .. })
        ));

        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, manifest.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
            ("icon.svg", b"<svg/>"),
        ]);
        let bundle = parse_bundle(&archive).expect("带图标的包应当合法");
        assert_eq!(bundle.files.len(), 2);
        assert_eq!(bundle.files[0].path, "icon.svg");
        assert_eq!(bundle.files[1].path, "panel.js");
    }

    #[test]
    fn dir_channel_shares_the_same_judgement() {
        let files: Vec<(&str, &[u8])> = vec![
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
            ("extra.js", "// 未引用".as_bytes()),
        ];
        let bundle = parse_bundle_from_dir(|entry| {
            Ok(files
                .iter()
                .find(|(name, _)| *name == entry)
                .map(|(_, content)| (*content).to_vec()))
        })
        .expect("目录通道应当合法");
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.manifest_raw, MANIFEST.as_bytes());

        let error = parse_bundle_from_dir(|_| Ok(None)).expect_err("没有 manifest 必须被拒");
        assert_eq!(
            error.to_string(),
            "plugin package must contain multica.plugin.json"
        );

        let error = parse_bundle_from_dir(|entry| {
            Ok(Some(match entry {
                "multica.plugin.json" => MANIFEST.as_bytes().to_vec(),
                "panel.js" => b"export default 1;".to_vec(),
                _ => return Ok(None),
            }))
        })
        .expect_err("模块语法在目录通道也必须被拒");
        assert!(matches!(error, BundleError::SurfaceModuleSyntax { .. }));
    }

    #[test]
    fn error_messages_match_upstream_wording() {
        assert_eq!(
            BundleError::FileTooLarge {
                entry: "panel.js".to_owned(),
                limit: MAX_BUNDLE_FILE_SIZE,
            }
            .to_string(),
            "plugin file \"panel.js\" exceeds 1048576 bytes"
        );
        assert_eq!(
            BundleError::MultipleManifests.to_string(),
            "plugin package contains more than one multica.plugin.json"
        );
        assert_eq!(
            BundleError::SurfaceModuleSyntax {
                entry: "panel.js".to_owned(),
            }
            .to_string(),
            "surface entry \"panel.js\" has top-level import/export/await or import.meta; a surface is a single classic script with no module graph, so bundle its dependencies in"
        );
        assert_eq!(
            BundleError::MissingEntry {
                entry: "skills/guide/SKILL.md".to_owned(),
            }
            .to_string(),
            "plugin package is missing \"skills/guide/SKILL.md\", which the manifest declares"
        );
    }

    #[test]
    fn plain_relative_paths_follow_path_clean() {
        assert!(is_plain_relative("panel.js"));
        assert!(is_plain_relative("skills/guide/SKILL.md"));
        assert!(!is_plain_relative(""));
        assert!(!is_plain_relative("/panel.js"));
        assert!(!is_plain_relative("./panel.js"));
        assert!(!is_plain_relative("a/../panel.js"));
        assert!(!is_plain_relative("a//panel.js"));
        assert!(!is_plain_relative("a/"));
    }
}
