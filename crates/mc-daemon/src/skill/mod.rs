//! daemon 侧 **skill 执行面**：本地发现 / 导入 / 落盘前的读取，以及 bundle 缓存校验。
//!
//! - **写者**：M6-9（`LUM-1674` / `docs/57` §4.2）。
//! - **上游**：`internal/daemon/local_skills.go`（726）+ `skill_cache.go`（192）+
//!   `slash_skill.go`（35）。
//!
//! | 本模块 | 上游 | 内容 |
//! | --- | --- | --- |
//! | [`local`] | `local_skills.go` | 每个 runtime 的 skill 发现根、`SKILL.md` 递归枚举、支持文件收集（二进制/UTF-8/NUL/体积/条数闸）、按 key 装载 bundle |
//! | [`cache`] | `skill_cache.go` | `<root>/<ws>/<source>/<id>/<hash>/bundle.json` 的读写 + 「bundle 与 ref 是否同一份」校验 |
//! | [`slash`] | `slash_skill.go` | `[/label](slash://skill/<id>)` 的提取与去重 |
//!
//! ## 为什么这些行不在 `execenv`
//!
//! 上游把 `local_skills.go` 放在 `internal/daemon/` 包根（与 `runtime_mcp.go` 同级），
//! 只在需要「落盘到 task 目录」时才调到 `execenv`。本仓照此分：**发现与读取**（纯 IO +
//! 判定）在 `skill`，**注入口**（cursor / codex / omp / claude 的 sidecar 文件）在
//! [`crate::execenv`] 的新增文件里。
//!
//! ## bundle hash 的单一实现点（DoD）
//!
//! [`cache`] 的校验**不自己算哈希**：它调 [`mc_core::skill::build_manifest`] ——
//! 与 `crates/mc-http/src/routes/daemon/skills.rs`（M3-7 移植、M6-4 补三源）**逐字同一个
//! `fn`**。`mc-daemon` 因此**不需要** `mc-skill` 边就能命中 `DoD` 的「证明是同一个函数」：
//! 两侧都指向 `mc_core::skill::…`，不是「各自算出同一个值」。
//!
//! ## 三方依赖
//!
//! 本模块**不新增**任何三方包：只用 `serde` / `serde_json` / `thiserror` / `tracing`
//! 与已有的两条 `path` 边（`mc-core`、`mc-mcp`）。home 目录、hex、随机 token 之类
//! 上游靠 `os.UserHomeDir` / `crypto/rand` 的地方，本 slice 用环境变量与两处**极窄**
//! 的本地实现替代，逐条登记在 `docs/32` §9.9。

pub mod cache;
pub mod local;
pub mod slash;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// 单个 skill 主文件（`SKILL.md`）与支持文件的体积上限（上游 `maxLocalSkillFileSize`，1 MiB）。
pub const MAX_LOCAL_SKILL_FILE_SIZE: u64 = 1 << 20;
/// 一个 skill 的支持文件**合计**上限（上游 `maxLocalSkillBundleSize`，8 MiB）。
pub const MAX_LOCAL_SKILL_BUNDLE_SIZE: u64 = 8 << 20;
/// 支持文件条数上限（上游 `maxLocalSkillFileCount`，与服务端导入器保持同一口径）。
pub const MAX_LOCAL_SKILL_FILE_COUNT: usize = 256;
/// 发现时向下递归的最大层数（上游 `maxLocalSkillDirDepth`）。
pub const MAX_LOCAL_SKILL_DIR_DEPTH: usize = 4;

/// 发现根的分类：runtime 自有的 skill 目录（`provider`）。
pub const ROOT_PROVIDER: &str = "provider";
/// 跨工具通用根 `~/.agents/skills`（`universal`）。
pub const ROOT_UNIVERSAL: &str = "universal";
/// 由**已启用插件**贡献的 skill 根（`plugin`）。
pub const ROOT_PLUGIN: &str = "plugin";

/// skill 执行面的失败。所有条目都带「在做什么」与「对哪个路径」——这一层的失败几乎都
/// 发生在「准备到一半」，路径本身就是证据（与 [`crate::execenv::ExecEnvError`] 同风格）。
#[derive(Debug, thiserror::Error)]
pub enum SkillExecError {
    #[error("skills: {op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("skills: skill key is required")]
    EmptySkillKey,
    #[error("skills: invalid skill key {0:?}")]
    InvalidSkillKey(String),
    #[error("skills: {0}")]
    Invalid(String),
    #[error("skills: local skill not found")]
    NotFound,
    #[error("skills: cannot resolve the user home directory")]
    NoHome,
    #[error("skills: skill bundle cache error: {0}")]
    Cache(String),
}

