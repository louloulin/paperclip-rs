# Round 367 — Acpx-engine Skill Staging + Managed Home (B3.1 第六阶段)

> 适用版本：`paperclip-rs` 截至 R367（R366 = 1171 → R367 = **1218**，+47 pc-acpx 测试）
> 参考实现：`paperclip` Node（`packages/adapter-utils/src/acpx-engine/execute.ts` 中 `prepareManagedCodexHome` / `prepareClaudeSkillRuntime` / `prepareCodexSkillRuntime` / `prepareGeminiSkillRuntime` / `reconcileManagedCodexSkills` / `materializePaperclipSkillCopy` / `hashPathContents` / `buildSkillSetKey`；`packages/adapter-utils/src/server-utils.ts` 中 `PaperclipSkillEntry` 接口）
> 测试基线：`cargo test -p pc-acpx` 202/202 绿（164 unit + 8+4+4+4+9+9 integration）；`pc-heartbeat` 928/928 全量无回归；`cargo build --workspace --bins` 通过；`cargo fmt --all -- --check` 通过

---

## 🎯 R367 目标

完成 **acpx-engine skill staging 层**的 Rust 化迁移（B3.1 第六阶段）：

1. **fs_ops 扩展**：`ensure_symlink` / `ensure_copied_file` / `symlink_or_copy_file` / `lstat_or_none` / `readlink_or_none` / `remove_path_if_exists` — skill staging seam 的基础 I/O 原语
2. **managed_home**：`prepare_managed_codex_home` + `.paperclip-managed-skills.json` manifest 读写 + `LogStream` / `OnLogSink` 契约
3. **skill_materialize**：`PaperclipSkillEntry` + `SkillSourceStatus` + `materialize_paperclip_skill_copy` + `hash_path_contents` + `build_skill_set_key`
4. **reconcile_skills**：`reconcile_managed_codex_skills` 三阶段 (managed-no-longer-desired → legacy-symlink → safety-net)
5. **skill_runtime**：`prepare_claude_skill_runtime` / `prepare_codex_skill_runtime` / `prepare_gemini_skill_runtime` + 共享 `SkillRuntimeIdentity` + `resolve_selected_runtime_skills` in-crate shim

**为什么这一阶段关键**：skill staging 是 acpx-engine 与 paperclip 主应用**唯一**的 host-side 接口——其他所有层都是 agent-side runtime。R367 把"如何把 paperclip 的 skill 注入到一个 sandbox session"这一契约固化到 Rust,未来 R368+ 接真实 subprocess 时,只需复用这些 helper,不重新发明 staging 流程。

---

## 🏗️ 新增模块

```
crates/pc-acpx/src/
├── fs_ops.rs                # 扩展：symlink/copy/lstat/readlink helpers
├── managed_home.rs          # NEW：managed Codex home + manifest
├── skill_materialize.rs     # NEW：skill copy + content hash cache key
├── reconcile_skills.rs      # NEW：三阶段 managed skills reconciliation
└── skill_runtime.rs         # NEW：三个 agent 的顶层 skill runtime 准备

crates/pc-acpx/tests/
└── round367_skill_staging.rs    # NEW：端到端集成测试
```

---

## 📐 1. fs_ops 扩展

### 新增 helpers

| 函数 | 签名 | Node 对位 | 用途 |
|---|---|---|---|
| `ensure_symlink` | `async fn(link, target) -> Result<(), AcpxError>` | `ensureSymlink` | 创建符号链接 |
| `ensure_copied_file` | `async fn(target, source) -> Result<bool, AcpxError>` | `ensureCopiedFile` | 单文件复制（不存在时返回 false）|
| `symlink_or_copy_file` | `async fn(source, target) -> Result<bool, AcpxError>` | `symlinkOrCopyFile` | symlink 优先，失败时 fallback 到 copy |
| `lstat_or_none` | `async fn(path) -> Option<Metadata>` | `lstat(...).catch(() => null)` | 不抛错的 stat |
| `readlink_or_none` | `async fn(path) -> Option<PathBuf>` | `readlink(...).catch(() => null)` | 不抛错的 readlink |
| `remove_path_if_exists` | `async fn(path) -> Result<bool, AcpxError>` | `removeSkillTarget` | 不存在时返回 false |

### 关键设计

