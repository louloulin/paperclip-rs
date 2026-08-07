# R394 — Adapter `opencode_local` Skills (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapters/opencode-local/src/server/skills.ts`(82 行)完整移植到 `crates/pc-adapter-opencode-local/src/skills.rs`。这是 R393 gemini-local 的**直接变种**,几乎全部结构相同,核心差异是:
1. `skillsHome` = `~/.claude/skills`(**共享** Claude 目录)
2. Snapshot 强制包含一个 warning(告诉用户 OpenCode 共享 Claude skills home)
3. `installedDetail / missingDetail / externalConflictDetail / externalDetail` 都加上 "shared" 关键词

## 范围

- 新增 `crates/pc-adapter-opencode-local/src/skills.rs`(485 行):4 公开函数 + 11 单元测试
- 新增 `crates/pc-adapter-opencode-local/src/lib.rs`:`pub mod skills;`
- 新增 `crates/pc-adapter-opencode-local/Cargo.toml`:加 `pc-acpx` + `tokio` 依赖
- 新增 `crates/pc-adapter-opencode-local/tests/round394_opencode_skills.rs`(284 行):10 个集成测试
- 跟 Node `packages/adapters/opencode-local/src/server/skills.ts` L1-82 精确对齐

## Node 函数 / 常量映射

| Node function | Rust function | 行号 |
|---|---|---|
| `resolveOpenCodeSkillsHome` (L17-24) | `resolve_opencode_skills_home` + `_with` variant | 纯函数 |
| `buildOpenCodeSkillSnapshot` (L26-44) | `build_opencode_skill_snapshot` | async |
| `listOpenCodeSkills` (L46-48) | `list_opencode_skills` | async |
| `syncOpenCodeSkills` (L50-76) | `sync_opencode_skills` | async, **有副作用** |
| `resolveOpenCodeDesiredSkillNames` (L78-81) | `resolve_opencode_desired_skill_names` | 纯函数 |

## 关键设计决策

### 1. 几乎完全复用 R393 模板
R393 (gemini-local) 与 R394 (opencode-local) 的 Node 实现差异仅限:
- 字符串字面量(`adapterType` / `locationLabel` / `*Detail` 文本)
- 一个强制 warning

Rust 实现遵循同样模式:**结构 100% 复用,只换字符串字面量**。这就是 R393 模板的**第一次复刻验证**。

### 2. 强制 warning 设计
OpenCode 共享 `~/.claude/skills`,所以 snapshot 永远携带:
```rust
warnings: Some(vec![
    "OpenCode currently uses the shared Claude skills home (~/.claude/skills)."
        .to_string(),
]),
```

调用方无需检查就知道 OpenCode 与 Claude 共享目录 — 避免用户在 Claude 配置里清除 skills 时同时误删 OpenCode 的 skills。

### 3. 没有"Noop"分支
不像 Claude/Codex,Gemini/OpenCode **总是有 skillsHome**(路径固定),
但仍然走 `skills_home: Option<&Path>` 接口,允许 `None` 时优雅降级
(返回 snapshot 但加 missing-home warning)。

### 4. Sync 副作用逻辑完全镜像 R393
两步流程(create/repair + remove stale)与 R393 一致,因为:
- 同样的 symlink-based sync 模型
- 同样的 `ensure_paperclip_skill_symlink` + `tokio::fs::remove_file`
- 同样的"不动外部 symlink"保护

## 单元测试(11 个,在 `skills.rs` 内)

### resolve_opencode_skills_home
- `skills_home_uses_configured_home` (1)
- `skills_home_trims_whitespace` (1)
- `skills_home_returns_none_when_unset` (1)
- `skills_home_with_default_falls_back` (1)

### build / list
- `list_uses_filesystem_when_config_empty` (1)
- `list_with_no_skills_home_still_builds_snapshot` (1)

### sync (R393 模板复用)
- `sync_creates_symlinks_for_desired_skills` (1, unix only)
- `sync_removes_stale_symlinks_for_undesired_skills` (1, unix only)
- `sync_does_not_remove_external_symlinks` (1, unix only)
- `sync_creates_skills_home_when_missing` (1)

