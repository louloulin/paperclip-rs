//! zip 归档的**解包与上限**（`POST /api/skills/import` 的 multipart 包 / 预览解析）。
//!
//! - **写者**：M6-3（`docs/57` §3.2：`mc-skill/src/{archive,source}.rs` | M6-3 写）。
//! - **上游**：`internal/handler/skill_import_archive.go`。
//! - **两条硬上限（照抄，不要「宽松一点」）**：单包文件数上限与整包字节上限。超限必须
//!   **整包失败**（上游 `errImportCapExceeded` ⇒ 413），不能截断 —— 截断会产出一个「看起来
//!   合法」的不完整 skill（上游注释就是这么写的）。
//! - **本仓约定**：`zip` 的版本停在 workspace 的 `2.x` 且只开 `deflate` 特征（见根
//!   `Cargo.toml` 的注释：6/8 的 MSRV 高于本仓 `rust-version`）；解包**只在内存里**做
//!   （本 crate 不写磁盘、不建临时目录）；路径先过 `reserved` / 二进制判定再过调用方。
//! - **本文件同时是「导入包」的共享类型家**：`ImportedSkill`（三条取件路径都往它上面
//!   `add_file`）与 `ImportError`（413 / 503 / 400 的分类）。放这里而不是新开模块，是因为
//!   `src/lib.rs` 的模块清单由 M6-0 anchor 冻结（`docs/32` §9 已登记）。
//! - **不做什么**：不做 zip 加密包（我们的包都是自己产的，`aes-crypto` 特征故意没开）；
//!   不落库（`skill_file` 行的写入归 M6-3 的 `routes/skills/import.rs` + `mc-repos`）。
//!
//! **状态：M6-3 已落地（LUM-1668）**。
//!
//! 行预算（门 ⑩）：桩写 180 行，落地 561 行（含 15 条用例）。

use std::io::{Cursor, Read};

use crate::binary::is_likely_binary_file_path;
use crate::frontmatter::parse_skill_frontmatter;
use crate::reserved::{clean_path, CONTENT_FILENAME};

/// 上游 `maxImportFileSize`：单个文件 1 MiB。
pub const MAX_IMPORT_FILE_SIZE: u64 = 1 << 20;
/// 上游 `maxImportTotalSize`：整包支持文件合计 8 MiB。
pub const MAX_IMPORT_TOTAL_SIZE: usize = 8 << 20;
/// 上游 `maxImportFileCount`：整包支持文件条数上限。
pub const MAX_IMPORT_FILE_COUNT: usize = 256;
/// 上游 `maxImportArchiveUploadSize`：压缩包本体 16 MiB（解包后另受上面三条约束）。
pub const MAX_IMPORT_ARCHIVE_UPLOAD_SIZE: usize = 16 << 20;

/// 上游 `errImportCapExceeded` / `errImportSourceUnavailable` 的分类面。
///
/// route 层用它决定状态码：`Cap` ⇒ 413、`SourceUnavailable` ⇒ 503（可重试）、
/// `Invalid` ⇒ 400/502。上游用 `errors.Is` 判，这里用枚举（Rust 没有 `errors.Is` 的
/// 包装链，枚举就是它的等价物）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFailure {
    /// 上游 `errImportCapExceeded`。
    Cap,
    /// 上游 `errImportSourceUnavailable`。
    SourceUnavailable,
    /// **上游没有这个哨兵**：上游用 `context.DeadlineExceeded` + `ctx.Err()` 判整轮取件超时
    /// （`importFetchErrorResponse` 的首个 504 分支）。Rust 侧超时由 `tokio::time::timeout`
    /// 在 route 层产生，没有可包装的 `error`，所以在这里补一个分类，语义与上游那条分支等价。
    Timeout,
    /// 取件成功但包本身不合法（源返回的不是 skill / slug 解析失败等）。
    Invalid,
}

/// 导入过程中的错误：`message` 是上游原文（route 层直接塞进 4xx/5xx 的 error 字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError {
    pub failure: ImportFailure,
    pub message: String,
}

