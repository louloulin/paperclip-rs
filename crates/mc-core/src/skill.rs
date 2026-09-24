//! Skill 领域类型（M6-0 anchor **重写**，不是扩充）。
//!
//! 类型来源 = `migrations/upstream/**` 的**真实列**（`contracts/upstream-schema.json`
//! 是同一组列的 `pg_get_*` 快照）。本文件覆盖 W6 的 skill 面实体：
//!
//! | 实体 | 表 | 建表迁移 | 追加迁移 | 列数 |
//! | --- | --- | --- | --- | ---: |
//! | [`Skill`] | `skill` | `008` | `368` | 10 |
//! | [`SkillFile`] | `skill_file` | `008` | — | 6 |
//! | [`AgentSkill`] | `agent_skill` | `008` | `161` | 4 |
//! | [`SkillLabel`] | `skill_to_label` | `162` | — | 3 |
//!
//! **本波 0 新迁移**：四张表全部早在上游（`008` / `161` / `162` / `368`）。
//!
//! 标签**目录**不在本文件：`162` 把 `issue_label` 泛化成一张带
//! `resource_type IN ('issue','agent','skill')` 的目录表，skill 标签只是
//! `skill_to_label` 的关联行（`label_id` 指向那张目录表）⇒ 目录的类型面在 M2，
//! 本文件只给关联行 [`SkillLabel`]。
//!
//! # 旧 stub 错在哪（重写前的实测，**不要再参考**）
//!
//! 重写前的 `skill.rs`（43 行）几乎每个字段都不对：
//!
//! | 旧 stub | 真值 |
//! | --- | --- |
//! | `slug: String` | 表**没有** `slug`；身份键是 `UNIQUE(workspace_id, name)` |
//! | `SkillVisibility{Workspace, Private}` | 表**没有** `visibility` 列（`handler/skill.go` 全文也没有这个词汇） |
//! | `body: String` | 列名是 **`content`** |
//! | `description: Option<String>` | `NOT NULL DEFAULT ''`（不是可空） |
//! | `enabled: bool` | `skill` 表没有 enabled；「启用」是 [`AgentSkill::enabled`]（`161` 加在关联表上） |
//! | `owner_id: Option<Id>` | 列名是 **`created_by`**（可空，`REFERENCES "user"(id)`，无 `ON DELETE`） |
//! | `plugin_key: Option<String>` | 表里没有；与插件的关系只有 `plugin_installation_id`（`368`，**无外键**，按仓库策略由应用层维护） |
//! | `config` 缺失 | `config JSONB NOT NULL DEFAULT '{}'` 是真实列 |
//!
//! # 「源」是三种（[`SkillSource`]）
//!
//! 上游 `pkg/skillbundle/hash.go` 定 `SourceWorkspace = "workspace"` /
//! `SourceBuiltin = "builtin"` / `SourcePlugin = "plugin"`，`service/task.go` 的
//! `AgentSkillBundleRef{ID, Source}` 用它决定从哪儿解析 bundle
//! （`LoadRequestedAgentSkillBundles` 的 `switch` 只认 builtin / workspace，其余落
//! not-found）。本类型只承载**词汇表**；解析实现分属 M6-4（builtin / plugin 物化）
//! 与 M3-7 的 `routes/daemon/skills.rs`（workspace）。
//!
//! # bundle hash 的口径（不可漂移）
//!
//! `skillbundle.BuildManifest` = 分节 sha256：先 `v1, Source, ID, Name, Description,
//! Content`，再按 `Path` 排序的每个文件 `(Path, "sha256:"+hex, Content)`；产出
//! `Manifest{Hash: "sha256:"+hex, SizeBytes, FileCount, Files}`。
//!
//! ⚠️ 两个落库的 digest 列是**纯 hex**（`plugin_package_version.digest` /
//! `plugin_package_file.sha256`，`CHECK char_length = 64`）；只有 bundle manifest 的
//! `hash` 与 `FileRef.sha256` 带 `sha256:` 前缀 ⇒ 前缀只在出口加，**不要**写进
//! 本文件的类型（[`SkillRef::hash`] 是上游 wire 形态，故带前缀，见其文档）。

