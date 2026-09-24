//! 本地（runtime 侧）skill 的发现、枚举与读取。
//!
//! - **上游**：`internal/daemon/local_skills.go`（726 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! ## 这一层做什么
//!
//! 每个 runtime 都会把「用户级 skill」放在自己的目录里（`~/.claude/skills`、
//! `$CODEX_HOME/skills`、`~/.cursor/skills` …），另有一个跨工具的通用根
//! `~/.agents/skills`。本模块：
//!
//! 1. 按 provider 解析出**有序**的发现根（provider 根优先，通用根最后：
//!    `~/.agents/skills` 永远只是「补充」，不会改变既有 provider 根里同名 skill 的解析）；
//! 2. 递归枚举「目录里有 `SKILL.md`」的 skill（深度上限 4、符号链接不跟、忽略点开头与
//!    LICENSE），按 `key` 去重（先出现的胜出）、最后按 `key` 排序；
//! 3. 收集支持文件（排除 `SKILL.md`，二进制 / 非法 UTF-8 / 嵌 NUL 的一律跳过，
//!    单文件 1 MiB、条数 256、合计 8 MiB 三道闸）；
//! 4. 「列表」与「按 key 装载」共用同一套判据，保证**列表里看得见的一定装得到**（上游
//!    专门为这条不变式写了一段注释）。
//!
//! ## 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - **`claude` 的插件 skill 根未接**：上游要 `listEnabledClaudePlugins` /
//!   `readClaudePluginManifest` / `claudePluginComponentPaths`（`claude_plugins.go`，
//!   不在本 slice 的写集）。本 slice 只落 provider 根 + 通用根；插件根（`plugin` 分类
//!   与 `<plugin>:` 键前缀）随之缺席 ⇒ 上游那些 `root="plugin"` 的条目本 slice 不产出。
//! - **`hermes` 的根解析未接**：上游走 `execenv.ResolveHermesProfile`（Hermes 侧 home
//!   解析，随 provider 配置切片进来）；本 slice 对 `hermes` 返回「不支持」，而不是退回
//!   硬编码 `~/.hermes` —— 后者与上游注释里点名的 Windows 行为不符（GH #8310）。
//! - **内置 runtime 描述符表**：上游查 `agent.BuiltinRuntimeByID`，该表当前**只有一行**
//!   （`omp` ⇒ `.omp/agent/skills`）。本 slice 把这行落成常量；描述符注册表本身归
//!   agent 切片。
//! - `name` / `description` 与「疑似二进制」判定**不另起一套**：分别调
//!   [`mc_skill::frontmatter::parse_skill_frontmatter`] 与
//!   [`mc_skill::binary::is_likely_binary_file_path`]（上游同样调 `internal/skill` 的
//!   同一对函数）。

use std::fs;
use std::path::{Path, PathBuf};

use mc_skill::binary::is_likely_binary_file_path;
use mc_skill::frontmatter::parse_skill_frontmatter;

use super::{
    clean_slash_path, env_or, is_ignored_local_skill_entry, normalize_local_skill_key,
    relativize_home_path, user_home, LocalSkillBundle, LocalSkillRoot, LocalSkillSummary, Result,
    SkillExecError, SkillFileData, MAX_LOCAL_SKILL_BUNDLE_SIZE, MAX_LOCAL_SKILL_DIR_DEPTH,
    MAX_LOCAL_SKILL_FILE_COUNT, MAX_LOCAL_SKILL_FILE_SIZE, ROOT_PROVIDER, ROOT_UNIVERSAL,
};

/// `SKILL.md`（skill 主文件名，上游到处按这个字面量判）。
pub const SKILL_MAIN_FILE: &str = "SKILL.md";

/// `provider` 的参数（`loadRuntimeLocalSkillBundle` / `listRuntimeLocalSkills` 的入参）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider<'a> {
    /// runtime/provider 标识（`claude` / `codex` / …）。
    pub id: &'a str,
}

impl<'a> Provider<'a> {
    /// 构造。
    #[must_use]
    pub const fn new(id: &'a str) -> Self {
        Self { id }
    }
}

/// 一个 provider 的发现结果：不支持与「支持但一个都没有」必须分得开。
///
/// 上游把这件事压在 `(value, supported, error)` 三元组里；本仓用枚举表达，调用方
/// 不可能把「不支持」当成「空列表」而给上层一个假的「这个 runtime 没有 skill」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderSkills {
    /// 该 provider 没有用户级 skill 面（上游 `return nil, false, nil`）。
    Unsupported,
    /// 支持；值可能为空列表。
    Supported(Vec<LocalSkillSummary>),
}

