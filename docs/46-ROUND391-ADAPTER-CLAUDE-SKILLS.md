# R391 — Adapter `claude_local` Skills (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapters/claude-local/src/server/skills.ts`
完整移植到 `crates/pc-adapter-claude-local/src/skills.rs`,作为 R388
(`skill_snapshot` builders) + R389 (`skill_materialize`) + R390
(`skill_io`) 工具链的**第一个具体 adapter 实现**。

完成后,`pc-adapter-claude-local` 暴露 `list_claude_skills` /
`sync_claude_skills` 两个公开函数,server 端只要按 adapter_type
派发即可。其他 7 个 adapter (codex / gemini / grok / opencode /
pi / cursor / hermes) 沿用同一模板逐个落地。

## 范围

- 新增 `crates/pc-acpx/src/adapter_skills.rs`(144 行):`AdapterSkillContext`
  结构 + 3 个小 helper(`lookup_path` / `env_object`) + 5 个单元测试
- 新增 `crates/pc-acpx/src/lib.rs`:`pub mod adapter_skills;` +
  `pub use adapter_skills::AdapterSkillContext;`(按字母序在 `acp_runtime`
  之前)
- 新增 `crates/pc-adapter-claude-local/src/skills.rs`(365 行):
  - `CLAUDE_SKILLS_HOME_SUFFIX` 常量
  - `resolve_claude_skills_home` / `resolve_claude_skills_home_with`
    helpers(镜像 Node `resolveClaudeSkillsHome` L18-25)
  - `build_claude_skill_snapshot` 主函数(镜像 Node `buildClaudeSkillSnapshot` L27-43)
  - `list_claude_skills` / `sync_claude_skills` 公开 API
  - `resolve_claude_desired_skill_names` thin wrapper
  - 12 个单元测试
- 新增 `crates/pc-adapter-claude-local/Cargo.toml`:加 `pc-acpx` 依赖
- 新增 `crates/pc-adapter-claude-local/src/lib.rs`:`pub mod skills;`
  模块声明
- 新增 `crates/pc-adapter-claude-local/tests/round391_claude_skills.rs`
  (~270 行):11 个集成测试
- 跟 Node `packages/adapters/claude-local/src/server/skills.ts`
  L1-64 精确对齐

## Node 函数 / 常量映射

| Node function | Rust function | 行号 |
|---|---|---|
| `__moduleDir = path.dirname(fileURLToPath(...))` | module_dir 参数 | 调用方负责 |
| `resolveClaudeSkillsHome` (L18-25) | `resolve_claude_skills_home` + `_with` variant | 纯函数 |
| `buildClaudeSkillSnapshot` (L27-43) | `build_claude_skill_snapshot` | async |
| `listClaudeSkills` (L45-47) | `list_claude_skills` | async |
| `syncClaudeSkills` (L49-52) | `sync_claude_skills` | async |
| `resolveClaudeDesiredSkillNames` (L54-59) | `resolve_claude_desired_skill_names` | 纯函数 |
| `AdapterSkillContext` (types.ts L278-283) | `pc_acpx::AdapterSkillContext` | 新增 |

## 关键设计决策

### 1. `AdapterSkillContext` 放在 `pc-acpx` 而非 `pc-adapter-api`
`AdapterSkillContext` 是 adapter / pc-acpx 共享的 value type。放在
`pc-acpx` 避免 `pc-adapter-api` 反向依赖 `pc-acpx`(架构上是
adapter-api 在底层)。`pc-acpx` 已经导出 `AdapterSkillSnapshot`,
把上下文放在同一 crate 内语义一致。

### 2. 不改 `Adapter` trait,adapter 暴露 `list_*_skills` / `sync_*_skills`
Node 的 `listSkills` / `syncSkills` 是 Adapter 的**可选方法**(用 `?:`)。
Rust 14 个 adapter 已实现 `Adapter` trait,加方法会破坏所有 impl。
选择**每个 adapter crate 单独暴露** `pub async fn list_x_skills(...)`
作为 adapter skills 的入口,server 端按 `adapter_type` 派发即可 —
更灵活,且不破坏现有 execute 流程。

### 3. Claude local sync 是 no-op(只 list)
Node `syncClaudeSkills` 也只是返回 `buildClaudeSkillSnapshot`,
不实际修改文件系统,因为 Claude skills 是通过
`prepare_claude_skill_runtime` 在 prompt bundle 中 materialise 的,
adapter 层不需要 symlink / copy。Rust 镜像这一行为,
`sync_claude_skills` 接受 `desired_skills` 参数只为 trait parity,
实际不消费。

### 4. `module_dir` 作为参数而非 hard-coded
Node `__moduleDir` 在编译期确定,Rust 因为测试要用不同 module_dir
发现 candidate 路径,改为函数参数。生产代码传 `CARGO_MANIFEST_DIR`,
测试传 unique scratch dir。

### 5. `BTreeMap<String, InstalledSkillTarget>` 从函数返回的 "empty" → `None`
`read_installed_skill_targets` 缺失目录返回空 map。builder
`external_installed` 字段是 `Option<BTreeMap>`,空 map 转 `None`
避免在 snapshot 中携带无意义的 "externalInstalled: {}"。

