//! 内置 skill 的**物化**（列表 / 取件 / 与工作区行的对齐）。
//!
//! - **写者**：M6-4（`docs/57` §3.2：`mc-skill/src/builtin.rs + assets/**` | M6-4 写）。
//! - **上游**：`internal/service/builtin_skills.go`（147 行）+ 两棵 `go:embed` 根树的资产。
//!   本仓的解析/供给路由是同切片（M6-4）的 `routes/agents/skills.rs` 与
//!   `routes/daemon/skills.rs`（M6-4 是后者唯一写者）。
//! - **语义**：内置 skill 是**进程内资源**（`assets/**`，编译期随 crate 进二进制），
//!   不是数据库行；解析时以 `SkillRef{source: Builtin}` 出现在 bundle 里，`id` 是
//!   `"builtin:<name>"`（[`builtin_skill_id`]，**不是 uuid**）。
//! - **bundle 哈希**：唯一实现点是 [`mc_core::skill::build_manifest`]（M6-4 落的
//!   §9.1 一次性豁免）⇒ **本文件不再写一份哈希**，只把资产投影成它的入参。
//! - **本仓约定**：`assets/**` 用 `include_str!` 引入（不用 `include_dir!`，也不用运行时
//!   读盘 —— 部署环境没有源树）。
//! - **不做什么**：不实现「内置 skill 的自动升级 / 版本回填」。
//!
//! # 上游的两棵 `go:embed` 根树（**别只落第一棵**）
//!
//! `builtin_skills.go:10` 与 `:16` 声明了两个 `embed.FS`：默认发货的 `builtin_skills/`
//! 与只在「老 daemon 的 runtime brief 还指着旧名字」时才追加的 `builtin_skills_legacy/`。
//! 两棵树的差别是**语义**而不是目录：legacy 里的是**重定向桩**（32 行，只写新位置），
//! 所以 [`BUILTIN_SKILLS`] 与 [`LEGACY_REDIRECT_SKILLS`] 分开，由调用方按能力决定发不发
//! （上游 `BuiltinSkills(agentSystemKey, legacyRedirects)` 的第 2 参）。
//! 解析路径用 [`all_builtin_skills`]（含桩）—— 上游 `AllBuiltinSkills` 的理由是
//! 「claim 已经决定了告诉过 agent 哪些内置，daemon 只能请求它拿到过的 ref」。
//!
//! # 内置 skill 的作用域（`builtinSkillSystemKey`）
//!
//! `multica-onboarding` 只发 [`MIKA_SYSTEM_KEY`]（`"mika"`）那个内置 agent；
//! 不在作用域表里的内置是**通用**的。见 [`builtin_skills`]。
//!
//! # 资产清单（R-M6-7：清单 + 逐文件 sha，不靠人工目测）
//!
//! 每个资产的 sha256 都钉在 [`BuiltinFile::sha256`] / [`BuiltinSkill::sha256`] 上，
//! [`tests::asset_sha256_matches_the_pinned_manifest`] 每次跑测试都会重算一遍 ⇒
//! 「资产被改动」与「清单漂移」都会立刻变红。清单口径 = 上游 `90e0bdf` 的
//! `internal/service/builtin_skills{,_legacy}/`：**11 文件 / 2,327 行**（10 + 1 文件，
//! 2,295 + 32 行）。

use mc_core::skill::{Manifest, ManifestFile, ManifestInput, SkillFileRef, SkillSource};
pub static MULTICA_ONBOARDING_FILES: &[BuiltinFile] = &[];

/// `SKILL.md` 正文（内置 skill 的 `Content`）。
const MULTICA_ONBOARDING_CONTENT: &str =
    include_str!("../assets/builtin_skills/multica-onboarding/SKILL.md");

const MULTICA_ONBOARDING: BuiltinSkill = BuiltinSkill {
    name: "multica-onboarding",
    content: MULTICA_ONBOARDING_CONTENT,
    sha256: "218fbcac5a115621ee56ee3d9f02c46e82dc4a1a76a5598055db869ab1570c36",
    files: MULTICA_ONBOARDING_FILES,
};

