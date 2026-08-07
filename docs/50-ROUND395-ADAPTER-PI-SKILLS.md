# R395 — Adapter `pi_local` Skills (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapters/pi-local/src/server/skills.ts`(76 行)完整移植到 `crates/pc-adapter-pi-local/src/skills.rs`。这是 R393 gemini-local 和 R394 opencode-local 模板的**第三个变种**,核心差异仅:
1. `skillsHome` = `~/.pi/agent/skills`(不是 `.gemini` 或 `.claude`)
2. `installedDetail` 字段(Node 不传 → Rust 用 `None`)
3. `locationLabel` = `~/.pi/agent/skills`

## 范围

- 新增 `crates/pc-adapter-pi-local/src/skills.rs`(466 行):4 公开函数 + 11 单元测试
- 新增 `crates/pc-adapter-pi-local/src/lib.rs`:`pub mod skills;`
- 新增 `crates/pc-adapter-pi-local/Cargo.toml`:加 `pc-acpx` + `tokio` 依赖
- 新增 `crates/pc-adapter-pi-local/tests/round395_pi_skills.rs`(281 行):10 个集成测试
- 跟 Node `packages/adapters/pi-local/src/server/skills.ts` L1-76 精确对齐

## Node 函数 / 常量映射

| Node function | Rust function | 行号 |
|---|---|---|
| `resolvePiSkillsHome` (L17-24) | `resolve_pi_skills_home` + `_with` variant | 纯函数 |
| `buildPiSkillSnapshot` (L26-39) | `build_pi_skill_snapshot` | async |
| `listPiSkills` (L41-43) | `list_pi_skills` | async |
| `syncPiSkills` (L45-71) | `sync_pi_skills` | async, **有副作用** |
| `resolvePiDesiredSkillNames` (L73-76) | `resolve_pi_desired_skill_names` | 纯函数 |

## 关键设计决策

### 1. 完全复用 R393 / R394 模板结构
代码结构 95% 相同,只改:
- `PI_SKILLS_HOME_SUFFIX = ".pi/agent/skills"`
- `adapter_type = "pi_local"`
- `locationLabel = "~/.pi/agent/skills"`
- `installedDetail: None`(Node 不传)
- `missingDetail / externalConflictDetail / externalDetail` 文本(不含 "shared")

### 2. **第二次正式验证** "persistent + side-effecting sync" 模板可复用性
R393 → R394 → R395,模板复用率稳定在 90%+。后续 pi / cursor / hermes 等
adapter 都可以走相同 pattern,**显著降低实现成本**。

### 3. Pi 的 sync 行为完全镜像 R393 / R394
两步流程(create/repair + remove stale)没有任何 Pi-specific 分支:
- 创建 desired symlinks via `ensure_paperclip_skill_symlink`
- 删除 installed 中 `target_path == available.source` 且 entry 不再 desired 的
- 跳过外部 symlinks(保护用户安装)

## 单元测试(11 个,在 `skills.rs` 内)

### resolve_pi_skills_home
- `skills_home_uses_configured_home` (1)
- `skills_home_trims_whitespace` (1)
- `skills_home_returns_none_when_unset` (1)
- `skills_home_with_default_falls_back` (1)

### build / list
- `list_uses_filesystem_when_config_empty` (1)
- `list_with_no_skills_home_still_builds_snapshot` (1)

### sync (模板复用验证)
- `sync_creates_symlinks_for_desired_skills` (1, unix only)
- `sync_removes_stale_symlinks_for_undesired_skills` (1, unix only)
- `sync_does_not_remove_external_symlinks` (1, unix only)
- `sync_creates_skills_home_when_missing` (1)

### resolve_pi_desired_skill_names
- `desired_names_delegate_to_pc_acpx` (1)

## 集成测试(10 个,`tests/round395_pi_skills.rs`)

### Constants
- `pi_skills_home_suffix_is_dot_pi_agent_skills` (1)

### resolve_pi_skills_home
- `skills_home_prefers_env_home_when_set` (1)
- `skills_home_with_fallback_uses_default` (1)
- `skills_home_with_fallback_honours_env_override` (1)

### list_pi_skills
- `list_pi_skills_returns_supported_snapshot` (1)
- `list_pi_skills_surfaces_warning_when_no_skills_home` (1)

### sync_pi_skills
- `sync_pi_skills_end_to_end_full_lifecycle` (1, unix only)
- `sync_pi_skills_accepts_empty_desired_skills` (1)

### resolve_pi_desired_skill_names
- `resolve_desired_names_with_configured_sync_preference` (1)

### build_pi_skill_snapshot
- `build_snapshot_matches_list_call` (1)

## 验证结果

- **编译**:`cargo build -p pc-adapter-pi-local` → `Finished`,0 error
- **fmt**:`cargo fmt -p pc-adapter-pi-local` → clean
- **pi-local lib 单元**:`cargo test -p pc-adapter-pi-local --lib skills` → 11 passed
- **round395 集成**:`cargo test -p pc-adapter-pi-local --test round395_pi_skills` → 10 passed
- **pi-local 总**:`cargo test -p pc-adapter-pi-local` → **30 tests, 0 failed**
  - lib 单元: 19 (R395 增量 11 skills + 既有 8)
  - round395 集成: 10 (R395 增量)
  - 既有集成: 1
- **0 regression**

## 与 Node parity 检查

| Node | Rust | Unit | Integration |
|---|---|---|---|
| `resolvePiSkillsHome` (L17-24) | ✓ | 4 | 3 |
| `buildPiSkillSnapshot` (L26-39) | ✓ | (via list/sync) | 1 |
| `listPiSkills` (L41-43) | ✓ | 2 | 2 |
| `syncPiSkills` (L45-71) | ✓ | 4 | 2 |
| `resolvePiDesiredSkillNames` (L73-76) | ✓ | 1 | 1 |

**Pi-local skills: 100% Node parity**

## 累计进度

| Adapter | Snapshot | Sync | 行数 | 状态 |
|---|---|---|---|---|
| claude-local | runtime-mounted | no-op | 365 | R391 ✅ |
| codex-local | runtime-mounted | no-op | 287 | R392 ✅ |
| gemini-local | persistent | side-effect | 502 | R393 ✅ |
| opencode-local | persistent | side-effect | 485 | R394 ✅ |
| **pi-local** | **persistent** | **side-effect** | **466** | **R395 ✅** |

## 下一步(R396+ 候选)

### R396 — `pc-adapter-grok-local` Skills(最小模板)
grok-local **无独立 skillsHome**,只在 snapshot 中 mention `.claude/skills`
作为 expected location。**最小可能的 skills 模板**(只 list,无 sync,无
filesystem I/O)— runtime-mounted 但 snapshot 上始终 warning。

### R397 — `pc-adapter-cursor-local` Skills
cursor-local 有 sync 副作用,**结构类似 gemini**,但 `skillsHome` 解析
逻辑不同(无固定 suffix,直接读 config)。

### R398 — `pc-adapter-hermes` Skills
hermes 自定义,**最复杂的 adapter skills**,需要传入 skillsHome,有
分类逻辑(`read_categories`)。

### R399 — Adapter skills 接入 server
在 `pc-server` 中加 skills sync 端点 + 在 `pc-agent` 中接入 skill 准备阶段,
把 R391-R398 的工具接入请求路径。