impl ImportError {
    pub fn cap(message: impl Into<String>) -> Self {
        Self {
            failure: ImportFailure::Cap,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            failure: ImportFailure::Invalid,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            failure: ImportFailure::SourceUnavailable,
            message: message.into(),
        }
    }

    /// 整轮取件超时（上游 `context.DeadlineExceeded` 分支 ⇒ 504）。
    pub fn timeout(message: impl Into<String>) -> Self {
        Self {
            failure: ImportFailure::Timeout,
            message: message.into(),
        }
    }

    /// 上游 `isCapError`。
    pub fn is_cap(&self) -> bool {
        self.failure == ImportFailure::Cap
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ImportError {}

/// 一条支持文件（上游 `importedFile`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedFile {
    pub path: String,
    pub content: String,
}

/// 从外部源解出的 skill 包（上游 `importedSkill`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportedSkill {
    pub name: String,
    pub description: String,
    /// `SKILL.md` 正文（落 `skill.content`）。
    pub content: String,
    pub files: Vec<ImportedFile>,
    /// 上游 `bundleSize`：支持文件字节数的滚动和（正文不算）。
    pub bundle_size: usize,
}

impl ImportedSkill {
    /// 上游 `newSkillsShImportedSkill` 一类构造：只给头部字段。
    pub fn new(name: String, description: String, content: String) -> Self {
        Self {
            name,
            description,
            content,
            files: Vec::new(),
            bundle_size: 0,
        }
    }