### 6. `SkillDetail::Static` 显式包装
`configured_detail` 字段是 `SkillDetail` enum,而不是 `String`。
Node 用字符串字面量,Rust 用 `SkillDetail::Static("...".to_string())`,
为后续动态详情(`Dynamic(closure)`)留余地。

## 单元测试(12 个,在 `skills.rs` 内)

### resolve_claude_skills_home
- `skills_home_uses_configured_home` (1)
- `skills_home_trims_whitespace` (1)
- `skills_home_returns_none_when_unset` (1)
- `skills_home_returns_none_when_home_empty_string` (1)
- `skills_home_returns_none_when_env_is_not_object` (1)
- `skills_home_with_default_falls_back` (1)

### build / list / sync
- `list_uses_filesystem_when_config_empty` (1)
- `sync_returns_same_shape_as_list` (1)
- `list_reflects_configured_runtime_skills` (1)
- `list_records_external_installed_targets` (1)

### resolve_claude_desired_skill_names
- `desired_names_delegate_to_pc_acpx` (1)
- `desired_names_resolve_configured_keys` (1)

## 集成测试(11 个,`tests/round391_claude_skills.rs`)

### Constants
- `claude_skills_home_suffix_is_dot_claude_skills` (1)

### resolve_claude_skills_home
- `skills_home_prefers_env_home_when_set` (1)
- `skills_home_with_fallback_uses_default` (1)
- `skills_home_with_fallback_honours_env_override` (1)

### list_claude_skills
- `list_claude_skills_returns_supported_snapshot` (1)
- `list_claude_skills_records_external_targets` (1)

### sync_claude_skills
- `sync_claude_skills_returns_snapshot_with_same_shape_as_list` (1)
- `sync_claude_skills_accepts_desired_skills_argument` (1)

### resolve_claude_desired_skill_names
- `resolve_desired_names_with_configured_sync_preference` (1)

### build_claude_skill_snapshot
- `build_snapshot_matches_list_call` (1)

### Snapshot shape stability
- `snapshot_shape_is_stable_for_minimal_config` (1)

## 验证结果

- **编译**:`cargo build -p pc-acpx` / `cargo build -p pc-adapter-claude-local` → `Finished`,0 error
- **fmt**:`cargo fmt -p pc-adapter-claude-local` → clean
- **pc-acpx 单元测试**:`cargo test -p pc-acpx --lib` → **818 passed** (R390 末 813 + adapter_skills 5 新增)
- **pc-acpx 集成测试**:`cargo test -p pc-acpx --test` → 360 passed (0 regression)
- **pc-adapter-claude-local 总**:`cargo test -p pc-adapter-claude-local` → **36 tests, 0 failed**
  - lib 单元: 23 (R391 增量 12, 既有 11)
  - round391 集成: 11 (R391 增量)
  - 既有集成: 2
- **0 regression**

## 与 Node parity 检查

逐项验证(每个 Node 函数都有对应 Rust 实现 + 至少 1 个单元测试 + 至少 1
个集成测试):

| Node | Rust | Unit | Integration |
|---|---|---|---|
| `AdapterSkillContext` (L278-283) | ✓ (pc-acpx) | 5 | n/a |
| `resolveClaudeSkillsHome` (L18-25) | ✓ | 6 | 3 |
| `buildClaudeSkillSnapshot` (L27-43) | ✓ | (via list/sync) | 1 |
| `listClaudeSkills` (L45-47) | ✓ | 4 | 2 |
| `syncClaudeSkills` (L49-52) | ✓ | 1 | 2 |
| `resolveClaudeDesiredSkillNames` (L54-59) | ✓ | 2 | 1 |
| `CLAUDE_SKILLS_HOME_SUFFIX` | ✓ | 0 | 1 |

**Node parity: 100% (claude-local skills)**

## 下一步(R392+ 候选)

### R392 — `pc-adapter-codex-local` Skills(简单变种)
codex-local skills 没有 skillsHome(Node 只调
`buildRuntimeMountedSkillSnapshot` 不传 skillsHome),是 claude-local
模板的最简化版,**最快可完成的第二个 adapter**。

### R393 — `pc-adapter-gemini-local` Skills(sync 有实质操作)
gemini-local 有完整的 symlink sync 逻辑(创建新 desired / 移除
undesired),需要 `ensurePaperclipSkillSymlink` + fs.unlink,是
**模板升级到有副作用 sync** 的样板。

### R394+ — `grok-local` / `opencode-local` / `pi-local` / `cursor-local` / `hermes`
沿用 R391/R392/R393 模板,每个 ~50-150 行,集中复刻。

### R395+ — Adapter skills 接入 server
`server/src/adapters/registry.ts` 收集 `listSkills` / `syncSkills`,
对应 paperclip-rs 需要在 `pc-server` 中加 skills sync 端点 + 在
`pc-agent` 中接入 skill 准备阶段。R388/R389/R390/R391 已把底层
拼图凑齐,接入是 wiring 工作。