### resolve_opencode_desired_skill_names
- `desired_names_delegate_to_pc_acpx` (1)

## 集成测试(10 个,`tests/round394_opencode_skills.rs`)

### Constants
- `opencode_skills_home_suffix_is_dot_claude_skills` (1)

### resolve_opencode_skills_home
- `skills_home_prefers_env_home_when_set` (1)
- `skills_home_with_fallback_uses_default` (1)
- `skills_home_with_fallback_honours_env_override` (1)

### list_opencode_skills
- `list_opencode_skills_returns_supported_snapshot` (1)
- `list_opencode_skills_surfaces_extra_warning_when_no_skills_home` (1)

### sync_opencode_skills
- `sync_opencode_skills_end_to_end_full_lifecycle` (1, unix only)
- `sync_opencode_skills_accepts_empty_desired_skills` (1)

### resolve_opencode_desired_skill_names
- `resolve_desired_names_with_configured_sync_preference` (1)

### build_opencode_skill_snapshot
- `build_snapshot_matches_list_call` (1)

## 验证结果

- **编译**:`cargo build -p pc-adapter-opencode-local` → `Finished`,0 error
- **fmt**:`cargo fmt -p pc-adapter-opencode-local` → clean
- **opencode-local lib 单元**:`cargo test -p pc-adapter-opencode-local --lib skills` → 11 passed
- **round394 集成**:`cargo test -p pc-adapter-opencode-local --test round394_opencode_skills` → 10 passed
- **opencode-local 总**:`cargo test -p pc-adapter-opencode-local` → **30 tests, 0 failed**
  - lib 单元: 19 (R394 增量 11 skills + 既有 8)
  - round394 集成: 10 (R394 增量)
  - 既有集成: 1
- **0 regression**

## 与 Node parity 检查

| Node | Rust | Unit | Integration |
|---|---|---|---|
| `resolveOpenCodeSkillsHome` (L17-24) | ✓ | 4 | 3 |
| `buildOpenCodeSkillSnapshot` (L26-44) | ✓ | (via list/sync) | 1 |
| `listOpenCodeSkills` (L46-48) | ✓ | 2 | 2 |
| `syncOpenCodeSkills` (L50-76) | ✓ | 4 | 2 |
| `resolveOpenCodeDesiredSkillNames` (L78-81) | ✓ | 1 | 1 |

**OpenCode-local skills: 100% Node parity**

## 模板复用率(R393 → R394)

| 部分 | 复用情况 |
|---|---|
| `build_*_skill_snapshot` | 90%(仅 detail 字符串 + warning 不同) |
| `list_*_skills` | 90% |
| `sync_*_skills` | 100%(完全相同的两步流程) |
| `resolve_*_skills_home` / `_with` | 100%(home 路径是常量字符串差异) |
| `resolve_*_desired_skill_names` | 100% |

R394 **第一次正式验证了"persistent + side-effecting sync"模板可复用性**。
后续 pi-local / cursor-local 等 adapter 可以走相同 pattern。

## 累计进度

- R391 ✅ claude-local skills (runtime-mounted, no-op sync)
- R392 ✅ codex-local skills (runtime-mounted, no-op sync, 简化)
- R393 ✅ gemini-local skills (persistent, **side-effecting sync**) ⭐
- **R394 ✅ opencode-local skills (persistent, side-effecting sync, 共享 Claude home)** ⭐

## 下一步(R395+ 候选)

### R395 — `pc-adapter-pi-local` Skills
pi-local 用 `~/.pi/agent/skills`,无 sync(类似 Claude,但 location
不同)— **runtime-mounted 模板的第三个变种**。

### R396 — `pc-adapter-grok-local` Skills
grok-local **无独立 skillsHome**,只在 snapshot 中 mention `.claude/skills`
作为 expected location。**最小可能的 skills 模板**(只 list,无 sync,无
filesystem I/O)。

### R397 — `pc-adapter-cursor-local` Skills
cursor-local 有 sync 副作用,**结构类似 gemini**,但 `skillsHome` 解析
逻辑不同。

### R398 — `pc-adapter-hermes` Skills
hermes 自定义,**最复杂的 adapter skills**,需要传入 skillsHome,有
分类逻辑。