    /// 上游 `importedSkill.addFile`：加一条支持文件并校验整包两条上限。
    ///
    /// 二进制文件**静默跳过**（`skill_file.content` 是 `TEXT` 列，二进制字节会撞
    /// SQLSTATE 22021；它们本来就是 agent 不会当文本读的参考素材）。
    pub fn add_file(
        &mut self,
        path: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<(), ImportError> {
        let path = path.into();
        let content = content.into();
        if is_likely_binary_file_path(&path) {
            tracing::info!(
                path,
                size = content.len(),
                "skill import: skipping binary file"
            );
            return Ok(());
        }
        if self.files.len() >= MAX_IMPORT_FILE_COUNT {
            return Err(ImportError::cap(format!(
                "import bundle exceeds {MAX_IMPORT_FILE_COUNT} file limit"
            )));
        }
        if self.bundle_size + content.len() > MAX_IMPORT_TOTAL_SIZE {
            return Err(ImportError::cap(format!(
                "import bundle exceeds {MAX_IMPORT_TOTAL_SIZE} byte limit"
            )));
        }
        self.bundle_size += content.len();
        self.files.push(ImportedFile { path, content });
        Ok(())
    }

    /// 上游 `sort.Slice(imported.files, by path)`：包内文件顺序稳定（与下载时序无关）。
    pub fn sort_files(&mut self) {
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
    }
}

/// 上游 `parseSkillArchive`：把上传的 `.skill` / `.zip` 解成一个包。
///
/// `.skill` 就是普通 zip，条目要么在根（`SKILL.md`、`scripts/…`），要么整体套在一个
/// 顶层目录下（`my-skill/SKILL.md`、`my-skill/scripts/…`，Anthropic `package_skill`
/// 的产物）。两者都接受：以**最浅的那个 `SKILL.md`** 所在目录为根。
pub fn parse_skill_archive(data: &[u8], filename: &str) -> Result<ImportedSkill, ImportError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data))
        .map_err(|_| ImportError::invalid("uploaded file is not a valid .skill/.zip archive"))?;

    // 先定位 skill 根：最浅 SKILL.md 的目录。候选路径当场过校验（绝对路径 / 穿越一律拒），
    // 免得恶意归档把不安全路径塞成「正文」。
    let mut skill_md_index: Option<usize> = None;
    let mut root_prefix = String::new();
    let mut seen_skill_md: Vec<(String, String)> = Vec::new();
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(|_| {
            ImportError::invalid("uploaded file is not a valid .skill/.zip archive")
        })?;
        if is_directory_entry(&file) {
            continue;
        }
        let raw_name = file.name().to_string();
        let clean = clean_archive_entry_name(&raw_name);
        if !base_name(&clean).eq_ignore_ascii_case(CONTENT_FILENAME) {
            continue;
        }
        if !validate_archive_file_path(&clean) {
            continue;
        }
        if let Some((previous, _)) = seen_skill_md.iter().find(|(path, _)| *path == clean) {
            return Err(ImportError::invalid(format!(
                "archive entries {previous:?} and {raw_name:?} resolve to the same path {clean:?}"
            )));
        }
        seen_skill_md.push((clean.clone(), raw_name.clone()));
        let prefix = archive_entry_prefix(&clean);
        if skill_md_index.is_none() || prefix.len() < root_prefix.len() {
            skill_md_index = Some(index);
            root_prefix = prefix;
        }
    }
    let Some(skill_md_index) = skill_md_index else {
        return Err(ImportError::invalid("archive does not contain a SKILL.md"));
    };

    let content = read_zip_entry(&mut archive, skill_md_index)
        .map_err(|message| ImportError::invalid(format!("read SKILL.md: {message}")))?;
    let frontmatter = parse_skill_frontmatter(&content);
    let mut name = frontmatter.name;
    if name.is_empty() {
        name = skill_name_from_archive(&root_prefix, filename);
    }
    if name.is_empty() {
        return Err(ImportError::invalid(
            "could not determine the skill name: SKILL.md has no name field and the archive is unnamed",
        ));
    }

    let mut imported = ImportedSkill::new(name, frontmatter.description, content);
    let mut seen_files: Vec<(String, String)> = Vec::new();
    for index in 0..archive.len() {
        // 先把条目名拷出来再释放 `ZipFile` 的借用 —— 下面 `read_zip_entry` 要再借一次 `archive`。
        let raw_name = {
            let file = archive.by_index(index).map_err(|_| {
                ImportError::invalid("uploaded file is not a valid .skill/.zip archive")
            })?;
            if is_directory_entry(&file) {
                continue;
            }
            file.name().to_string()
        };
        let clean = clean_archive_entry_name(&raw_name);
        // 只有落在 skill 根下的条目才属于这个 skill。
        if !root_prefix.is_empty() && !clean.starts_with(&root_prefix) {
            continue;
        }
        let rel = clean[root_prefix.len()..].to_string();
        if rel.is_empty() {
            continue;
        }
        // 任何深度的 SKILL.md 都不是支持文件：顶层那份是正文，嵌套那份会与保留名撞车。
        if base_name(&rel).eq_ignore_ascii_case(CONTENT_FILENAME) {
            continue;
        }
        if is_ignored_archive_entry(&rel) {
            continue;
        }
        if !validate_archive_file_path(&rel) {
            continue;
        }
        if let Some((previous, _)) = seen_files.iter().find(|(path, _)| *path == rel) {
            return Err(ImportError::invalid(format!(
                "archive entries {previous:?} and {raw_name:?} resolve to the same path {rel:?}"
            )));
        }
        seen_files.push((rel.clone(), raw_name));
        // 单个超限 / 读不出来的素材**跳过**（与本地运行时导入器一致），不整包失败。
        let Ok(file_content) = read_zip_entry(&mut archive, index) else {
            continue;
        };
        imported.add_file(rel, file_content)?;
    }
    imported.sort_files();
    Ok(imported)
}

/// `Compress-Archive` 的目录标记可能没有目录属性，只看末位反斜杠。
fn is_directory_entry(file: &zip::read::ZipFile<'_>) -> bool {
    file.is_dir() || file.name().ends_with('\\')
}

/// 上游 `cleanArchiveEntryName`：先把 `Compress-Archive` 的反斜杠规范成 `/`，再过 zip 路径语义。
fn clean_archive_entry_name(name: &str) -> String {
    clean_path(&name.replace('\\', "/"))
}