pub static MULTICA_PLATFORM_FILES: &[BuiltinFile] = &[
    BuiltinFile {
        path: "references/agents.md",
        sha256: "2c63899b71d302196af0e1ca448eb41e87072a4c06c30e076c9333effa876e98",
        content: include_str!("../assets/builtin_skills/multica-platform/references/agents.md"),
    },
    BuiltinFile {
        path: "references/autopilots.md",
        sha256: "75bfb9d20e9af556d01290247df4f482be0bb0566e3fdb7f7fafe7f8e18c29b7",
        content: include_str!("../assets/builtin_skills/multica-platform/references/autopilots.md"),
    },
    BuiltinFile {
        path: "references/issues.md",
        sha256: "a6d28d5471967c2c3f89c39684660626d3130e814166967fbcffd93603198d3f",
        content: include_str!("../assets/builtin_skills/multica-platform/references/issues.md"),
    },
    BuiltinFile {
        path: "references/mentions.md",
        sha256: "c7f96872a1e547e4f8714ec400bb33cc651d64d5ee3c74c971ada99ce99cb625",
        content: include_str!("../assets/builtin_skills/multica-platform/references/mentions.md"),
    },
    BuiltinFile {
        path: "references/projects.md",
        sha256: "cc05408ae333b166778ccc2b5e559bdf312daf32aaab61923778c924896df37e",
        content: include_str!("../assets/builtin_skills/multica-platform/references/projects.md"),
    },
    BuiltinFile {
        path: "references/runtimes.md",
        sha256: "1601d99753404494d1f246d69d70af0048d2b32cc9dc4da6f10fc314c5d4b71a",
        content: include_str!("../assets/builtin_skills/multica-platform/references/runtimes.md"),
    },
    BuiltinFile {
        path: "references/skill-import.md",
        sha256: "f88046e97980a42b632cbf5434c05a1c5518b05aaf4616815d481de6166f12f7",
        content: include_str!(
            "../assets/builtin_skills/multica-platform/references/skill-import.md"
        ),
    },
    BuiltinFile {
        path: "references/squads.md",
        sha256: "a53c9d12d7c5a1dbd56a06f1abf7dec24479181bc63d80c725ec7b6d6f57ed37",
        content: include_str!("../assets/builtin_skills/multica-platform/references/squads.md"),
    },
];

/// `SKILL.md` 正文（内置 skill 的 `Content`）。
const MULTICA_PLATFORM_CONTENT: &str =
    include_str!("../assets/builtin_skills/multica-platform/SKILL.md");

const MULTICA_PLATFORM: BuiltinSkill = BuiltinSkill {
    name: "multica-platform",
    content: MULTICA_PLATFORM_CONTENT,
    sha256: "8bfcca9c43e8eacc79275de0ae6ded526f076624b6cf7a248615b92ccc8cae04",
    files: MULTICA_PLATFORM_FILES,
};

pub static MULTICA_WORKING_ON_ISSUES_FILES: &[BuiltinFile] = &[];

/// `SKILL.md` 正文（内置 skill 的 `Content`）。
const MULTICA_WORKING_ON_ISSUES_CONTENT: &str =
    include_str!("../assets/builtin_skills_legacy/multica-working-on-issues/SKILL.md");

const MULTICA_WORKING_ON_ISSUES: BuiltinSkill = BuiltinSkill {
    name: "multica-working-on-issues",
    content: MULTICA_WORKING_ON_ISSUES_CONTENT,
    sha256: "26fe14d88e0562bd789d1e417c34ad6067d0b8d2fc1b9f1a62fddfe2c2f0789a",
    files: MULTICA_WORKING_ON_ISSUES_FILES,
};

/// 通用内置 skill 常量：平台契约（`PlatformSkillName`）。
///
/// `multica-platform` 承载 issues / mentions / agents / squads / autopilots / projects /
/// runtimes / skill import 的契约，**每个** agent 都收到它（上游注释逐字）。
pub const PLATFORM_SKILL_NAME: &str = "multica-platform";

