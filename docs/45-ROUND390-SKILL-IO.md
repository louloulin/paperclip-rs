# R390 — Skill I/O Helpers (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapter-utils/src/server-utils.ts`
中 7 个 skills I/O 函数 + 2 个常量移植到新模块
`crates/pc-acpx/src/skill_io.rs`,作为 R388 (`skill_snapshot` builders)
+R389 (`skill_materialize` materialize/hash/lock) 的**第三块拼图** ——
配合后,concrete adapters (`pc-adapter-claude-local` /
`pc-adapter-codex-local` / `pc-adapter-gemini-local` /
`pc-adapter-grok-local` / `pc-adapter-opencode-local` /
`pc-adapter-pi-local`) 可直接拼出 `listXxxSkills` /
`syncXxxSkills`。

## 范围

- 新增 `crates/pc-acpx/src/skill_io.rs`(930 行):7 个公开函数 +
  2 个常量 + 1 个公开枚举 + 5 个内部 helper + 22 个单元测试
- 新增 `crates/pc-acpx/tests/round390_skill_io.rs`(440 行):18 个集成测试
- 更新 `crates/pc-acpx/src/lib.rs`:新增 `pub mod skill_io;` +
  `pub use skill_io::{...};`(按字母序在 `skill_materialize` 之前)
- 跟 Node `paperclip/packages/adapter-utils/src/server-utils.ts`
  L125-128 / L290-292 / L2440-3160 精确对齐

## Node 函数 / 常量映射

### 常量(完全镜像 L125-128)
```rust
pub const PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES: &[&str] =
    &["../../skills", "../../../../../skills"];
pub const PAPERCLIP_SKILL_KEY_PREFIX: &str = "paperclipai/paperclip";
```

### 7 个公开函数

| Node function | Rust function | 行号 |
|---|---|---|
| `isMaintainerOnlySkillTarget` (L290-292) | `is_maintainer_only_skill_target` | 纯函数 |
| `resolvePaperclipSkillsDir` (L2440-2457) | `resolve_paperclip_skills_dir` | async I/O |
| `listPaperclipSkillEntries` (L2467-2477) | `list_paperclip_skill_entries` | async I/O |
| `readInstalledSkillTargets` (L2481-2490) | `read_installed_skill_targets` | async I/O |
| `normalizeConfiguredPaperclipRuntimeSkills` (L2740-2767) | `normalize_configured_paperclip_runtime_skills` | 纯函数 |
| `readPaperclipRuntimeSkillEntries` (L2769-2773) | `read_paperclip_runtime_skill_entries` | async I/O |
| `readPaperclipSkillMarkdown` (L2775-2787) | `read_paperclip_skill_markdown` | async I/O |
| `ensurePaperclipSkillSymlink` (L2891-2920) | `ensure_paperclip_skill_symlink` | async I/O |
| `ensurePaperclipSkillSymlink` (variant) | `ensure_paperclip_skill_symlink_with_linker` | 测试变体 |
| `removeMaintainerOnlySkillSymlinks` (L3121-3160) | `remove_maintainer_only_skill_symlinks` | async I/O |