/// 内置 runtime 描述符里本 slice 需要的那两列（`BuiltinRuntimes` 当前只有 `omp` 一行）。
const BUILTIN_USER_SKILLS_DIRS: &[(&str, &str)] = &[("omp", ".omp/agent/skills")];

/// 有序发现根：provider 根（可选）+ 通用根 `~/.agents/skills`。
///
/// 返回 `Ok(None)` 表示该 provider **没有**用户级 skill 面（上游 `supported=false`）。
pub fn local_skill_roots_for_provider(provider: &str) -> Result<Option<Vec<LocalSkillRoot>>> {
    let home = user_home()?;
    let Some(provider_root) = provider_root(provider, &home) else {
        return Ok(None);
    };

    let mut roots = Vec::with_capacity(2);
    // providerRoot 为空串表示「这个 runtime 没有自己的 home」⇒ 不要拿 daemon 的 cwd 去解析键。
    if !provider_root.as_os_str().is_empty() {
        roots.push(LocalSkillRoot::new(provider_root, ROOT_PROVIDER));
    }
    roots.push(LocalSkillRoot::new(
        home.join(".agents").join("skills"),
        ROOT_UNIVERSAL,
    ));
    // ⚠️ 上游这里还有一段 `claude` 的插件根（`listEnabledClaudePlugins`）；本 slice 不接，
    // 理由见模块文档（`claude_plugins.go` 不在写集）。登记在 `docs/32` §9.9。
    Ok(Some(roots))
}

/// provider → 用户级 skill 根（上游 `localSkillRootsForProvider` 的 `switch`）。
///
/// `None` = 不支持（上游 `default: return nil, false, nil`）。
fn provider_root(provider: &str, home: &Path) -> Option<PathBuf> {
    if let Some((_, relative)) = BUILTIN_USER_SKILLS_DIRS
        .iter()
        .find(|(id, _)| *id == provider)
    {
        return Some(home.join(relative));
    }
    let path = match provider {
        "claude" => home.join(".claude").join("skills"),
        // CodeBuddy Code 是 Claude Code 的 fork，但有自己的配置目录，**不**读 ~/.claude/skills。
        "codebuddy" => home.join(".codebuddy").join("skills"),
        "codex" => env_or("CODEX_HOME", || home.join(".codex")).join("skills"),
        "copilot" => home.join(".copilot").join("skills"),
        "opencode" => home.join(".config").join("opencode").join("skills"),
        "codearts" => home.join(".codeartsdoer").join("skills"),
        "deveco" => home.join(".config").join("deveco").join("skills"),
        "openclaw" => home.join(".openclaw").join("skills"),
        "pi" => home.join(".pi").join("agent").join("skills"),
        "cursor" => home.join(".cursor").join("skills"),
        // ⚠️ 上游走 `execenv.ResolveHermesProfile`（不在本 slice）；见模块文档。
        "hermes" => return None,
        "kimi" => home.join(".kimi").join("skills"),
        "reasonix" => env_or("REASONIX_HOME", || home.join(".reasonix")).join("skills"),
        "dsh" => env_or("DSH_HOME", || home.join(".dsh")).join("skills"),
        "kiro" => home.join(".kiro").join("skills"),
        "qoder" => home.join(".qoder").join("skills"),
        "qoderclicn" => home.join(".qoder-cn").join("skills"),
        "traecli" => home.join(".traecli").join("skills"),
        "antigravity" => home.join(".gemini").join("antigravity-cli").join("skills"),
        "grok" => env_or("GROK_HOME", || home.join(".grok")).join("skills"),
        "qwen" => env_or("QWEN_HOME", || home.join(".qwen")).join("skills"),
        "qwenpaw" => qwenpaw_skill_pool(home),
        "mcode" => home.join(".minimax").join("skills"),
        _ => return None,
    };
    Some(path)
}

/// `QwenPaw` 的 skill 池（上游 `QWENPAW_WORKING_DIR` → `COPAW_WORKING_DIR` → 存在的
/// `~/.copaw` → `~/.qwenpaw`，然后接 `skill_pool`）。
fn qwenpaw_skill_pool(home: &Path) -> PathBuf {
    let mut root = super::non_empty_env("QWENPAW_WORKING_DIR")
        .or_else(|| super::non_empty_env("COPAW_WORKING_DIR"));
    if root.is_none() {
        let legacy = home.join(".copaw");
        root = Some(if legacy.is_dir() {
            legacy.to_string_lossy().to_string()
        } else {
            home.join(".qwenpaw").to_string_lossy().to_string()
        });
    }
    PathBuf::from(root.unwrap_or_default()).join("skill_pool")
}