use serde::{Deserialize, Serialize};
// M6-4 追加：bundle manifest 的唯一实现点（见文件尾「bundle manifest」段）。
use sha2::{Digest, Sha256};

use super::id::Id;
use super::timestamp::Timestamp;

/// Skill 主体（表 `skill`，10 列）。
///
/// `content` 是 `SKILL.md` 正文本身（markdown），支持文件在 [`SkillFile`]。
/// 列表接口历史上会省略 `content`（正文 50–200KB，见 multica-ai/multica#2174）；
/// 那是**响应 DTO** 的事（M6-2 的 `SkillSummary`），不是本类型的事。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    pub description: String,
    /// `SKILL.md` 正文（列 `content`）。
    pub content: String,
    /// 列 `config JSONB NOT NULL DEFAULT '{}'`。
    pub config: serde_json::Value,
    /// 人写的技能为 `None`；插件贡献的由 [`Skill::plugin_installation_id`] 追溯。
    pub created_by: Option<Id>,
    /// `368` 加：非 `NULL` ⇒ 这一行是某次安装贡献的，卸载时要精确删掉。
    /// **无外键**：卸载在同一事务里显式删除。
    pub plugin_installation_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Skill 支持文件（表 `skill_file`，6 列）。
///
/// `UNIQUE(skill_id, path)`：`path` 是技能根内的相对路径，`SKILL.md` 本身**不是**
/// 支持文件（上游 `internal/skill/reserved.go` 的 `IsReservedContentPath` 判它保留）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillFile {
    pub id: Id,
    pub skill_id: Id,
    pub path: String,
    pub content: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Agent ↔ skill 关联（表 `agent_skill`，4 列，`PK(agent_id, skill_id)`）。
///
/// 这张表就是**授权**：bundle 解析只认 `ListAgentSkillsByIDs` 的结果，所以
/// 「源」再合法，没这一行也解析不到。`enabled`（`161`）是每 agent 的开关。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSkill {
    pub agent_id: Id,
    pub skill_id: Id,
    pub enabled: bool,
    pub created_at: Timestamp,
}

/// skill ↔ 标签关联（表 `skill_to_label`，3 列，`PK(skill_id, label_id)`）。
///
/// `label_id` 指向被 `162` 泛化的 `issue_label` 目录行（`resource_type = 'skill'`）；
/// 目录实体本身不在本 crate 的 skill 面。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillLabel {
    pub skill_id: Id,
    pub label_id: Id,
    pub created_at: Timestamp,
}

/// bundle 的来源（上游 `skillbundle.Source*` 三个常量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    /// 工作区里人写的技能行（默认形态）。
    #[default]
    Workspace,
    /// 平台内置资产（M6-4 物化）。
    Builtin,
    /// 插件贡献的技能（`skill.plugin_installation_id` 非空）。
    Plugin,
}

impl SkillSource {
    /// 上游常量值（wire / bundle manifest / 比较都用这一个）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Builtin => "builtin",
            Self::Plugin => "plugin",
        }
    }

    /// 三种源（顺序 = 上游常量声明顺序）。
    pub const ALL: [Self; 3] = [Self::Workspace, Self::Builtin, Self::Plugin];
}

/// bundle 引用（上游 `service.AgentSkillRefData` + `skillbundle.Manifest` 的联合投影）。
///
/// 这是**出口形态**：`hash` / [`SkillFileRef::sha256`] 都带 `sha256:` 前缀，
/// 与 `skillbundle.BuildManifest` 逐字一致（落库的 digest 列是纯 hex，见模块文档）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillRef {
    pub id: Id,
    pub source: SkillSource,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `"sha256:" + hex`。
    pub hash: String,
    pub size_bytes: i64,
    pub file_count: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SkillFileRef>,
}

/// bundle 内单个文件在 manifest 里的摘要（上游 `skillbundle.FileRef`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillFileRef {
    pub path: String,
    /// `"sha256:" + hex`。
    pub sha256: String,
    pub size_bytes: i64,
}