impl SkillExecError {
    /// 带路径的 IO 失败（与 `ExecEnvError::io` 同形）。
    pub(crate) fn io(op: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// 本 crate 的结果别名。
pub type Result<T, E = SkillExecError> = std::result::Result<T, E>;

/// 一个发现根（上游 `localSkillRoot`）。
///
/// `kind` 是 [`ROOT_PROVIDER`] / [`ROOT_UNIVERSAL`] / [`ROOT_PLUGIN`] 之一，会上行到
/// UI（`runtimeLocalSkillSummary.root`）；`key_prefix` 只对插件根非空，让插件的调用键
/// 与 Claude Code 的 `plugin:skill` 形态一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSkillRoot {
    /// 目录。
    pub path: PathBuf,
    /// 分类（`provider` / `universal` / `plugin`）。
    pub kind: &'static str,
    /// 键前缀（插件根用 `<plugin.name>:`）。
    pub key_prefix: String,
    /// 贡献者插件 id（非插件根为空串）。
    pub plugin: String,
}

impl LocalSkillRoot {
    /// 非插件根。
    #[must_use]
    pub fn new(path: PathBuf, kind: &'static str) -> Self {
        Self {
            path,
            kind,
            key_prefix: String::new(),
            plugin: String::new(),
        }
    }

    /// 插件根（带键前缀与插件 id）。
    #[must_use]
    pub fn plugin(path: PathBuf, key_prefix: impl Into<String>, plugin: impl Into<String>) -> Self {
        Self {
            path,
            kind: ROOT_PLUGIN,
            key_prefix: key_prefix.into(),
            plugin: plugin.into(),
        }
    }
}

/// 列表出口：一个本地 skill 的摘要（上游 `runtimeLocalSkillSummary`）。
///
/// ⚠️ 这个类型**离开用户机器**（`GET /api/daemon/local-skills` 一类的 inventory），
/// 所以里面**不得**出现命令参数、URL、请求头或环境值 —— 与
/// [`crate::mcp::runtime::RuntimeLocalMcpServerSummary`] 同一条纪律。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LocalSkillSummary {
    /// 调用键（`relative/dir`，插件根带 `<plugin>:` 前缀）。
    pub key: String,
    /// 展示名（frontmatter `name`，缺失时回落成目录名；插件根回落成 key）。
    pub name: String,
    /// frontmatter `description`（空串**不出现在 JSON 里**）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// 源路径（home 前缀被折成 `~`）。
    pub source_path: String,
    /// 该发现的 runtime/provider。
    pub provider: String,
    /// 发现根分类；旧 daemon 不送 ⇒ 空串表示「未知」。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub root: String,
    /// 贡献插件 id（非插件根省略）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plugin: String,
    /// 该 runtime 是否支持在 UI 里关掉这个 skill（上游只对 codex / claude 为真）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_disable: bool,
    /// 文件总数（**含** `SKILL.md` 本身）。
    pub file_count: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde 的 `skip_serializing_if` 只接受 `&T`
fn is_false(value: &bool) -> bool {
    !*value
}

/// 导入出口：一个本地 skill 的完整 bundle（上游 `runtimeLocalSkillBundle`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LocalSkillBundle {
    /// 展示名。
    pub name: String,
    /// frontmatter `description`。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `SKILL.md` 正文本身。
    pub content: String,
    /// 源路径（home 折 `~`）。
    pub source_path: String,
    /// runtime/provider。
    pub provider: String,
    /// 支持文件（**不含** `SKILL.md`，上游同）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SkillFileData>,
}

/// bundle 内的单个文件（上游 `SkillFileData`，本 slice 只用到 `path` / `content` 两列）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillFileData {
    /// 相对路径（`/` 分隔，已排序）。
    pub path: String,
    /// 正文；发现相（`include_content=false`）下为空串。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    /// 预留列（本 slice 不写；上游 wire 形态保留）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    /// 预留列（本 slice 不写）。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub size_bytes: i64,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // 同上
fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// claim 期从服务端拿到的 bundle 引用 + 内容（上游 `SkillRefData` / `SkillData` 的联合投影）。
///
/// 校验只读 `id` / `source` / `hash` / `size_bytes` / `file_count` / `files` / `content`
/// 这几列，其余列（`name` / `description`）保留在类型里是为了回写缓存时**逐字**保持
/// 服务端给的形状。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillRefData {
    /// `skill` 行 id（builtin 源是 `builtin:<name>`，不是 uuid）。
    pub id: String,
    /// `workspace` / `builtin` / `plugin`。
    pub source: String,
    /// 展示名。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// 描述。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `sha256:<hex>`（[`mc_core::skill::Manifest::hash`]）。
    pub hash: String,
    /// 正文 + 全部文件正文的字节数之和。
    #[serde(default)]
    pub size_bytes: i64,
    /// 支持文件条数（**不含** `SKILL.md`）。
    #[serde(default)]
    pub file_count: usize,
    /// 支持文件的摘要列（本 slice 的校验不读它，原样透传）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SkillFileRefData>,
}

/// 支持文件的摘要行（上游 `SkillFileRefData`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillFileRefData {
    /// 相对路径。
    pub path: String,
    /// `sha256:<hex>`。
    pub sha256: String,
    /// 字节数。
    pub size_bytes: i64,
}

/// bundle 内容（上游 `SkillData`）：缓存里那份 `bundle.json` 的形状。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillBundleData {
    /// skill id。
    pub id: String,
    /// 源。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// 展示名。
    pub name: String,
    /// 描述。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `sha256:<hex>`。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    /// 字节数（`0` 表示未声明 ⇒ 校验跳过这一条，与上游同）。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub size_bytes: i64,
    /// `SKILL.md` 正文。
    pub content: String,
    /// 支持文件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SkillFileData>,
}

/// 任务侧 skill 投影里**本 slice 的渲染器会读**的两列（上游 `SkillContextForEnv` 的子集）。
///
/// 上游该结构还有 id / description 等列，但 `runtime_skill_policy`、`codex_user_skills`、
/// `skill_visibility` 三个消费者只读 `Name`（做 slug 与占位判定）与 `Content`（解
/// frontmatter 的 `disable-model-invocation`）。任务上下文那半边（含其余列）随
/// task-context 切片进来后，这个类型应换成它的投影 —— 登记在 `docs/32` §9.9。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillForEnv {
    /// 展示名（**不是**盘上 slug；slug 由 [`crate::execenv::skill_visibility`] 现算）。
    pub name: String,
    /// `SKILL.md` 正文。
    pub content: String,
}

/// 当前用户 home；解析不到返回 [`SkillExecError::NoHome`]。
///
/// 上游用 `os.UserHomeDir()`（Windows 走 `%USERPROFILE%`，其余走 `$HOME`）。本 crate
/// 没有 `dirs` 边，故按同一优先级读环境变量；**不**用 `std::env::home_dir()`
/// （1.80 上是 deprecated，`clippy -D warnings` 会红）。
pub fn user_home() -> Result<PathBuf> {
    if let Some(home) = non_empty_env("HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Some(profile) = non_empty_env("USERPROFILE") {
        return Ok(PathBuf::from(profile));
    }
    Err(SkillExecError::NoHome)
}

/// 读一个环境变量，空串视作未设置（上游到处都在做 `strings.TrimSpace(os.Getenv(...))`）。
#[must_use]
pub fn non_empty_env(name: &str) -> Option<String> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 环境变量给了就用，否则回落（上游 `codexHome` / `REASONIX_HOME` / `DSH_HOME` 的同一写法）。
#[must_use]
pub fn env_or(name: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    match non_empty_env(name) {
        Some(value) => PathBuf::from(value),
        None => fallback(),
    }
}

/// 发现时必须忽略的目录/文件条目（上游 `isIgnoredLocalSkillEntry`）。
#[must_use]
pub fn is_ignored_local_skill_entry(name: &str) -> bool {
    if name.is_empty() || name.starts_with('.') {
        return true;
    }
    matches!(
        name.to_ascii_lowercase().as_str(),
        "license" | "license.md" | "license.txt"
    )
}