/// 列出该 runtime 的用户级 skill（上游 `listRuntimeLocalSkills`）。
pub fn list_runtime_local_skills(provider: &str) -> Result<ProviderSkills> {
    let Some(roots) = local_skill_roots_for_provider(provider)? else {
        return Ok(ProviderSkills::Unsupported);
    };

    let mut skills: Vec<LocalSkillSummary> = Vec::new();
    // 严格按 key 去重：根按优先级访问，先出现者胜 —— 于是「加通用根」只**增加**
    // 不冲突的 key，既有 provider 根的可见项一个不少（上游把这条写成可证的不变式）。
    let mut seen_keys: Vec<String> = Vec::new();
    for root in &roots {
        if !root.path.exists() {
            continue;
        }
        // 每个根**各自**一个 visited 集：用户可以故意把同一份盘上 skill 用两个名字暴露
        // （`~/.claude/skills/bar -> ~/.agents/skills/foo`），共享 visited 会把合法的第二项
        // 静默吞掉。
        let mut root_skills: Vec<LocalSkillSummary> = Vec::new();
        let mut visited: Vec<PathBuf> = Vec::new();
        enumerate_local_skills(
            provider,
            root,
            &root.path,
            &root.path,
            0,
            &mut visited,
            &mut root_skills,
        );
        for skill in root_skills {
            if seen_keys.contains(&skill.key) {
                continue;
            }
            seen_keys.push(skill.key.clone());
            skills.push(skill);
        }
    }

    skills.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(ProviderSkills::Supported(skills))
}

/// 递归枚举一个根下的 skill（上游 `enumerateLocalSkills`）。
///
/// 一旦某个目录**自己**带 `SKILL.md` 就登记它，并**不再**往下走（哪怕它内部还有嵌套
/// 的 `SKILL.md`）；否则继续下沉。`visited` 记的是解析后的真实路径，于是环状符号链接
/// 不会把递归绕死 —— 这也是这里唯一一处「先解析」的原因。
#[allow(clippy::too_many_arguments)] // 上游单函数顺序照搬：递归参数就是它的调用面
fn enumerate_local_skills(
    provider: &str,
    root: &LocalSkillRoot,
    walk_root: &Path,
    current_dir: &Path,
    depth: usize,
    visited: &mut Vec<PathBuf>,
    skills: &mut Vec<LocalSkillSummary>,
) {
    if depth > MAX_LOCAL_SKILL_DIR_DEPTH {
        return;
    }
    // 解析失败（最多见的是悬空链接）就放弃这一支，与上游同。
    let Ok(resolved) = fs::canonicalize(current_dir) else {
        return;
    };
    if visited.contains(&resolved) {
        return;
    }
    visited.push(resolved);

    let Ok(entries) = fs::read_dir(current_dir) else {
        return;
    };
    let mut entries: Vec<fs::DirEntry> = entries.filter_map(std::result::Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if is_ignored_local_skill_entry(&name) {
            continue;
        }
        let path = entry.path();
        // 上游这里 `os.Stat`（跟符号链接）判「是不是目录」。
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }

        if path.join(SKILL_MAIN_FILE).is_file() {
            let Some(skill) = summarise_local_skill(provider, root, walk_root, &path) else {
                continue;
            };
            skills.push(skill);
            continue;
        }

        enumerate_local_skills(provider, root, walk_root, &path, depth + 1, visited, skills);
    }
}