- **跨平台符号链接**：使用 `cfg(unix)` / `cfg(not(unix))` 分支；非 Unix 平台 `ensure_symlink` 返回 `AcpxError::SymlinkUnsupported`
- **blocking 任务包装**：使用 `tokio::task::spawn_blocking` 包装 std fs 操作，避免阻塞 runtime
- **错误枚举扩展**：`AcpxError` 新增 `SymlinkUnsupported` + `Join { context, error }` 两个变体

---

## 📐 2. managed_home

### 公开 API

```rust
pub const PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST: &str = ".paperclip-managed-skills.json";

pub enum LogStream { Stdout, Stderr }
pub type OnLogSink = std::sync::Arc<dyn Fn(LogStream, &str) + Send + Sync>;

#[derive(Serialize, Deserialize)]
pub struct ManagedSkillsManifest {
    #[serde(default = "default_manifest_version")]
    pub version: u32,
    #[serde(rename = "managedSkillNames", default)]
    pub managed_skill_names: Vec<String>,
}

pub struct PrepareManagedCodexHomeInput { /* company_id, source_home, target_home, on_log */ }

pub async fn prepare_managed_codex_home(input: PrepareManagedCodexHomeInput) -> Result<PathBuf, AcpxError>;
pub async fn read_managed_codex_skills_manifest(skills_home: impl AsRef<Path>) -> ManagedSkillsManifest;
pub async fn write_managed_codex_skills_manifest(skills_home: impl AsRef<Path>, manifest: &ManagedSkillsManifest) -> Result<(), AcpxError>;
```

### 关键设计

- **serde rename**：`managedSkillNames` (camelCase) 匹配 Node on-disk JSON 契约；Rust 字段是 snake_case
- **容错读**：`read_managed_codex_skills_manifest` 把任何 read/parse 错误折成空 manifest（Node 端用 try/catch 实现相同语义）
- **幂等**：当 `source_home == target_home`（resolved）时整个函数是 no-op
- **auth.json 是 symlink，config 文件是 copy**：和 Node 完全一致——auth 必须保持 canonical（避免 logout/login 状态分裂），配置必须隔离（用户可能多 worktree）

### 单元测试覆盖（9 个）

- `manifest_round_trips_through_disk` — JSON 序列化/反序列化
- `manifest_read_returns_empty_when_missing` — 容错读
- `manifest_read_falls_back_on_corrupt_json` — 损坏 JSON 容错
- `manifest_from_names_sorts_and_dedupes`
- `prepare_managed_codex_home_is_noop_when_paths_match`
- `prepare_managed_codex_home_creates_target_and_copies_files`
- `prepare_managed_codex_home_emits_log_line`
- `prepare_managed_codex_home_skips_missing_config_files`
- `manifest_default_version_is_one`

---

## 📐 3. skill_materialize

### 公开 API

```rust
#[derive(Serialize, Deserialize)]
pub struct PaperclipSkillEntry {
    pub key: String,
    #[serde(rename = "runtimeName")] pub runtime_name: String,
    #[serde(with = "path_buf_serde")] pub source: PathBuf,
    #[serde(rename = "versionId", ...)] pub version_id: Option<String>,
    #[serde(rename = "currentVersionId", ...)] pub current_version_id: Option<String>,
    #[serde(rename = "sourceStatus", ...)] pub source_status: Option<SkillSourceStatus>,
    #[serde(rename = "missingDetail", ...)] pub missing_detail: Option<String>,
}

pub enum SkillSourceStatus { Available, Missing }

#[derive(Default)]
pub struct MaterializedSkillCopyResult {
    pub copied_files: usize,
    pub skipped_symlinks: Vec<String>,
}

pub async fn materialize_paperclip_skill_copy(source, target) -> Result<MaterializedSkillCopyResult, AcpxError>;
pub fn hash_path_contents(...) -> Pin<Box<dyn Future<Output = ()> + Send>>;  // 递归，boxed
pub async fn build_skill_set_key(skills: &[PaperclipSkillEntry], label: &str) -> String;
```

### 关键设计

- **递归 async 用 Pin<Box<dyn Future>>**：`hash_path_contents` 必须 boxed-recursive，避免无穷大小 future
- **PathBuf serde helper**：`source` 字段用 `path_buf_serde` 模块处理（`PathBuf` 没有 `DeserializeOwned`）
- **symlink 跳过**：`materialize_paperclip_skill_copy` 递归复制，**丢弃**（不跟随）所有 symlink，sandobx 不信任任意用户路径
- **同源短路**：当 source 和 target resolved 相同时返回 `skipped_symlinks: [source]`，让调用方可以发现 circular reference

