# R393 — Adapter `gemini_local` Skills (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapters/gemini-local/src/server/skills.ts`(78 行)完整移植到 `crates/pc-adapter-gemini-local/src/skills.rs`。这是 paperclip-rs **第一个有副作用 sync 的 adapter 样板**:
- `syncGeminiSkills` 创建 desired symlinks via `ensure_paperclip_skill_symlink`
- 删除已不再 desired 的 Paperclip-managed symlinks
- 跳过外部安装的 symlinks

完成后 paperclip-rs 拥有**双模板**:
- R391/R392:list-only 模板(Claude / Codex)
- **R393**:list + 副作用 sync 模板(Gemini)

## 范围

- 新增 `crates/pc-adapter-gemini-local/src/skills.rs`(502 行):4 公开函数 + 11 单元测试
- 新增 `crates/pc-adapter-gemini-local/src/lib.rs`:`pub mod skills;`
- 新增 `crates/pc-adapter-gemini-local/Cargo.toml`:加 `pc-acpx` + `tokio` 依赖
- 新增 `crates/pc-adapter-gemini-local/tests/round393_gemini_skills.rs`(289 行):10 个集成测试
- 跟 Node `packages/adapters/gemini-local/src/server/skills.ts` L1-78 精确对齐

## Node 函数 / 常量映射

| Node function | Rust function | 行号 |
|---|---|---|
| `resolveGeminiSkillsHome` (L17-24) | `resolve_gemini_skills_home` + `_with` variant | 纯函数 |
| `buildGeminiSkillSnapshot` (L26-40) | `build_gemini_skill_snapshot` | async |
| `listGeminiSkills` (L42-44) | `list_gemini_skills` | async |
| `syncGeminiSkills` (L46-72) | `sync_gemini_skills` | async, **有副作用** |
| `resolveGeminiDesiredSkillNames` (L74-77) | `resolve_gemini_desired_skill_names` | 纯函数 |

## 关键设计决策

### 1. 用 `build_persistent_skill_snapshot` 而非 `build_runtime_mounted_skill_snapshot`
Node Gemini 使用 persistent snapshot,因为 Gemini CLI 启动时直接读
`<skillsHome>` — Paperclip 必须提前 materialise。镜像 Node `L29`:
- `installed: BTreeMap<String, InstalledSkillTarget>`(required)
- `skillsHome: String`(required)
- `locationLabel / missingDetail / externalConflictDetail / externalDetail`(optional)

Rust `PersistentSkillSnapshotOptions` 的 `installed` / `skillsHome` 字段
是 required(non-Option),所以 `list_gemini_skills(skills_home: None)`
走单独分支传空 `installed`。

### 2. `sync_gemini_skills` 是真正的有副作用操作
两步流程:
1. **创建/修复**:对每个 desired 的 entry,调
   `ensure_paperclip_skill_symlink(source, target)` — 创建或修复 symlink。
2. **删除**:对 installed 中 `target_path == available.source` 且 entry
   不再 desired 的 symlink,直接 `tokio::fs::remove_file(target)`。
   镜像 Node `fs.unlink(...).catch(() => {})` 静默失败。

**关键修复**(单元测试发现):不能用 `ensure_*` 来"删除"(它只
Created/Repaired/Skipped),必须直接 `remove_file`。

### 3. 镜像 Node 的"外部 symlink 不动"语义
删除分支检查 `installedEntry.targetPath !== available.source` → 不删除。
这保护用户从外部手动安装的同名 symlink,只清理 Paperclip 之前
managed 但现在不再 desired 的。

### 4. `tokio = { workspace = true }` 移入 `[dependencies]`
Gemini sync 用了 `tokio::fs::create_dir_all` + `tokio::fs::remove_file`
(不只是 symlink metadata),需要从 `dev-dependencies` 移到
`[dependencies]`。

### 5. `skills_home` 作参数(不是 ctx 解析)
与 Claude-local 一致:`resolve_gemini_skills_home_with(ctx, default_home)`
分两步,主函数 (`sync_gemini_skills`) 接受已解析的 `skills_home` 参数
— 这样调用者(server)可以注入默认 home,测试可以注入 sandbox 路径。

## 单元测试(11 个,在 `skills.rs` 内)