/// 把一个「带 `SKILL.md` 的目录」转成摘要（`key` / `name` / `file_count` …）。
fn summarise_local_skill(
    provider: &str,
    root: &LocalSkillRoot,
    walk_root: &Path,
    skill_dir: &Path,
) -> Option<LocalSkillSummary> {
    let relative = skill_dir.strip_prefix(walk_root).ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    let key = normalize_local_skill_key(&relative).ok()?;
    let key = format!("{}{}", root.key_prefix, key);

    let content = read_local_skill_main_file(skill_dir).ok()?;
    let frontmatter = parse_skill_frontmatter(&content);
    let name = if root.plugin.is_empty() {
        if frontmatter.name.is_empty() {
            skill_dir
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            frontmatter.name.clone()
        }
    } else {
        key.clone()
    };

    let files = collect_local_skill_files(skill_dir, false).ok()?;

    Some(LocalSkillSummary {
        key,
        name,
        description: frontmatter.description,
        source_path: relativize_home_path(skill_dir),
        provider: provider.to_string(),
        root: root.kind.to_string(),
        plugin: root.plugin.clone(),
        can_disable: provider == "codex" || provider == "claude",
        // `files` 是**支持**文件（不含 `SKILL.md`）；列表上用户要看的是总数 ⇒ 加回 1。
        file_count: files.len() + 1,
    })
}

/// 读 `SKILL.md`（上游 `readLocalSkillMainFile`）：超过 1 MiB 直接拒。
pub fn read_local_skill_main_file(skill_dir: &Path) -> Result<String> {
    let main_path = skill_dir.join(SKILL_MAIN_FILE);
    let metadata = fs::metadata(&main_path)
        .map_err(|err| SkillExecError::io("stat SKILL.md", &main_path, err))?;
    if metadata.len() > MAX_LOCAL_SKILL_FILE_SIZE {
        return Err(SkillExecError::Invalid(format!(
            "SKILL.md exceeds {MAX_LOCAL_SKILL_FILE_SIZE} bytes"
        )));
    }
    let content =
        fs::read(&main_path).map_err(|err| SkillExecError::io("read SKILL.md", &main_path, err))?;
    String::from_utf8(content)
        .map_err(|err| SkillExecError::Invalid(format!("SKILL.md is not valid UTF-8: {err}")))
}

