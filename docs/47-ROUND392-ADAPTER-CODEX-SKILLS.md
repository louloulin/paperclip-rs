# R392 — Adapter `codex_local` Skills (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapters/codex-local/src/server/skills.ts`(49 行)完整移植到 `crates/pc-adapter-codex-local/src/skills.rs`。作为 R391 的简化变种,**验证"无 skillsHome"模板可独立运行**,也为后续 `cursor-local` / `grok-local` / `opencode-local` / `pi-local` 提供最小可复刻样板。

## 范围

- 新增 `crates/pc-adapter-codex-local/src/skills.rs`(287 行):3 个公开函数 + 6 个单元测试
- 新增 `crates/pc-adapter-codex-local/src/lib.rs`:`pub mod skills;`
- 新增 `crates/pc-adapter-codex-local/Cargo.toml`:加 `pc-acpx` 依赖
- 新增 `crates/pc-adapter-codex-local/tests/round392_codex_skills.rs`(226 行):7 个集成测试
- 跟 Node `packages/adapters/codex-local/src/server/skills.ts` L1-49 精确对齐

## Node 函数 / 常量映射

| Node function | Rust function | 行号 |
|---|---|---|
| `__moduleDir = path.dirname(fileURLToPath(...))` | module_dir 参数 | 调用方负责 |
| `buildCodexSkillSnapshot` (L13-21) | `build_codex_skill_snapshot` | async |
| `listCodexSkills` (L23-25) | `list_codex_skills` | async |
| `syncCodexSkills` (L27-30) | `sync_codex_skills` | async |
| `resolveCodexDesiredSkillNames` (L32-35) | `resolve_codex_desired_skill_names` | 纯函数 |

## 关键设计决策

### 1. 比 claude-local 更简单 — 无 skillsHome / externalInstalled
Node Codex 没有 `resolveCodexSkillsHome`(无 `~/.codex/skills` 共享目录)。
整个 surface 由 Paperclip 管理,skills 由 `prepare_codex_skill_runtime`
materialise 到 per-company managed Codex home。

**Rust 镜像**:`RuntimeMountedSkillSnapshotOptions` 中
`external_installed / external_location_label / external_detail /
skills_home` 全部传 `None`,snapshot 保持最小。

### 2. Sync 仍是 no-op
镜像 Node `syncCodexSkills`:接受 `desiredSkills` 参数(为 trait
parity),但实际不消费,直接 rebuild snapshot。

### 3. `configuredDetail` 精确镜像 Node 文本
"Will be linked into the effective CODEX_HOME/skills/ directory on
the next run." — 包装成 `SkillDetail::Static(...)`。

### 4. 完全复用 R391 模板结构
R392 的所有 helper / 类型定义 / 测试模式与 R391 一致,**只删掉
skillsHome 相关逻辑**,把 R391 的 ~365 行精简到 ~287 行,这是"模板
演化"的天然结果。

## 单元测试(6 个,在 `skills.rs` 内)

### build / list / sync
- `list_uses_filesystem_when_config_empty` (1)
- `list_reflects_configured_runtime_skills` (1)
- `sync_returns_same_shape_as_list` (1)
- `snapshot_has_no_external_installed_block` (1)

### resolve_codex_desired_skill_names
- `desired_names_delegate_to_pc_acpx` (1)
- `desired_names_resolve_configured_keys` (1)

## 集成测试(7 个,`tests/round392_codex_skills.rs`)

### list_codex_skills
- `list_codex_skills_returns_supported_snapshot` (1)
- `list_codex_skills_handles_missing_skills_directory` (1)
- `codex_snapshot_does_not_surface_skills_home` (1)

### sync_codex_skills
- `sync_codex_skills_returns_same_shape_as_list` (1)
- `sync_codex_skills_accepts_desired_skills_argument` (1)

### resolve_codex_desired_skill_names
- `resolve_desired_names_with_configured_sync_preference` (1)

### build_codex_skill_snapshot
- `build_snapshot_matches_list_call` (1)

## 验证结果

- **编译**:`cargo build -p pc-adapter-codex-local` → `Finished`,0 error
- **fmt**:`cargo fmt -p pc-adapter-codex-local` → clean
- **codex-local lib 单元**:`cargo test -p pc-adapter-codex-local --lib skills` → 6 passed
- **round392 集成**:`cargo test -p pc-adapter-codex-local --test round392_codex_skills` → 7 passed
- **codex-local 总**:`cargo test -p pc-adapter-codex-local` → **17 tests, 0 failed**
  - lib 单元: 9 (R392 增量 6 skills + 既有 3)
  - round392 集成: 7 (R392 增量)
  - 既有集成: 1
- **0 regression**

## 与 Node parity 检查

逐项验证(每个 Node 函数都有对应 Rust 实现 + 至少 1 个单元测试 + 至少 1
个集成测试):

| Node | Rust | Unit | Integration |
|---|---|---|---|
| `buildCodexSkillSnapshot` (L13-21) | ✓ | (via list/sync) | 1 |
| `listCodexSkills` (L23-25) | ✓ | 3 | 3 |
| `syncCodexSkills` (L27-30) | ✓ | 1 | 2 |
| `resolveCodexDesiredSkillNames` (L32-35) | ✓ | 2 | 1 |

**Codex-local skills: 100% Node parity**

## 模板复用率

R391 (`pc-adapter-claude-local::skills`) → R392 (`pc-adapter-codex-local::skills`):
- 公共逻辑:`build_*_skill_snapshot` (基本相同)
- 公共逻辑:`list_*_skills` / `sync_*_skills` / `resolve_*_desired_skill_names` (模式相同)
- 差异点:skillsHome / externalInstalled(只 Claude 有)
- 差异点:`adapterType` 字符串 + `configuredDetail` 文本

R393 (gemini-local) 会引入 sync 副作用,模板继续扩展。

## 下一步(R393+ 候选)

### R393 — `pc-adapter-gemini-local` Skills(完整 symlink sync)
Gemini local 是**第一个有副作用 sync 的样板**:
- `syncGeminiSkills` 创建 symlink (`ensurePaperclipSkillSymlink`)
- 移除不再 desired 的 symlink (`fs.unlink`)
- `skillsHome = ~/.gemini/skills`(类似 Claude)

完成后 paperclip-rs 就有了"无副作用 list + 有副作用 sync"双模板。

### R394+ — 其他 adapter
- `grok-local`(无独立 skillsHome,用 `.claude/skills` 共享)
- `opencode-local`(`~/.claude/skills` 共享,有 sync 副作用)
- `pi-local`(`~/.pi/agent/skills`)
- `cursor-local`(有 sync 副作用)

按 R391/R392 模板逐个复刻,每个 ~150-300 行。