### resolve_gemini_skills_home
- `skills_home_uses_configured_home` (1)
- `skills_home_trims_whitespace` (1)
- `skills_home_returns_none_when_unset` (1)
- `skills_home_with_default_falls_back` (1)

### build / list
- `list_uses_filesystem_when_config_empty` (1)
- `list_with_no_skills_home_still_builds_snapshot` (1)

### sync (核心新功能)
- `sync_creates_symlinks_for_desired_skills` (1, unix only)
- `sync_removes_stale_symlinks_for_undesired_skills` (1, unix only)
- `sync_does_not_remove_external_symlinks` (1, unix only)
- `sync_creates_skills_home_when_missing` (1)

### resolve_gemini_desired_skill_names
- `desired_names_delegate_to_pc_acpx` (1)

## 集成测试(10 个,`tests/round393_gemini_skills.rs`)

### Constants
- `gemini_skills_home_suffix_is_dot_gemini_skills` (1)

### resolve_gemini_skills_home
- `skills_home_prefers_env_home_when_set` (1)
- `skills_home_with_fallback_uses_default` (1)
- `skills_home_with_fallback_honours_env_override` (1)

### list_gemini_skills
- `list_gemini_skills_returns_supported_snapshot` (1)
- `list_gemini_skills_surfaces_warning_when_no_skills_home` (1)

### sync_gemini_skills (核心)
- `sync_gemini_skills_end_to_end_full_lifecycle` (1, unix only) — 完整 3 步
  lifecycle: create both → drop one → drop all,验证外部 symlink 不动
- `sync_gemini_skills_accepts_empty_desired_skills` (1)

### resolve_gemini_desired_skill_names
- `resolve_desired_names_with_configured_sync_preference` (1)

### build_gemini_skill_snapshot
- `build_snapshot_matches_list_call` (1)

## 验证结果

- **编译**:`cargo build -p pc-adapter-gemini-local` → `Finished`,0 error
- **fmt**:`cargo fmt -p pc-adapter-gemini-local` → clean
- **gemini-local lib 单元**:`cargo test -p pc-adapter-gemini-local --lib skills` → 11 passed
- **round393 集成**:`cargo test -p pc-adapter-gemini-local --test round393_gemini_skills` → 10 passed
- **gemini-local 总**:`cargo test -p pc-adapter-gemini-local` → **30 tests, 0 failed**
  - lib 单元: 19 (R393 增量 11 skills + 既有 8)
  - round393 集成: 10 (R393 增量)
  - 既有集成: 1
- **0 regression**

## 与 Node parity 检查

| Node | Rust | Unit | Integration |
|---|---|---|---|
| `resolveGeminiSkillsHome` (L17-24) | ✓ | 4 | 3 |
| `buildGeminiSkillSnapshot` (L26-40) | ✓ | (via list/sync) | 1 |
| `listGeminiSkills` (L42-44) | ✓ | 2 | 2 |
| `syncGeminiSkills` (L46-72) | ✓ | 4 | 2 |
| `resolveGeminiDesiredSkillNames` (L74-77) | ✓ | 1 | 1 |

**Gemini-local skills: 100% Node parity**

## 模板对比

| Adapter | Snapshot 形状 | Sync 副作用 | 行数 |
|---|---|---|---|
| claude-local (R391) | runtime-mounted | no-op | 365 |
| codex-local (R392) | runtime-mounted | no-op | 287 |
| **gemini-local (R393)** | **persistent** | **create + remove symlinks** | **502** |
| opencode-local (R394) | runtime-mounted | symlink (类似 gemini) | ~400 |
| pi-local (R395) | runtime-mounted | no-op | ~290 |

## 下一步(R394+ 候选)

### R394 — `pc-adapter-opencode-local` Skills
opencode-local 用 `~/.claude/skills` 共享,有 sync 副作用(类似
gemini),模板与 R393 接近但 detail 文本不同。

### R395 — `pc-adapter-pi-local` Skills
pi-local 用 `~/.pi/agent/skills`,无 sync(类似 Claude,但 location
不同)。

### R396 — `pc-adapter-grok-local` Skills
grok-local **无独立 skillsHome**,只在 snapshot 中 mention `.claude/skills`
作为 expected location。