// ---------------------------------------------------------------------------
// bundle manifest：**唯一实现点**（`skillbundle.BuildManifest` 的逐字移植）
//
// 为什么在这里（`mc-core`）而不是 `mc-http`：`service.AgentSkillRefData` 与
// `mc-daemon` 的缓存校验都要用**同一个** `fn`（不是「同结果的新实现」），而
// `mc-http` 的 `build_bundle` 是 `pub(crate)`、`mc-daemon` 又**没有** `mc-skill` 依赖边。
// `mc-core` 同时被两者依赖 ⇒ 落在这里不需要新增任何依赖边、不改任何 `Cargo.toml`。
//
// ⚠️ 本段是 `docs/57` §9.1 冻结面上的一次性豁免（`LUM-1669`，M6-4）：**只追加**，
// 不改上面的类型定义、既有断言与模块文档段。
// ---------------------------------------------------------------------------

/// `skillbundle.BuildManifest` 的一个输入文件（`skillbundle.File`）。
///
/// 借用而不是 `String`：调用方手里的行结构（`mc-repos` 的 `(path, content)` 对）
/// 不需要为此再克隆一遍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestFile<'a> {
    /// 技能根内的相对路径。
    pub path: &'a str,
    /// 文件正文。
    pub content: &'a str,
}

/// `skillbundle.Skill`（`BuildManifest` 的入参）。
///
/// `id` 在 builtin 源上是 `"builtin:<name>"`（**不是** uuid），所以这里是 `&str`
/// 而不是 [`Id`]：manifest 是 hash 口径，不是 wire 形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestInput<'a> {
    /// `ID`（workspace 源是 uuid 文本；builtin 源是 `builtin:<name>`）。
    pub id: &'a str,
    /// `Source`（[`SkillSource::as_str`] 的三个常量之一）。
    pub source: SkillSource,
    /// `Name`。
    pub name: &'a str,
    /// `Description`。
    pub description: &'a str,
    /// `Content`（`SKILL.md` 正文）。
    pub content: &'a str,
    /// 支持文件（**顺序无关**：函数自己按 `path` 升序排）。
    pub files: &'a [ManifestFile<'a>],
}

/// `skillbundle.Manifest`：digest 投影（**不是** wire 类型，wire 出口是 [`SkillRef`]）。
///
/// `hash` / [`SkillFileRef::sha256`] 带 `sha256:` 前缀；`size_bytes` = 正文 + 全部
/// 文件正文的**字节**数之和。
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// `"sha256:" + hex`。
    pub hash: String,
    /// `SizeBytes`。
    pub size_bytes: i64,
    /// `FileCount`。
    pub file_count: i32,
    /// `Files`（按 `path` 升序）。
    pub files: Vec<SkillFileRef>,
}

/// `skillbundle.writeHashPart`：`fmt.Fprintf(h, "%d:%s\n", len(value), value)`。
///
/// `len` 是 **UTF-8 字节数**（Go 的 `len(string)`），不是字符数 —— 中文路径/正文
/// 写错这一条就会让 daemon 侧的缓存校验全部失配。
///
/// `hasher` 收 `&mut Sha256`（而不是 Go 那样的 `interface{ Write([]byte) }` 泛型）：
/// 上游只喂 `sha256.New()`，多态在这里没有第二个实现者。
pub fn write_hash_part(hasher: &mut Sha256, value: &str) {
    hasher.update(format!("{}:{}\n", value.len(), value).as_bytes());
}

/// `skillbundle.BuildManifest`：分节 sha256 的 bundle digest。
///
/// 分节顺序**逐字**：`"v1"` → `source` → `id` → `name` → `description` → `content`
/// →（按 `path` 升序的每个文件）`path` → `"sha256:"+hex(content)` → `content`。
/// 任何一处少写/多写一节都会产出**仍然自洽**、但与上游不同的 hash。
#[must_use]
pub fn build_manifest(input: &ManifestInput<'_>) -> Manifest {
    let mut files: Vec<ManifestFile<'_>> = input.files.to_vec();
    files.sort_by(|a, b| a.path.cmp(b.path));

    let mut hasher = Sha256::new();
    write_hash_part(&mut hasher, "v1");
    write_hash_part(&mut hasher, input.source.as_str());
    write_hash_part(&mut hasher, input.id);
    write_hash_part(&mut hasher, input.name);
    write_hash_part(&mut hasher, input.description);
    write_hash_part(&mut hasher, input.content);

    let mut size = as_i64(input.content.len());
    let mut refs = Vec::with_capacity(files.len());
    for file in files {
        let digest = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(file.content.as_bytes()))
        );
        write_hash_part(&mut hasher, file.path);
        write_hash_part(&mut hasher, &digest);
        write_hash_part(&mut hasher, file.content);
        let file_size = as_i64(file.content.len());
        size += file_size;
        refs.push(SkillFileRef {
            path: file.path.to_string(),
            sha256: digest,
            size_bytes: file_size,
        });
    }

    Manifest {
        hash: format!("sha256:{}", hex::encode(hasher.finalize())),
        size_bytes: size,
        file_count: i32::try_from(refs.len()).unwrap_or(i32::MAX),
        files: refs,
    }
}