### Cache Key 设计

```
sha256(
  "paperclip-acpx-${label}-skills:v1\n" +
  sorted(skills).for_each(|entry| "skill:${key}:${runtime_name}\n" + hash_path_contents(source))
)
```

`label` 是 agent 标识（`"claude"` / `"codex"` / `"gemini"`），不同 agent 用同一组 skills 产生不同 key（缓存隔离）。

### 单元测试覆盖（8 个）

- `materialize_returns_self_when_source_equals_target`
- `materialize_copies_files_and_directories_recursively`
- `materialize_drops_symlinks`
- `materialize_overwrites_existing_target`
- `build_skill_set_key_is_deterministic`
- `build_skill_set_key_changes_when_skill_contents_change`
- `build_skill_set_key_changes_with_label`
- `skill_entry_round_trips_through_json`

---

## 📐 4. reconcile_skills

### 公开 API

```rust
pub struct ReconcileManagedCodexSkillsInput { /* skills_home, all_skills, selected_skills, on_log */ }

pub enum RevocationPhase { ManagedNoLongerDesired, LegacySymlink, ManagedButUnavailable }

pub struct RevocationRecord { pub name: String, pub phase: RevocationPhase }

pub async fn reconcile_managed_codex_skills(input) -> Result<Vec<RevocationRecord>, AcpxError>;
```

### 三阶段语义（与 Node 完全一致）

1. **Phase 1 — managed no longer desired**：manifest 中有但 `selected_skills` 没有的 name → 删除
2. **Phase 2 — legacy symlinks**：`all_skills` 中有但不在 `desired` 和 `managed` 中，且 target 是 symlink 指向 legacy source → 删除（catch 老 paperclip 版本的 symlink 残留）
3. **Phase 3 — safety net**：manifest 中有但不在 `desired` 和 `available` 中 → 再删一次（Phase 1 已处理，幂等兜底）

### 关键设计

- **macOS `/var` → `/private/var` 处理**：`resolve_link` 内调用 `canonicalize()`，避免绝对路径 vs realpath 解析路径不一致
- **返回 RevocationRecord 而不是 silent removal**：调用方 / 测试可以精确断言每个 phase 的行为
- **`read_managed_codex_skills_manifest` 在函数开头读一次**：Phase 1 删除文件后 manifest 视图不变，Phase 3 看到的是同一份 manifest

### 单元测试覆盖（5 个）

- `phase_one_revokes_managed_no_longer_desired`
- `phase_three_is_safety_net_after_phase_one`
- `phase_two_revokes_legacy_symlink`（unix only）
- `reconcile_emits_log_lines_via_sink`
- `reconcile_is_noop_when_everything_already_aligned`

---

## 📐 5. skill_runtime

### 公开 API

```rust
pub struct SkillRuntimeIdentity {
    pub mode: String,
    pub skill_set_key: String,
    pub desired_skill_names: Vec<String>,
    pub selected_skills: Vec<String>,
    pub skills_home: PathBuf,
    pub codex_home: Option<PathBuf>,
    pub bundle_root: Option<PathBuf>,
}

pub struct PrepareSkillRuntimeOutput {
    pub identity: SkillRuntimeIdentity,
    pub command_notes: Vec<String>,
    pub prompt_instructions: String,  // Claude only
}

pub async fn prepare_claude_skill_runtime(input: PrepareClaudeSkillRuntimeInput) -> Result<PrepareSkillRuntimeOutput, AcpxError>;
pub async fn prepare_codex_skill_runtime(input: PrepareCodexSkillRuntimeInput) -> Result<PrepareSkillRuntimeOutput, AcpxError>;
pub async fn prepare_gemini_skill_runtime(input: PrepareGeminiSkillRuntimeInput) -> Result<PrepareSkillRuntimeOutput, AcpxError>;

pub fn resolve_selected_runtime_skills(all, desired) -> (Vec<...>, Vec<...>, Vec<String>);
```

### 三 agent 对位