/// `multica-onboarding` 的作用域键：只有这个 `system_key` 的 agent 收到它。
///
/// 上游 `service.MikaSystemKey = "mika"`（`builtin_agents.go:16`）。
pub const MIKA_SYSTEM_KEY: &str = "mika";

/// 只在「老 daemon 的重定向」场景发货的那个 skill 名（上游 `builtin_skills_legacy/`）。
pub const LEGACY_REDIRECT_SKILL_NAME: &str = "multica-working-on-issues";

/// 默认发货的内置 skill（上游 `loadBuiltinSkills` 的候选集：`builtin_skills/` 一棵树）。
///
/// 顺序 = 上游 `fs.ReadDir` 的字典序（`multica-onboarding` < `multica-platform`）。
pub const BUILTIN_SKILLS: &[BuiltinSkill] = &[MULTICA_ONBOARDING, MULTICA_PLATFORM];

/// 重定向桩（上游 `legacyRedirectSkills`：`builtin_skills_legacy/` 一棵树）。
///
/// **默认不发货**：只对 runtime brief 还指着旧名字的 daemon 追加。
pub const LEGACY_REDIRECT_SKILLS: &[BuiltinSkill] = &[MULTICA_WORKING_ON_ISSUES];

/// 全部内置（上游 `AllBuiltinSkills` = 两棵树相加）—— **解析路径用这个**。
pub const ALL_BUILTIN_SKILLS: &[BuiltinSkill] = &[
    MULTICA_ONBOARDING,
    MULTICA_PLATFORM,
    MULTICA_WORKING_ON_ISSUES,
];

/// 内置资产文件的**清单形**：路径 + 引脚 sha + 编译期内联的正文。
///
/// `content` 用 `include_str!` ⇒ 缺文件是**编译错误**（不是运行时空 bundle），
/// 与上游 `go:embed` 的失败语义一致。
pub struct BuiltinFile {
    /// 技能根内的相对路径（`SKILL.md` 本身**不在**这里，它是 [`BuiltinSkill::content`]）。
    pub path: &'static str,
    /// 上游 `90e0bdf` 的 sha256（纯 hex，与落库 digest 列同形）。
    pub sha256: &'static str,
    /// 文件正文。
    pub content: &'static str,
}

/// 一个内置 skill（上游 `service.AgentSkillData` 的进程内形态）。
pub struct BuiltinSkill {
    /// 目录名 = skill 名（上游 `loadBuiltinSkill` 用目录名，不解析 frontmatter）。
    pub name: &'static str,
    /// `SKILL.md` 正文（上游 `AgentSkillData.Content`）。
    pub content: &'static str,
    /// `SKILL.md` 的 sha256（纯 hex）。
    pub sha256: &'static str,
    /// 支持文件（不含 `SKILL.md`）。
    pub files: &'static [BuiltinFile],
}

impl BuiltinSkill {
    /// 上游 `BuiltinSkillID`：`"builtin:" + name`。
    ///
    /// 这个 id **不是 uuid**：`agent_skill` 关联表存的是 uuid，所以内置 skill 不进关联表，
    /// 它的可见性来自「平台内置」这一事实（与上游 `ListAgentSkillSummaries` 的 union 侧同理）。
    #[must_use]
    pub fn id(&self) -> String {
        builtin_skill_id(self.name)
    }