/// `path.Clean` 的逐行移植（Go `path` 包，**斜杠**路径；不是 `filepath.Clean`）。
///
/// 本切片有两处判据直接建立在它上面（`local` 的 key 规范化、`cache` 的
/// `safe_skill_file_path`），所以这里照 `Clean` 的实现写，而不是「等价地」用
/// `Path::components()` 拼一个 —— 后者在 `a//b`、`a/./b`、`a/../b`、空串这些边界上
/// 与 Go 的答案不同，会让「同一个 skill 在两处算出不同的 key」。
///
/// 复算举例：`"" => "."`、`"a/" => "a"`、`"a//b" => "a/b"`、`"a/../b" => "b"`、
/// `"../x" => "../x"`、`"./x" => "x"`、`"a/.." => "."`、`"../../a" => "../../a"`。
#[must_use]
pub fn clean_slash_path(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let rooted = path.starts_with('/');
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    if rooted {
        out.push('/');
    }
    let mut r = usize::from(rooted);
    // `out.len()` 的「前缀中可回退到的最深处」：rooted 时是 1（那个 `/`），否则 0。
    let mut dotdot = r;
    while r < bytes.len() {
        match bytes[r] {
            b'/' => r += 1,
            b'.' if r + 1 == bytes.len() || bytes[r + 1] == b'/' => r += 1,
            b'.' if r + 1 < bytes.len()
                && bytes[r + 1] == b'.'
                && (r + 2 == bytes.len() || bytes[r + 2] == b'/') =>
            {
                r += 2;
                if out.len() > dotdot {
                    out.pop();
                    while out.len() > dotdot {
                        if out.as_bytes()[out.len() - 1] == b'/' {
                            break;
                        }
                        out.pop();
                    }
                } else if !rooted {
                    if !out.is_empty() {
                        out.push('/');
                    }
                    out.push_str("..");
                    dotdot = out.len();
                }
            }
            _ => {
                if (rooted && out.len() != 1) || (!rooted && !out.is_empty()) {
                    out.push('/');
                }
                // 分隔符是 ASCII，故区间端点一定落在 char 边界上，切片不会 panic。
                let start = r;
                while r < bytes.len() && bytes[r] != b'/' {
                    r += 1;
                }
                out.push_str(&path[start..r]);
            }
        }
    }
    if out.is_empty() {
        out.push('.');
    }
    out
}

/// 规范化一个本地 skill 键（上游 `normalizeLocalSkillKey`）。
///
/// ⚠️ 上游最后那步是 `strings.HasPrefix(cleaned, "..")` —— 它把 `..foo` 也一并拒了。
/// 这是上游行为（不是笔误），照抄以免「同一个键在两处一个接受一个拒绝」。
pub fn normalize_local_skill_key(key: &str) -> Result<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(SkillExecError::EmptySkillKey);
    }
    let cleaned = clean_slash_path(trimmed);
    if cleaned == "." || cleaned.starts_with('/') || cleaned.starts_with("..") {
        return Err(SkillExecError::InvalidSkillKey(key.to_string()));
    }
    Ok(cleaned)
}

/// 把 home 前缀折成 `~`（上游 `relativizeHomePath`）。解析不到 home 时原样返回斜杠形态。
#[must_use]
pub fn relativize_home_path(path: &Path) -> String {
    let slash = path.to_string_lossy().replace('\\', "/");
    let Ok(home) = user_home() else {
        return slash;
    };
    let home_slash = home.to_string_lossy().replace('\\', "/");
    if slash == home_slash {
        return "~".to_string();
    }
    let prefix = format!("{home_slash}/");
    match slash.strip_prefix(&prefix) {
        Some(rest) => format!("~/{rest}"),
        None => slash,
    }
}

/// 缓存路径段的安全化（上游 `safeCacheSegment`）：只留 `[A-Za-z0-9._-]`，其余换 `_`。
///
/// 空串 → `_`；结果是 `"."` / `".."` 时前置 `_`（否则会成为目录穿越）。
#[must_use]
pub fn safe_cache_segment(segment: &str) -> String {
    if segment.is_empty() {
        return "_".to_string();
    }
    let mut out = String::with_capacity(segment.len());
    for ch in segment.chars() {
        let keep = ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.';
        out.push(if keep { ch } else { '_' });
    }
    if out == "." || out == ".." {
        out.insert(0, '_');
    }
    out
}

/// 支持文件路径白名单（上游 `safeSkillFilePath`）。
///
/// 判据全部是**拒绝**：空、含 NUL、绝对路径、含 `\`、`path.Clean` 之后与原文不同
/// （即非规范形态：`a/./b`、`a//b`、`a/`、`./a` 都要拒）、`.`、`..`、`../` 开头。
#[must_use]
pub fn safe_skill_file_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.starts_with('/') || path.contains('\\') {
        return false;
    }
    let clean = clean_slash_path(path);
    if clean == "." || clean != path || clean.starts_with("../") || clean == ".." {
        return false;
    }
    true
}