### `SkillSymlinkOutcome` 枚举(镜像 Node `"created" | "repaired" | "skipped"`)
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSymlinkOutcome {
    Created,
    Repaired,
    Skipped,
}
```

## 关键设计决策

### 1. Lex-normalize 替代 `PathBuf::join`(root cause 修复)
Node `path.resolve(moduleDir, "../../skills")` 自动 lex-normalize `..`。
Rust `PathBuf::join` 不规范化路径,`temp_dir/.../../../skills` 保留字面 `..`,
而 `tokio::fs::metadata` 在 macOS sandbox 上对未规范化的 `..` 路径会返回
`Ok((true, 192))`(即使真实路径不存在),导致 `resolvePaperclipSkillsDir`
错误地"发现"一个不存在的目录。

**解决方案**:实现 `lex_normalize(path)` 纯函数,collapse `.` / `..`
components 不访问文件系统,镜像 Node `path.resolve` 语义。所有
candidate path 在 join 后立即 lex-normalize,稳定可预测。

### 2. `ensure_paperclip_skill_symlink_with_linker` 测试变体
默认 `link_skill` 闭包用 `tokio::fs::symlink`(unix only,windows
unsupported)。测试可通过注入 `link_skill` 闭包验证 create/repair 调用,
不必每次都碰真实文件系统。

### 3. windows backslash 归一化
`is_maintainer_only_skill_target` 在 contains 检查前先
`value.replace('\\', "/")`,镜像 Node L286-288。

### 4. `BTreeMap` 而非 `HashMap`
稳定迭代顺序,与 Node `Map` 的插入顺序语义匹配。

### 5. `PAPERCLIP_SKILL_KEY_PREFIX` 拼接到 namespace
`paperclipai/paperclip/${entry.name}` 镜像 Node L2475。

### 6. `Option<PathBuf>` 表示 nullable resolve result
与 Node `Promise<string | null>` 对齐。

### 7. `tokio::fs::*` 统一 async I/O
符合 workspace `unsafe_code = "forbid"`。

## 单元测试(22 个,模块内)

### 常量
- `constants_match_node_literals` (1)

### isMaintainerOnlySkillTarget
- `recognises_absolute_dot_agents_segment` (1)
- `rejects_paths_without_dot_agents` (1)

### resolvePaperclipSkillsDir
- `resolve_returns_none_when_no_candidate_exists` (1)
- `resolve_picks_first_existing_relative_candidate` (1)

### listPaperclipSkillEntries
- `list_returns_empty_when_root_missing` (1)
- `list_emits_namespaced_keys` (1)

### readInstalledSkillTargets
- `read_installed_returns_empty_when_missing` (1)
- `read_installed_classifies_entries` (1)

### normalizeConfiguredPaperclipRuntimeSkills
- `normalize_drops_entries_missing_required_fields` (1)
- `normalize_trims_whitespace_and_returns_empty_for_non_array` (1)
- `normalize_handles_none_input` (1)

### readPaperclipRuntimeSkillEntries
- `read_prefers_configured_when_present` (1)

### readPaperclipSkillMarkdown
- `read_markdown_returns_body_for_matching_key` (1)
- `read_markdown_returns_none_for_unknown_key` (1)

### ensurePaperclipSkillSymlink
- `ensure_creates_when_target_missing` (1)
- `ensure_skips_when_target_is_correct_link` (1)
- `ensure_skips_when_target_is_regular_file` (1)
- `ensure_skips_when_target_resolves_to_existing_path` (1)
- `ensure_repairs_when_target_link_is_broken` (1)
- `ensure_with_injected_linker_records_calls` (1)

### removeMaintainerOnlySkillSymlinks
- `remove_maintainer_only_drops_only_under_dot_agents` (1, unix only)
- `remove_maintainer_only_returns_empty_when_missing` (1)

## 集成测试(18 个,`tests/round390_skill_io.rs`)

### Constants
- `constants_match_node_literals` (1)

### isMaintainerOnlySkillTarget
- `maintainer_target_recognises_dot_agents_segment` (1)
- `maintainer_target_rejects_other_paths` (1)
- `maintainer_target_handles_windows_backslashes` (1)

### resolvePaperclipSkillsDir
- `resolve_uses_additional_candidate_when_relative_missing` (1)
- `resolve_first_existing_wins_over_additional` (1)

### listPaperclipSkillEntries
- `list_then_markdown_round_trip` (1, 与 read_paperclip_skill_markdown)
- `list_emits_well_formed_entries` (1)

### readPaperclipSkillMarkdown
- `markdown_returns_none_for_unknown_key` (1)

### readInstalledSkillTargets
- `read_installed_classifies_dir_file_symlink` (1, unix only)

### normalizeConfiguredPaperclipRuntimeSkills
- `normalize_drops_invalid_shapes` (1)
- `normalize_returns_empty_for_non_array_value` (1)
- `normalize_returns_empty_for_none` (1)

### readPaperclipRuntimeSkillEntries
- `runtime_entries_prefers_configured_when_present` (1)
- `runtime_entries_falls_back_to_filesystem_when_unconfigured` (1)

### ensurePaperclipSkillSymlink
- `ensure_creates_skips_repairs_real_path` (1, unix only)
- `ensure_skips_when_target_resolves_to_real_existing_path` (1, unix only)

### removeMaintainerOnlySkillSymlinks
- `remove_maintainer_only_filters_only_dot_agents_targets` (1, unix only)

## 验证结果

- **编译**:`cargo build -p pc-acpx` → `Finished `dev` profile`,0 error
- **fmt**:`cargo fmt --check -p pc-acpx` → clean
- **单元测试**:`cargo test -p pc-acpx --lib skill_io` → 22 passed
- **集成测试**:`cargo test -p pc-acpx --test round390_skill_io` → 18 passed
- **总测试**:`cargo test -p pc-acpx` → **813 passed, 0 failed**
  - R389 末 baseline 773 + skill_io 22 unit + skill_io 18 integration = 813
  - 0 regression
- **pc-acpx 模块数** (lib):38 → 39
- **公开符号 re-export**:新增 12 个 (`PAPERCLIP_SKILL_KEY_PREFIX`,
  `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES`,
  `SkillSymlinkOutcome`, `ensure_paperclip_skill_symlink`,
  `ensure_paperclip_skill_symlink_with_linker`,
  `is_maintainer_only_skill_target`, `list_paperclip_skill_entries`,
  `normalize_configured_paperclip_runtime_skills`,
  `read_installed_skill_targets`,
  `read_paperclip_runtime_skill_entries`,
  `read_paperclip_skill_markdown`,
  `remove_maintainer_only_skill_symlinks`,
  `resolve_paperclip_skills_dir`)

## 与 Node parity 检查

逐项验证(每个 Node 函数都有对应 Rust 实现 + 至少 1 个单元测试 + 至少 1
个集成测试):

| Node | Rust | Unit | Integration |
|---|---|---|---|
| `isMaintainerOnlySkillTarget` | ✓ | 2 | 3 |
| `resolvePaperclipSkillsDir` | ✓ | 2 | 2 |
| `listPaperclipSkillEntries` | ✓ | 2 | 2 |
| `readInstalledSkillTargets` | ✓ | 2 | 1 |
| `normalizeConfiguredPaperclipRuntimeSkills` | ✓ | 3 | 3 |
| `readPaperclipRuntimeSkillEntries` | ✓ | 1 | 2 |
| `readPaperclipSkillMarkdown` | ✓ | 2 | 2 |
| `ensurePaperclipSkillSymlink` | ✓ | 6 | 2 |
| `removeMaintainerOnlySkillSymlinks` | ✓ | 2 | 1 |
| `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES` | ✓ | 0 | 1 |
| `PAPERCLIP_SKILL_KEY_PREFIX` | ✓ | 0 | 1 |

**Node parity: 100%**

## 下一步(R391+ 候选)

### R391 — Adapter 实质实现(关键路径)
现在 `pc-acpx` 提供了完整的 skills 工具链:
- `skill_sync_preference` (R387) — 读写 `paperclipSkillSync` config
- `skill_snapshot` (R388) — 构造 `AdapterSkillSnapshot` / `PersistentSkillSnapshot`
- `skill_materialize` (R389) — materialize + hash + lock + sentinel
- `skill_io` (R390) — async I/O + symlink + maintainer cleanup

**直接的下一步**:在每个 `pc-adapter-{claude,codex,gemini,grok,opencode,pi}-local`
crate 用这 4 块拼图实现 `listXxxSkills` / `syncXxxSkills`,
让 adapter 真正支持 Paperclip skill sync workflow。

### R392+ — 剩余 server-utils.ts helpers
- `parseObject` / `asString` / `asNumber` / `asBoolean` / `asStringArray` / `parseJson` (L350-378)
- `appendWithCap` / `appendWithByteCap` (L381-394)
- `resolvePathValue` / `renderTemplate` (L402-426)
- `joinPromptSections` (L428)
- ... 等

按 docs/09-CURRENT-STATE-AND-NEXT-PLAN.md 整体计划推进。