    /// 该内置 skill 是否只发给某个 `system_key` 的 agent（上游 `builtinSkillSystemKey`）。
    #[must_use]
    pub const fn system_key(&self) -> Option<&'static str> {
        if str_eq(self.name, "multica-onboarding") {
            Some(MIKA_SYSTEM_KEY)
        } else {
            None
        }
    }

    /// 支持文件的借用视图（[`mc_core::skill::build_manifest`] 需要 `&[ManifestFile]`）。
    ///
    /// 返回 `Vec`（而不是静态切片）的原因：`ManifestFile` 是从 `BuiltinFile` 的字段
    /// 投影出来的，`static` 常量上下文里无法对任意 `&'static [BuiltinFile]` 做逐元素投影。
    #[must_use]
    pub fn manifest_files(&self) -> Vec<ManifestFile<'_>> {
        self.files
            .iter()
            .map(|file| ManifestFile {
                path: file.path,
                content: file.content,
            })
            .collect()
    }

    /// bundle manifest（`hash` / `size_bytes` / `file_count` / 每文件 `sha256:`）。
    ///
    /// 内部直接调 [`mc_core::skill::build_manifest`] ⇒ 与 workspace / plugin 源的
    /// digest 是**同一个函数**（不是「同结果的新实现」）。
    /// `Description` 恒为空 —— 上游 `AgentSkillData`（内置物）没有 description 字段。
    #[must_use]
    pub fn manifest(&self) -> Manifest {
        let files = self.manifest_files();
        mc_core::skill::build_manifest(&ManifestInput {
            id: &self.id(),
            source: SkillSource::Builtin,
            name: self.name,
            description: "",
            content: self.content,
            files: &files,
        })
    }

    /// 支持文件的 ref 列表（与 manifest 的 `files` 逐字一致）。
    #[must_use]
    pub fn file_refs(&self) -> Vec<SkillFileRef> {
        self.manifest().files
    }
}

/// 上游 `service.BuiltinSkillID`：`"builtin:" + name`。
#[must_use]
pub fn builtin_skill_id(name: &str) -> String {
    format!("builtin:{name}")
}

/// 上游 `service.AgentSkillBundleKey`：`source + "\x00" + id`。
///
/// 源**是键的一部分**：同名的 builtin 与 workspace skill 是两个不同的 bundle。
#[must_use]
pub fn agent_skill_bundle_key(source: SkillSource, id: &str) -> String {
    format!("{}\u{0}{id}", source.as_str())
}

/// 上游 `TaskService.BuiltinSkills(agentSystemKey, legacyRedirects)`：
/// 该 agent 收到哪些内置 skill。
///
/// `legacy_redirects` = 老 daemon 的 runtime brief 还指着旧名字（上游按**能力**判断，
/// 从不按版本号字符串）。
#[must_use]
pub fn builtin_skills(system_key: &str, legacy_redirects: bool) -> Vec<&'static BuiltinSkill> {
    let mut out: Vec<&'static BuiltinSkill> = BUILTIN_SKILLS
        .iter()
        .filter(|skill| skill.system_key().is_none_or(|want| want == system_key))
        .collect();
    if legacy_redirects {
        out.extend(LEGACY_REDIRECT_SKILLS.iter());
    }
    out
}

/// 上游 `TaskService.AllBuiltinSkills`：全部内置（含重定向桩）—— 解析路径用。
#[must_use]
pub fn all_builtin_skills() -> &'static [BuiltinSkill] {
    ALL_BUILTIN_SKILLS
}

/// 按 `"builtin:<name>"` 取一个内置 skill。
///
/// ⚠️ 与上游同为**全量**查找（不是 agent 作用域内的查找）：daemon 只能请求它拿到过的
/// ref，作用域已在 claim 侧决定过（上游 `AllBuiltinSkills` 的注释）。
#[must_use]
pub fn builtin_skill_by_id(id: &str) -> Option<&'static BuiltinSkill> {
    let name = id.strip_prefix("builtin:")?;
    ALL_BUILTIN_SKILLS.iter().find(|skill| skill.name == name)
}