/// `usize` 字节数 → `i64`（上游 `SizeBytes` 是 int64）。
///
/// 只有 PB 级内容才会溢出；饱和比回绕成负数安全。
fn as_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn skill_source_strings_match_upstream_bundle_constants() {
        // 上游 pkg/skillbundle/hash.go 的三个常量是**闭集**：
        // workspace / builtin / plugin。改这里必须同步改上游语义。
        assert_eq!(SkillSource::Workspace.as_str(), "workspace");
        assert_eq!(SkillSource::Builtin.as_str(), "builtin");
        assert_eq!(SkillSource::Plugin.as_str(), "plugin");
        assert_eq!(SkillSource::default(), SkillSource::Workspace);
        assert_eq!(SkillSource::ALL.len(), 3);
    }

    #[test]
    fn skill_source_serde_is_lowercase_and_round_trips() {
        for source in SkillSource::ALL {
            let json = serde_json::to_string(&source).unwrap();
            assert_eq!(json, format!("\"{}\"", source.as_str()));
            assert_eq!(serde_json::from_str::<SkillSource>(&json).unwrap(), source);
        }
    }

    #[test]
    fn skill_ref_hash_keeps_the_wire_prefix() {
        // 出口形态带前缀；纯 hex 只出现在 `ContentHash` 与落库 digest 列。
        let reference = SkillRef {
            id: Id::nil(),
            source: SkillSource::Workspace,
            name: "demo".into(),
            description: String::new(),
            hash: "sha256:00".into(),
            size_bytes: 0,
            file_count: 0,
            files: vec![],
        };
        assert!(reference.hash.starts_with("sha256:"));
        let json = serde_json::to_value(&reference).unwrap();
        assert!(
            json.get("description").is_none(),
            "空 description / files 按上游 omitempty 不出现在 JSON 里"
        );
    }

    // -----------------------------------------------------------------------
    // bundle manifest（M6-4 追加；上游 `pkg/skillbundle/hash_test.go` 的等价断言）
    // -----------------------------------------------------------------------

    fn input<'a>(files: &'a [ManifestFile<'a>]) -> ManifestInput<'a> {
        ManifestInput {
            id: "1c331d0b-94fd-412a-a7cc-6a209add00a1",
            source: SkillSource::Workspace,
            name: "deploy",
            description: "deploy helper",
            content: "main",
            files,
        }
    }

    /// 口径逐字断言：分节顺序 + 每节 `len:value\n` + `sha256:` 前缀形态。
    ///
    /// 手工按上游口径重算一遍（**不复用** [`build_manifest`]），否则这条用例
    /// 只能证明函数与它自己一致。
    #[test]
    fn manifest_follows_upstream_section_order_byte_for_byte() {
        let files = [
            ManifestFile {
                path: "b.md",
                content: "bb",
            },
            ManifestFile {
                path: "a.md",
                content: "a",
            },
        ];
        let manifest = build_manifest(&input(&files));

        // 手工重算：a.md 先（path 升序），每节 `len:value\n`，len 按字节。
        let digest_a = format!("sha256:{}", hex::encode(Sha256::digest(b"a")));
        let digest_b = format!("sha256:{}", hex::encode(Sha256::digest(b"bb")));
        let mut expected = String::new();
        for part in [
            "v1",
            "workspace",
            "1c331d0b-94fd-412a-a7cc-6a209add00a1",
            "deploy",
            "deploy helper",
            "main",
            "a.md",
        ] {
            writeln!(expected, "{}:{}", part.len(), part).unwrap();
        }
        for part in [digest_a.as_str(), "a", "b.md", digest_b.as_str(), "bb"] {
            writeln!(expected, "{}:{}", part.len(), part).unwrap();
        }

        let want_hash = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(expected.as_bytes()))
        );
        assert_eq!(manifest.hash, want_hash);
        assert!(manifest.hash.starts_with("sha256:"));
        assert_eq!(manifest.hash.len(), "sha256:".len() + 64);
        assert_eq!(manifest.file_count, 2);
        assert_eq!(manifest.size_bytes, 7);
        assert_eq!(manifest.files[0].path, "a.md");
        assert_eq!(manifest.files[1].path, "b.md");
        for file in &manifest.files {
            assert!(file.sha256.starts_with("sha256:"));
            assert_eq!(file.sha256.len(), "sha256:".len() + 64);
        }
        assert_eq!(manifest.files[1].sha256, digest_b);
        assert_eq!(manifest.files[0].size_bytes, 1);
    }

    /// 上游 `TestBuildManifestStableAcrossFileOrder`：文件顺序不影响 hash，输出按 path 排。
    #[test]
    fn manifest_is_stable_across_file_order() {
        let forward = [
            ManifestFile {
                path: "a.md",
                content: "a",
            },
            ManifestFile {
                path: "b.md",
                content: "b",
            },
        ];
        let reversed = [forward[1], forward[0]];
        let a = build_manifest(&input(&forward));
        let b = build_manifest(&input(&reversed));
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.files[0].path, "a.md");
        assert_eq!(b.files[0].path, "a.md");
    }

    /// 上游 `TestBuildManifestChangesWhenContentChanges`：正文进 hash。
    #[test]
    fn manifest_changes_with_content() {
        let one = build_manifest(&input(&[]));
        let other = build_manifest(&ManifestInput {
            content: "changed",
            ..input(&[])
        });
        assert_ne!(one.hash, other.hash);
    }

    /// `writeHashPart` 的长度是**字节**数（多字节 UTF-8 也能逐字节复算）。
    #[test]
    fn hash_part_uses_byte_length() {
        let mut hasher = Sha256::new();
        write_hash_part(&mut hasher, "部署");
        // "部署" = 6 字节 ⇒ 头部必须是 "6:" 而不是 "2:"。
        let expected = Sha256::digest("6:部署\n".as_bytes());
        assert_eq!(hasher.finalize().to_vec(), expected.to_vec());
    }

    /// 空 description / 空 files 的形态：仍然有那两节（`0:\n`）。
    #[test]
    fn manifest_writes_empty_parts_for_empty_description_and_files() {
        let manifest = build_manifest(&ManifestInput {
            id: "builtin:multica-platform",
            source: SkillSource::Builtin,
            name: "multica-platform",
            description: "",
            content: "",
            files: &[],
        });
        assert_eq!(manifest.file_count, 0);
        assert_eq!(manifest.size_bytes, 0);
        assert!(manifest.files.is_empty());

        let mut expected = String::new();
        for part in [
            "v1",
            "builtin",
            "builtin:multica-platform",
            "multica-platform",
            "",
            "",
        ] {
            writeln!(expected, "{}:{}", part.len(), part).unwrap();
        }
        let want = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(expected.as_bytes()))
        );
        assert_eq!(manifest.hash, want);
    }

    /// 三种源各产出不同 hash（源进分节 ⇒ 同名同名一个 workspace 与 builtin 不共用 digest）。
    #[test]
    fn manifest_separates_the_three_sources() {
        let base = ManifestInput {
            id: "demo",
            source: SkillSource::Workspace,
            name: "demo",
            description: "d",
            content: "c",
            files: &[],
        };
        let hashes: Vec<String> = SkillSource::ALL
            .iter()
            .map(|source| {
                build_manifest(&ManifestInput {
                    source: *source,
                    ..base
                })
                .hash
            })
            .collect();
        assert_eq!(hashes.len(), 3);
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
        assert_ne!(hashes[0], hashes[2]);
    }
}