/// 小写 hex（上游各处用 `encoding/hex`；本 crate 没有 `hex` 边，这里是 6 行本地实现）。
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // `write!` 到 `String` 不会失败（`fmt::Write` 的 `String` 实现是 infallible）。
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 Go `path.Clean` 的答案逐条对照（用例取自 Go 标准库的测试表）。
    #[test]
    fn clean_slash_path_matches_go_path_clean() {
        for (input, want) in [
            ("", "."),
            ("abc", "abc"),
            ("abc/def", "abc/def"),
            ("a/b/c", "a/b/c"),
            (".", "."),
            ("..", ".."),
            ("../", ".."),
            ("../../", "../.."),
            ("/", "/"),
            ("/.", "/"),
            ("/..", "/"),
            ("/abc", "/abc"),
            ("/abc/def", "/abc/def"),
            ("a/", "a"),
            ("a//b", "a/b"),
            ("a/./b", "a/b"),
            ("./x", "x"),
            ("a/../b", "b"),
            ("a/..", "."),
            ("a/b/../..", "."),
            ("a/b/../../c", "c"),
            ("/a/../..", "/"),
            ("../../../a", "../../../a"),
            ("//", "/"),
            ("abc/def/", "abc/def"),
            ("a/../../b", "../b"),
        ] {
            assert_eq!(clean_slash_path(input), want, "input={input:?}");
        }
    }

    #[test]
    fn normalize_key_rejects_dot_absolute_and_dotdot_prefixes() {
        assert_eq!(normalize_local_skill_key(" a/b ").unwrap(), "a/b");
        assert_eq!(normalize_local_skill_key("a//b").unwrap(), "a/b");
        assert!(matches!(
            normalize_local_skill_key("   "),
            Err(SkillExecError::EmptySkillKey)
        ));
        for bad in [".", "/abs/x", "..", "../x", "..foo"] {
            assert!(
                matches!(
                    normalize_local_skill_key(bad),
                    Err(SkillExecError::InvalidSkillKey(_))
                ),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn safe_file_path_rejects_non_canonical_and_absolute() {
        for good in ["a.md", "a/b.md", "a.b-c_d/e.md"] {
            assert!(safe_skill_file_path(good), "should accept {good:?}");
        }
        for bad in [
            "",
            "/abs.md",
            "a\\b.md",
            "a//b.md",
            "a/./b.md",
            "a/",
            "./a.md",
            ".",
            "..",
            "../a.md",
            "a\u{0}.md",
        ] {
            assert!(!safe_skill_file_path(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn safe_cache_segment_keeps_only_safe_characters() {
        assert_eq!(safe_cache_segment(""), "_");
        assert_eq!(safe_cache_segment("."), "_.");
        assert_eq!(safe_cache_segment(".."), "_..");
        assert_eq!(safe_cache_segment("a/b\\c:d"), "a_b_c_d");
        assert_eq!(safe_cache_segment("Skill-1_v2.0"), "Skill-1_v2.0");
        assert_eq!(safe_cache_segment("部署"), "__");
    }

    #[test]
    fn ignored_entries_are_dotfiles_licenses_and_empty() {
        for ignored in [
            "",
            ".git",
            ".hidden",
            "LICENSE",
            "license.md",
            "License.TXT",
        ] {
            assert!(is_ignored_local_skill_entry(ignored), "{ignored:?}");
        }
        for kept in ["skills", "SKILL.md", "readme.md"] {
            assert!(!is_ignored_local_skill_entry(kept), "{kept:?}");
        }
    }

    #[test]
    fn hex_encode_is_lowercase_and_padded() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn relativize_home_path_folds_the_home_prefix() {
        let home = user_home().expect("home");
        assert_eq!(relativize_home_path(&home), "~");
        assert_eq!(
            relativize_home_path(&home.join("skills").join("a")),
            "~/skills/a"
        );
        assert_eq!(
            relativize_home_path(Path::new("/definitely/not/home")),
            "/definitely/not/home"
        );
    }

    #[test]
    fn skill_summary_json_omits_empty_optional_fields() {
        let summary = LocalSkillSummary {
            key: "a/b".into(),
            name: "Name".into(),
            description: String::new(),
            source_path: "~/skills/a/b".into(),
            provider: "claude".into(),
            root: ROOT_PROVIDER.into(),
            plugin: String::new(),
            can_disable: false,
            file_count: 3,
        };
        let raw = serde_json::to_value(&summary).expect("serialize");
        let object = raw.as_object().expect("object");
        assert!(!object.contains_key("description"));
        assert!(!object.contains_key("plugin"));
        assert!(!object.contains_key("can_disable"));
        assert_eq!(object["file_count"], 3);

        let with_flags = LocalSkillSummary {
            can_disable: true,
            description: "d".into(),
            plugin: "p".into(),
            ..summary
        };
        let raw = serde_json::to_value(&with_flags).expect("serialize");
        assert_eq!(raw["can_disable"], true);
        assert_eq!(raw["description"], "d");
        assert_eq!(raw["plugin"], "p");
    }
}