/// `const fn` 里的字符串比较（`str::eq` 在 const 上下文不可用）。
const fn str_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut i = 0;
    while i < left.len() {
        if left[i] != right[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_hex(content: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(content.as_bytes()))
    }

    fn line_count(content: &str) -> usize {
        content.lines().count()
    }

    fn file_content(skill: &BuiltinSkill, path: &str) -> String {
        skill
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{} 缺少 {}", skill.name, path))
            .content
            .to_string()
    }

    /// R-M6-7：资产生成清单 + **逐文件 sha 重算**（引脚值 = 上游 `90e0bdf`）。
    #[test]
    fn asset_sha256_matches_the_pinned_manifest() {
        for skill in ALL_BUILTIN_SKILLS {
            assert_eq!(
                sha256_hex(skill.content),
                skill.sha256,
                "{} 的 SKILL.md 与引脚 sha 不符",
                skill.name
            );
            for file in skill.files {
                assert_eq!(
                    sha256_hex(file.content),
                    file.sha256,
                    "{} 的 {} 与引脚 sha 不符",
                    skill.name,
                    file.path
                );
            }
        }
    }

    /// 清单口径：两棵根树相加 = **11 文件 / 2,327 行**（`docs/57` §4.2 / issue 第 3 条）。
    #[test]
    fn asset_manifest_totals_match_the_upstream_tree() {
        assert_eq!(BUILTIN_SKILLS.len(), 2);
        assert_eq!(LEGACY_REDIRECT_SKILLS.len(), 1);
        assert_eq!(ALL_BUILTIN_SKILLS.len(), 3);

        let files: usize = ALL_BUILTIN_SKILLS.iter().map(|s| 1 + s.files.len()).sum();
        assert_eq!(files, 11, "两棵 go:embed 根树相加 = 11 个文件");

        let lines: usize = ALL_BUILTIN_SKILLS
            .iter()
            .map(|s| {
                line_count(s.content) + s.files.iter().map(|f| line_count(f.content)).sum::<usize>()
            })
            .sum();
        assert_eq!(lines, 2327, "11 个文件的物理行数之和");

        // 只落第一棵树 = 少 1 文件 / 32 行 —— 这条断言就是防这个。
        let shipped: usize = BUILTIN_SKILLS.iter().map(|s| 1 + s.files.len()).sum();
        assert_eq!(shipped, 10);
        assert_eq!(
            line_count(LEGACY_REDIRECT_SKILLS[0].content),
            32,
            "legacy 桩是 32 行"
        );
    }

    /// 上游 `builtinSkillSystemKey`：只有 `mika` 收到 onboarding。
    #[test]
    fn builtin_scoping_matches_the_system_key_table() {
        let names = |system_key: &str, legacy: bool| -> Vec<&'static str> {
            builtin_skills(system_key, legacy)
                .into_iter()
                .map(|skill| skill.name)
                .collect()
        };

        // 普通工作区 agent（system_key 为空）：只有 platform。
        assert_eq!(names("", false), vec![PLATFORM_SKILL_NAME]);
        // mika：onboarding + platform（字典序）。
        assert_eq!(
            names(MIKA_SYSTEM_KEY, false),
            vec!["multica-onboarding", PLATFORM_SKILL_NAME]
        );
        // legacy redirects 只在被要求时追加，且**追加在末尾**（上游 `append` 语义）。
        assert_eq!(
            names("", true),
            vec![PLATFORM_SKILL_NAME, LEGACY_REDIRECT_SKILL_NAME]
        );
        assert_eq!(names(MIKA_SYSTEM_KEY, true).len(), 3);
        // 别的 system_key 拿不到 onboarding。
        assert_eq!(names("other", false), vec![PLATFORM_SKILL_NAME]);
    }

    /// 解析路径用**全量**（含 legacy 桩），与供给路径不是同一个集合。
    #[test]
    fn resolve_path_sees_every_builtin_including_legacy_stubs() {
        assert_eq!(all_builtin_skills().len(), 3);
        for skill in all_builtin_skills() {
            assert_eq!(
                builtin_skill_by_id(&skill.id()).map(|s| s.name),
                Some(skill.name)
            );
        }
        assert_eq!(builtin_skill_by_id("builtin:nope").map(|s| s.name), None);
        assert_eq!(builtin_skill_by_id("not-a-uuid").map(|s| s.name), None);
        assert!(
            builtin_skill_by_id("builtin:multica-working-on-issues").is_some(),
            "重定向桩在解析面可见（否则老 daemon 的 ref 解析不到）"
        );
    }

    /// id / bundle key 的形态逐字（`BuiltinSkillID` + `AgentSkillBundleKey`）。
    #[test]
    fn ids_and_bundle_keys_match_upstream() {
        assert_eq!(
            builtin_skill_id("multica-platform"),
            "builtin:multica-platform"
        );
        assert_eq!(
            agent_skill_bundle_key(SkillSource::Builtin, "builtin:multica-platform"),
            "builtin\u{0}builtin:multica-platform"
        );
        assert_eq!(
            agent_skill_bundle_key(
                SkillSource::Workspace,
                "1c331d0b-94fd-412a-a7cc-6a209add00a1"
            ),
            "workspace\u{0}1c331d0b-94fd-412a-a7cc-6a209add00a1"
        );
        assert_eq!(
            agent_skill_bundle_key(SkillSource::Plugin, "p1"),
            "plugin\u{0}p1"
        );
        let platform = builtin_skill_by_id("builtin:multica-platform").unwrap();
        assert_eq!(platform.id(), "builtin:multica-platform");
    }

    /// manifest 走的是 `mc-core` 的**同一个函数**：手工按口径复算一遍 platform。
    #[test]
    fn manifest_hash_is_the_mc_core_implementation() {
        use sha2::{Digest, Sha256};

        let platform = builtin_skill_by_id("builtin:multica-platform").unwrap();
        let manifest = platform.manifest();

        let mut expected = String::new();
        for part in [
            "v1",
            "builtin",
            "builtin:multica-platform",
            "multica-platform",
            "",
            platform.content,
        ] {
            expected.push_str(&format!("{}:{}\n", part.len(), part));
        }
        // 支持文件按 path 升序，每节 path / sha256:<hex> / content。
        for file in &manifest.files {
            let content = file_content(platform, &file.path);
            let digest = format!("sha256:{}", hex::encode(Sha256::digest(content.as_bytes())));
            for part in [file.path.as_str(), digest.as_str(), content.as_str()] {
                expected.push_str(&format!("{}:{}\n", part.len(), part));
            }
        }
        let want = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(expected.as_bytes()))
        );
        assert_eq!(manifest.hash, want);

        // 「同一个 fn」的证据：类型署名指向 `mc_core::skill::build_manifest` 本身，
        // 用同一份入参再算一次必须逐字相等。
        let same: fn(&ManifestInput<'_>) -> Manifest = mc_core::skill::build_manifest;
        let files = platform.manifest_files();
        let id = platform.id();
        assert_eq!(
            same(&ManifestInput {
                id: &id,
                source: SkillSource::Builtin,
                name: platform.name,
                description: "",
                content: platform.content,
                files: &files,
            })
            .hash,
            manifest.hash
        );

        assert_eq!(manifest.file_count, 8, "platform 有 8 个 references/*.md");
        assert_eq!(manifest.files.len(), 8);
        assert!(manifest.size_bytes > 0);
        assert!(manifest
            .files
            .iter()
            .all(|f| f.path.starts_with("references/")));
        assert_eq!(platform.file_refs(), manifest.files);
    }

    /// 平台契约里被引用的路径真的存在（`references/*.md` 一个都不少）。
    #[test]
    fn platform_references_are_complete() {
        let platform = builtin_skill_by_id("builtin:multica-platform").unwrap();
        let mut paths: Vec<&str> = platform.files.iter().map(|f| f.path).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                "references/agents.md",
                "references/autopilots.md",
                "references/issues.md",
                "references/mentions.md",
                "references/projects.md",
                "references/runtimes.md",
                "references/skill-import.md",
                "references/squads.md",
            ]
        );
    }

    /// 不同内置 skill 的 digest 互不相同（不是常量）。
    #[test]
    fn different_builtins_have_different_hashes() {
        let hashes: Vec<String> = ALL_BUILTIN_SKILLS
            .iter()
            .map(|s| s.manifest().hash)
            .collect();
        assert_eq!(hashes.len(), 3);
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
        assert_ne!(hashes[0], hashes[2]);
    }

    /// 作用域表引用的名字必须真的存在（改名时这条先红）。
    #[test]
    fn scoped_names_exist_in_the_shipped_tree() {
        assert!(BUILTIN_SKILLS.iter().any(|s| s.name == PLATFORM_SKILL_NAME));
        assert!(BUILTIN_SKILLS
            .iter()
            .any(|s| s.name == "multica-onboarding" && s.system_key() == Some(MIKA_SYSTEM_KEY)));
        assert!(LEGACY_REDIRECT_SKILLS
            .iter()
            .any(|s| s.name == LEGACY_REDIRECT_SKILL_NAME));
    }
}