| Agent | Skills home | 链接方式 | Bundle key |
|---|---|---|---|
| Claude | `<stateDir>/runtime-skills/claude/<key>/.claude/skills/` | materialize (copy) | sha256(alphabetical `claude` skills) |
| Codex | `<managedCodexHome>/skills/` | materialize + manifest | sha256(alphabetical `codex` skills) |
| Gemini | `$HOME/.gemini/skills/` | symlink (fallback to copy on EPERM) | sha256(alphabetical `gemini` skills) |

### 关键设计

- **`SkillRuntimeIdentity`** 共享类型：三个 agent 返回同一 shape，便于上层统一处理
- **`CODEX_HOME` env 注入**：`prepare_codex_skill_runtime` 是唯一改 env 的准备函数（mut input）；Claude/Gemini 不改 env
- **`prepare_claude_skill_runtime` 必返回 `prompt_instructions`**：Claude 专属，0 skill 时为空字符串
- **in-crate shim `resolve_selected_runtime_skills`**：R367 简化版的 skill 过滤（只接收预解析的 entries）；完整 config 解析留给上层 `paperclip-server`
- **失败 logging**：`on_log` 接收 LogStream::Stderr 行，**不抛错**——单 skill materialization 失败不阻断整个 runtime 准备

### 单元测试覆盖（5 个）

- `resolve_selected_runtime_skills_filters_by_key`
- `claude_runtime_materializes_skills_into_bundle_root`
- `claude_runtime_is_pure_with_no_selected_skills`
- `codex_runtime_seeds_home_and_writes_manifest`
- `gemini_runtime_uses_symlink_or_copy_fallback`

---

## 🔗 6. round367_skill_staging.rs 集成测试（9 个）

**managed home e2e**:
- `managed_home_round_trip_through_real_disk`
- `manifest_persists_across_concurrent_writes`

**skill materialize e2e**:
- `materialize_copies_skill_tree_with_skipped_symlinks`
- `skill_set_key_changes_when_label_changes`

**reconciliation e2e**:
- `reconcile_managed_skills_three_phases`

**Agent-specific runtime e2e**:
- `claude_runtime_end_to_end`
- `codex_runtime_seeds_home_reconciles_and_writes_manifest`
- `gemini_runtime_uses_symlink_or_copy_fallback`

**跨模块契约锁定**:
- `error_classification_dispatches_to_protocol_phase`

---

## 🔁 总累计基线

| 模块 | R362 | R363 | R364 | R365 | R366 | **R367** |
|---|---|---|---|---|---|---|
| constants | ✓ | | | | | |
| gemini_version | ✓ | | | | | |
| session_codec | ✓ | | | | | |
| hash | ✓ | | | | | |
| normalize | ✓ | | | | | |
| transcript | ✓ | | | | | |
| usage | ✓ | | | | | |
| settings | | ✓ | | | | |
| fs_ops (扩展) | | ✓ | | | | **R367 +6 helpers** |
| bin | | ✓ | | | | |
| error | | ✓ | | | | **+2 variants** |
| agent_command | | | ✓ | | | |
| startup_metrics | | | ✓ | | | |
| prepared_runtime | | | ✓ | | | |
| acp_runtime | | | | ✓ | | |
| error_classification | | | | | ✓ | |
| child_stderr | | | | | ✓ | |
| startup_timing | | | | | ✓ | |
| **managed_home** | | | | | | **NEW** |
| **skill_materialize** | | | | | | **NEW** |
| **reconcile_skills** | | | | | | **NEW** |
| **skill_runtime** | | | | | | **NEW** |
| **pc-acpx 测试总数** | 47 | 66 | 90 | 105 | 155 | **202** |
| **总累计** | 975 | 994 | 1018 | 1042 | 1171 | **1218** |

---

## ✅ 完成度更新

| 模块 | R367 完成度 |
|---|---|
| **acpx-engine 子模块** | **97%** (+5%, R367) |
| **后端核心** (pc-heartbeat + pc-repos + pc-core) | 96% |
| **完整后端** (含 adapters + plugins) | ~78% |
| **最大剩余缺口** | 真实 `SubprocessAcpRuntime` (R368+) |

---

## 🎯 R368+ 候选

1. **R368-369**：真实 `SubprocessAcpRuntime` 实现 — spawn acpx 子进程并 wire stdin/stdout/stderr,~3-4 轮
2. **R370+**：Budgets 完整迁移（B2）— 计费/限流模块,~3-4 轮