/// 收集支持文件（上游 `collectLocalSkillFiles`），按 `path` 升序返回。
///
/// `include_content=false` 是**发现相**（只要路径），`true` 是**同步相**（带正文）。
/// 两相走**同一套**跳过判据 —— 上游专门说明过：发现相不算正文、同步相算，会让「列表
/// 承诺的文件」在导入时被静默丢掉（于是列表与落盘不一致）。
pub fn collect_local_skill_files(
    skill_dir: &Path,
    include_content: bool,
) -> Result<Vec<SkillFileData>> {
    // `filepath.WalkDir` 不跟符号链接根，而 lark-cli 之类的安装器把每个 skill 做成指向
    // 共享目录的链接 ⇒ 从链接路径走会枚举出 0 个子项。先解析真实路径再走。
    let walk_root = fs::canonicalize(skill_dir).unwrap_or_else(|_| skill_dir.to_path_buf());

    let mut files: Vec<SkillFileData> = Vec::new();
    let mut total_size: u64 = 0;
    walk_local_skill_files(
        &walk_root,
        &walk_root,
        include_content,
        &mut files,
        &mut total_size,
    )?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// 递归收集本体（对应 Go 的 `filepath.WalkDir` 回调）。
fn walk_local_skill_files(
    walk_root: &Path,
    current_dir: &Path,
    include_content: bool,
    files: &mut Vec<SkillFileData>,
    total_size: &mut u64,
) -> Result<()> {
    let Ok(entries) = fs::read_dir(current_dir) else {
        // 上游对 walkErr 一律 `return nil`（跳过这一支，不中断整棵树）。
        return Ok(());
    };
    let mut entries: Vec<fs::DirEntry> = entries.filter_map(std::result::Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // 符号链接一律跳过（含指向目录的）：上游 `entry.Type()&ModeSymlink != 0` 后
        // `IsDir()` 恒为 false ⇒ 既不下降也不收作文件。
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if is_ignored_local_skill_entry(&name) {
                continue; // SkipDir
            }
            walk_local_skill_files(walk_root, &path, include_content, files, total_size)?;
            continue;
        }
        if is_ignored_local_skill_entry(&name) || name.eq_ignore_ascii_case(SKILL_MAIN_FILE) {
            continue;
        }

        let Ok(relative) = path.strip_prefix(walk_root) else {
            continue;
        };
        let relative = clean_slash_path(&relative.to_string_lossy().replace('\\', "/"));
        if relative == "." || relative.starts_with('/') || relative.starts_with("..") {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_LOCAL_SKILL_FILE_SIZE {
            continue;
        }
        // 扩展名的廉价初判（与归档/URL 导入器同一个函数）；真正的保证是下面那次读。
        if is_likely_binary_file_path(&relative) {
            tracing::info!(
                skill_dir = %walk_root.display(),
                path = %relative,
                size = metadata.len(),
                reason = "binary_extension",
                "local skill: skipping binary file"
            );
            continue;
        }
        // 读一次是**真判据**：合法 UTF-8 **且**不含 NUL。前者不够 —— 服务端导入路径会
        // 把正文里的 0x00 剥掉（`sanitizeNullBytes`），于是「UTF-8 合法但含 NUL」的文件
        // 回来时与写下去的字节不同（UTF-16LE 的 ASCII 文本就是现实例子）。
        let Ok(content) = fs::read(&path) else {
            continue;
        };
        if content.contains(&0) || std::str::from_utf8(&content).is_err() {
            tracing::info!(
                skill_dir = %walk_root.display(),
                path = %relative,
                size = metadata.len(),
                reason = "invalid_utf8_or_nul",
                "local skill: skipping binary file"
            );
            continue;
        }
        if files.len() >= MAX_LOCAL_SKILL_FILE_COUNT {
            return Err(SkillExecError::Invalid(format!(
                "local skill exceeds {MAX_LOCAL_SKILL_FILE_COUNT} files"
            )));
        }
        *total_size += metadata.len();
        if *total_size > MAX_LOCAL_SKILL_BUNDLE_SIZE {
            return Err(SkillExecError::Invalid(format!(
                "local skill exceeds {MAX_LOCAL_SKILL_BUNDLE_SIZE} bytes in total"
            )));
        }

        files.push(SkillFileData {
            path: relative,
            content: if include_content {
                String::from_utf8(content).unwrap_or_default()
            } else {
                String::new()
            },
            sha256: String::new(),
            size_bytes: 0,
        });
    }
    Ok(())
}

/// 按 key 装载一个本地 skill 的完整 bundle（上游 `loadRuntimeLocalSkillBundle`）。
///
/// 返回 `Ok(None)` = 该 provider 不支持；`Err(NotFound)` = 支持但这个 key 一个根都没有。
/// 这条区分与上游的三元组同形，**不要**把两者合并成 `None`。
///
/// 装载与列表**共用**「根里有这个 skill」的判据（目录 + `SKILL.md`）：否则用户按列表点了
/// 一个 skill，导入的却可能是低优先级根里同名的另一份内容。
pub fn load_runtime_local_skill_bundle(
    provider: &str,
    skill_key: &str,
) -> Result<Option<LocalSkillBundle>> {
    let Some(roots) = local_skill_roots_for_provider(provider)? else {
        return Ok(None);
    };
    let key = normalize_local_skill_key(skill_key)?;

    for root in &roots {
        let root_key = if root.key_prefix.is_empty() {
            key.clone()
        } else {
            let Some(stripped) = key.strip_prefix(&root.key_prefix) else {
                continue;
            };
            stripped.to_string()
        };
        let skill_dir = root.path.join(&root_key);
        let metadata = match fs::metadata(&skill_dir) {
            Ok(metadata) => metadata,
            Err(err) => {
                // 不存在 ⇒ 这个根没有它，试下一个；其余（权限 / IO）原样上报 —— 静默跳过
                // 可能装到低优先级根里**另一个**同 key 的 skill。
                if err.kind() == std::io::ErrorKind::NotFound {
                    continue;
                }
                return Err(SkillExecError::io("stat local skill dir", &skill_dir, err));
            }
        };
        if !metadata.is_dir() {
            continue;
        }
        // 目录里**必须**有 `SKILL.md` 才算这个 skill：同名但没主文件的目录不遮蔽低优先级根。
        let main_path = skill_dir.join(SKILL_MAIN_FILE);
        match fs::metadata(&main_path) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(SkillExecError::io("stat SKILL.md", &main_path, err)),
        }

        let content = read_local_skill_main_file(&skill_dir)?;
        let frontmatter = parse_skill_frontmatter(&content);
        let name = if root.plugin.is_empty() {
            if frontmatter.name.is_empty() {
                skill_dir
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default()
            } else {
                frontmatter.name.clone()
            }
        } else {
            key.clone()
        };
        let files = collect_local_skill_files(&skill_dir, true)?;

        return Ok(Some(LocalSkillBundle {
            name,
            description: frontmatter.description,
            content,
            source_path: relativize_home_path(&skill_dir),
            provider: provider.to_string(),
            files,
        }));
    }

    Err(SkillExecError::NotFound)
}

#[cfg(test)]
mod tests;