/// 上游 `validateArchiveFilePath`：在 `validateFilePath` 之上再加「Windows 盘符」与 NUL。
///
/// ⚠️ `validateFilePath` 是 route 层的入参校验（`routes/skills/helpers.rs`），本 crate
/// 不能依赖 `mc-http` ⇒ 这里留一份 8 行移植（已登记 `docs/32` §9.6）。语义与
/// `helpers::validate_file_path` 逐字相同（含 `..foo` 也被拒的上游怪癖）。
fn validate_archive_file_path(path: &str) -> bool {
    if path.contains('\0') {
        return false;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    if path.is_empty() || path.starts_with('/') {
        return false;
    }
    !clean_path(path).starts_with("..")
}

/// 上游 `archiveEntryPrefix`：根条目 `""`，`my-skill/SKILL.md` ⇒ `my-skill/`。
fn archive_entry_prefix(clean_name: &str) -> String {
    match clean_name.rfind('/') {
        // 无斜杠 与 斜杠在 0 位（`/SKILL.md` 一类的绝对形态）都等价于「没有包装目录」。
        None | Some(0) => String::new(),
        Some(index) => clean_name[..=index].to_string(),
    }
}

/// 上游 `skillNameFromArchive`：无 frontmatter name 时用包装目录名，其次用上传文件名。
fn skill_name_from_archive(root_prefix: &str, filename: &str) -> String {
    if !root_prefix.is_empty() {
        let base = root_prefix.trim_end_matches('/');
        let base = base.rsplit('/').next().unwrap_or("");
        if !base.is_empty() && base != "." && base != ".." {
            return base.to_string();
        }
    }
    let clean = filename.replace('\\', "/");
    let base = base_name(&clean);
    // Go `path.Ext`：从右往左找**最后一个** `.`，找到什么就返回什么 —— 包括点在下标 0 的
    // `.skill`（扩展名是 `.skill`，去掉后是**空串**）。这与 POSIX「隐藏文件没有扩展名」的
    // 直觉相反，但上游 `skillNameFromArchive` 就是这么判的 ⇒ 名字为空、走上层报错。
    match base.rfind('.') {
        None => base,
        Some(index) => &base[..index],
    }
    .trim()
    .to_string()
}

/// 上游 `isIgnoredArchiveEntry`：编辑器 / 操作系统噪声与 license 文件不进包。
fn is_ignored_archive_entry(rel: &str) -> bool {
    for segment in rel.split('/') {
        if segment.is_empty() || segment == "__MACOSX" || segment.starts_with('.') {
            return true;
        }
    }
    matches!(
        base_name(rel).to_ascii_lowercase().as_str(),
        "license" | "license.md" | "license.txt"
    )
}

/// Go `path.Base`（只按 `/` 切）。
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

/// 上游 `readZipFile`：读一条，截在 `maxSize + 1` 字节（谎报 `uncompressed_size` 的头
/// 不能撑爆内存）。超限 ⇒ `Err`（`SKILL.md` 那条是致命的，支持文件那条被跳过）。
fn read_zip_entry(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    index: usize,
) -> Result<String, String> {
    let mut file = archive.by_index(index).map_err(|error| error.to_string())?;
    let name = file.name().to_string();
    let mut buffer = Vec::new();
    let limit = MAX_IMPORT_FILE_SIZE + 1;
    file.by_ref()
        .take(limit)
        .read_to_end(&mut buffer)
        .map_err(|error| error.to_string())?;
    if buffer.len() as u64 > MAX_IMPORT_FILE_SIZE {
        return Err(format!(
            "file {name:?} exceeds {MAX_IMPORT_FILE_SIZE} bytes"
        ));
    }
    String::from_utf8(buffer).map_err(|_| format!("file {name:?} is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .expect("start_file");
            writer.write_all(content.as_bytes()).expect("write");
        }
        writer.finish().expect("finish").into_inner()
    }

    #[test]
    fn root_layout_imports_content_and_supporting_files() {
        let data = zip_of(&[
            (
                "SKILL.md",
                "---\nname: root-skill\ndescription: d\n---\nbody\n",
            ),
            ("scripts/run.sh", "echo hi\n"),
            ("references/notes.md", "notes\n"),
        ]);
        let imported = parse_skill_archive(&data, "bundle.skill").expect("parse");
        assert_eq!(imported.name, "root-skill");
        assert_eq!(imported.description, "d");
        assert_eq!(
            imported.content,
            "---\nname: root-skill\ndescription: d\n---\nbody\n"
        );
        let paths: Vec<&str> = imported.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["references/notes.md", "scripts/run.sh"]);
        assert_eq!(imported.bundle_size, "notes\n".len() + "echo hi\n".len());
    }

    #[test]
    fn wrapper_layout_roots_on_the_shallowest_skill_md() {
        // 「最浅」按**前缀字符串长度**比（上游就是 `len(prefix) <`），不是按目录深度。
        let data = zip_of(&[
            ("my-skill/SKILL.md", "---\nname: wrapped\n---\nbody\n"),
            ("my-skill/scripts/run.sh", "echo wrapped\n"),
            ("my-skill/SKILL.md.bak", "noise\n"),
            ("deep/nested/SKILL.md", "---\nname: unrelated\n---\nx\n"),
        ]);
        let imported = parse_skill_archive(&data, "my-skill.skill").expect("parse");
        assert_eq!(imported.name, "wrapped");
        // `deep/nested/**` 不在 `my-skill/` 根下，整包被丢掉。
        let paths: Vec<&str> = imported.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["SKILL.md.bak", "scripts/run.sh"]);
    }

    #[test]
    fn name_falls_back_to_wrapper_dir_then_filename() {
        let data = zip_of(&[("packaged/SKILL.md", "no frontmatter\n")]);
        let imported = parse_skill_archive(&data, "whatever.skill").expect("wrapper dir name");
        assert_eq!(imported.name, "packaged");

        let data = zip_of(&[("SKILL.md", "no frontmatter\n")]);
        let imported = parse_skill_archive(&data, "fallback.skill").expect("filename stem");
        assert_eq!(imported.name, "fallback");
    }

    #[test]
    fn noise_and_license_entries_are_dropped() {
        let data = zip_of(&[
            ("SKILL.md", "---\nname: n\n---\n"),
            ("__MACOSX/._SKILL.md", "junk"),
            (".DS_Store", "junk"),
            ("sub/.hidden", "junk"),
            ("LICENSE", "mit"),
            ("LICENSE.md", "mit"),
            ("license.txt", "mit"),
            ("keep.md", "kept"),
        ]);
        let imported = parse_skill_archive(&data, "n.skill").expect("parse");
        let paths: Vec<&str> = imported.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["keep.md"]);
    }

    #[test]
    fn nested_skill_md_is_never_a_supporting_file() {
        let data = zip_of(&[
            ("SKILL.md", "---\nname: n\n---\n"),
            ("nested/SKILL.md", "---\nname: inner\n---\n"),
        ]);
        let imported = parse_skill_archive(&data, "n.skill").expect("parse");
        assert!(imported.files.is_empty(), "nested SKILL.md must be dropped");
    }

    #[test]
    fn traversal_and_absolute_entries_are_rejected() {
        let data = zip_of(&[
            ("SKILL.md", "---\nname: n\n---\n"),
            ("../escape.md", "nope"),
            ("/abs.md", "nope"),
            ("C:/win.md", "nope"),
            ("ok.md", "yes"),
        ]);
        let imported = parse_skill_archive(&data, "n.skill").expect("parse");
        let paths: Vec<&str> = imported.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["ok.md"]);
    }

    #[test]
    fn binary_assets_are_silently_skipped() {
        let data = zip_of(&[
            ("SKILL.md", "---\nname: n\n---\n"),
            ("logo.png", "\u{0}PNG"),
            ("font.woff2", "binary"),
            ("doc.md", "text"),
        ]);
        let imported = parse_skill_archive(&data, "n.skill").expect("parse");
        let paths: Vec<&str> = imported.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["doc.md"]);
        assert_eq!(imported.bundle_size, "text".len());
    }

    #[test]
    fn not_a_zip_and_missing_skill_md_are_rejected() {
        let error = parse_skill_archive(b"not a zip at all", "x.skill").unwrap_err();
        assert_eq!(
            error.message,
            "uploaded file is not a valid .skill/.zip archive"
        );
        assert_eq!(error.failure, ImportFailure::Invalid);

        let data = zip_of(&[("README.md", "hi")]);
        let error = parse_skill_archive(&data, "x.skill").unwrap_err();
        assert_eq!(error.message, "archive does not contain a SKILL.md");
    }

    #[test]
    fn filename_fallback_strips_the_last_extension_like_go_path_ext() {
        let data = zip_of(&[("SKILL.md", "no frontmatter\n")]);
        // Go `path.Ext` 只认最后一个 `.`：`archive.tar.gz` 的扩展名是 `.gz`。
        assert_eq!(
            parse_skill_archive(&data, "archive.tar.gz").unwrap().name,
            "archive.tar"
        );
        // 没扩展名 ⇒ 原样（含前后空白要去掉）。
        assert_eq!(
            parse_skill_archive(&data, "  README  ").unwrap().name,
            "README"
        );
        // ⚠️ 反直觉但逐字对齐上游：`.skill` 本身就是扩展名 ⇒ 去掉后为空 ⇒ 整包报错
        // （而不是当成隐藏文件名 `.skill`）。
        let error = parse_skill_archive(&data, ".skill").unwrap_err();
        assert!(
            error
                .message
                .starts_with("could not determine the skill name"),
            "{}",
            error.message
        );
    }

    #[test]
    fn unnamed_archive_is_rejected() {
        let data = zip_of(&[("SKILL.md", "no name here\n")]);
        let error = parse_skill_archive(&data, "").unwrap_err();
        assert!(
            error
                .message
                .starts_with("could not determine the skill name"),
            "{}",
            error.message
        );
    }

    #[test]
    fn duplicate_paths_are_rejected_after_cleaning() {
        let data = zip_of(&[
            ("SKILL.md", "---\nname: n\n---\n"),
            ("a/./b.md", "one"),
            ("a/b.md", "two"),
        ]);
        let error = parse_skill_archive(&data, "n.skill").unwrap_err();
        assert!(
            error
                .message
                .contains("resolve to the same path \"a/b.md\""),
            "{}",
            error.message
        );
    }

    #[test]
    fn file_count_and_total_byte_caps_fail_the_whole_bundle() {
        let mut imported = ImportedSkill::new("n".into(), String::new(), "body".into());
        for index in 0..MAX_IMPORT_FILE_COUNT {
            imported
                .add_file(format!("f{index}.md"), "x")
                .expect("under the count cap");
        }
        let error = imported.add_file("over.md", "x").unwrap_err();
        assert!(error.is_cap(), "{error}");
        assert_eq!(error.message, "import bundle exceeds 256 file limit");

        let mut imported = ImportedSkill::new("n".into(), String::new(), "body".into());
        // 上游是 `> max`（不是 `>=`）：整 8 MiB 允许，8 MiB + 1 拒。
        imported
            .add_file("exact-budget.md", "y".repeat(MAX_IMPORT_TOTAL_SIZE))
            .expect("exactly at the byte budget is allowed");
        let error = imported
            .add_file("big.md", "y".repeat(MAX_IMPORT_TOTAL_SIZE + 1))
            .unwrap_err();
        assert_eq!(error.message, "import bundle exceeds 8388608 byte limit");
        assert_eq!(error.failure, ImportFailure::Cap);
    }

    #[test]
    fn oversize_individual_asset_is_skipped_not_fatal() {
        let oversize = "z".repeat(usize::try_from(MAX_IMPORT_FILE_SIZE).unwrap() + 1);
        let data = zip_of(&[
            ("SKILL.md", "---\nname: n\n---\n"),
            ("huge.md", &oversize),
            ("small.md", "ok"),
        ]);
        let imported = parse_skill_archive(&data, "n.skill").expect("parse");
        let paths: Vec<&str> = imported.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["small.md"]);
    }

    #[test]
    fn single_file_cap_does_not_leak_into_the_bundle_byte_sum() {
        // 一个「刚好 1 MiB」的文件（在被跳过的 1 MiB+1 之外）必须进包、且计入 bundle_size。
        let exact = "q".repeat(usize::try_from(MAX_IMPORT_FILE_SIZE).unwrap());
        let data = zip_of(&[("SKILL.md", "---\nname: n\n---\n"), ("exact.md", &exact)]);
        let imported = parse_skill_archive(&data, "n.skill").expect("parse");
        assert_eq!(imported.files.len(), 1);
        assert_eq!(
            imported.bundle_size,
            usize::try_from(MAX_IMPORT_FILE_SIZE).unwrap()
        );
    }
}
