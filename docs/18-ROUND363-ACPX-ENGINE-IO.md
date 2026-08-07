# Round 363 — Acpx-engine I/O 入口 (B3.1 第二阶段)

> 适用版本：`paperclip-rs` 截至 R363（R362 = 975 → R363 = **994**，+19 pc-acpx 测试）
> 参考实现：`paperclip` Node（`packages/adapter-utils/src/acpx-engine/execute.ts`）
> 测试基线：`cargo test -p pc-acpx` 66/66 绿；`pc-heartbeat` 928/928 全量无回归；`cargo build --workspace --bins` 通过；`cargo fmt --all -- --check` 通过

---

## 🎯 R363 目标

继续 **acpx-engine** Rust 化迁移（B3.1 第二阶段），从最大单一缺口 3500+ 行 Node 中
抽出**I/O 入口**层：

1. 引擎设置解析：`resolve_engine_settings` (新增 `settings.rs`)
2. 异步文件系统基础：`path_exists` / `path_is_file` / `ensure_parent_dir` / `write_file_atomically` (新增 `fs_ops.rs`)
3. 向上查找 binary：`find_ancestor_bin` (新增 `bin.rs`)
4. 错误类型统一：`AcpxError` (新增 `error.rs`)
5. **新增集成测试** `round363_io_layer` 验证完整链路

**为什么 I/O 层是这个阶段的关键**：纯函数层（R362）只定义了"做什么"，I/O 层定义
"在哪里做"。后续的 execute.ts 主流程（buildRuntime / warm handles / staging）直接
依赖这三个工具。

---

## 🏗️ 新增模块

```
crates/pc-acpx/src/
├── settings.rs           # resolve_engine_settings (纯函数 + 输入扩展)
├── fs_ops.rs             # path_exists / path_is_file / ensure_parent_dir / write_file_atomically (async I/O)
├── bin.rs                # find_ancestor_bin + Platform (async I/O)
└── error.rs              # AcpxError (io::Error 包装 + 路径 tag)
```

### 模块职责

| 模块 | 职责 | 依赖 |
|---|---|---|
| `settings.rs` | 解析 `AcpxEngineOptions` → `AcpxEngineSettings`（绝对路径 + adapter_type） | 无 |
| `fs_ops.rs` | 异步文件系统基础操作（原语级） | `tokio::fs` + `uuid` |
| `bin.rs` | 向上查找 `node_modules/.bin/<bin>` (支持 Windows + POSIX) | `tokio::fs` |
| `error.rs` | 统一错误类型（带 path tag） | `thiserror` |

---

## 🔧 R363 实现的 5 个函数

### `settings.rs` ✅ (1 个)

| Rust | Node |
|---|---|
| `resolve_engine_settings(options, fallback) -> AcpxEngineSettings` | `resolveEngineSettings` |
| `AcpxEngineOptions { adapter_type, module_dir, package_root_dir }` | `AcpxEngineExecutorOptions` 子集 |
| `AcpxEngineSettings { adapter_type, module_dir, package_root_dir }` | `AcpxEngineSettings` |

**关键设计**：
- 所有路径输出**绝对路径**（消除歧义）
- 相对输入路径自动解析为 `cwd + relative`（与 Node `path.resolve` 行为一致）
- `adapter_type` 空字符串归一为 `"acp_engine"`（不抛错）

### `fs_ops.rs` ✅ (4 个)

| Rust | Node |
|---|---|
| `path_exists(path) -> bool` | `pathExists` |
| `path_is_file(path) -> bool` | (新增) |
| `ensure_parent_dir(path) -> Result<()>` | `ensureParentDir` |
| `write_file_atomically(input) -> Result<()>` | `writeFileAtomically` |
| `WriteFileAtomicallyInput { target, contents, mode }` | 内嵌 argument |

**关键设计**：
- `path_exists` 不返回 error，错误折叠为 `false`（存在性问题的语义）
- `path_is_file` 区分文件 vs 目录 vs symlink
- `write_file_atomically` 完整流程：
  1. `ensure_parent_dir` 创建中间目录
  2. 打开 `<target>.tmp-<pid>-<uuid>` 临时文件（`create_new`，避免冲突）
  3. 写内容 + 关闭
  4. `rename` 原子替换
  5. best-effort `chmod` (Unix only)
- 失败时 cleanup 临时文件

### `bin.rs` ✅ (2 个)

| Rust | Node |
|---|---|
| `find_ancestor_bin(start_dir, bin_name, platform) -> Option<PathBuf>` | `findAncestorBin` |
| `Platform::Posix / Windows` | `process.platform === "win32"` 分支 |
| `Platform::detect()` | (新增 - 基于 `cfg!(target_os)`) |
| `Platform::candidate_paths(bin_dir, bin_name) -> Vec<PathBuf>` | (新增 - 平台分支逻辑) |

**关键设计**：
- `Platform` 抽象：测试可固定平台（避免 host 依赖）
- Windows 优先 `.cmd` shim
- 树根检测：`parent == current` 时停止
- 相对路径自动解析为 `cwd + relative`（与 Node `path.resolve` 一致）

### `error.rs` ✅ (1 个)

| Rust | Node |
|---|---|
| `AcpxError::Io { path, error }` | `throw new Error(...)` |
| `AcpxError::NoParent(path)` | (新增 - 路径无 parent 边界情况) |
| `AcpxError::io(path, err) -> Self` | (新增 - 适配器) |

**关键设计**：
- `thiserror::Error` derive
- `path` 字段独立 → diagnostic 友好
- 单个 adapter 方法 `io()` 简化错误构造

---

## 📊 R363 测试覆盖

| 测试类型 | 数量 | 位置 |
|---|---|---|
| 单元测试 | **15** (R362 是 39, R363 新增 15) | `src/fs_ops.rs::tests` (6) + `src/bin.rs::tests` (5) + `src/settings.rs::tests` (4) |
| R362 集成测试 | 8 | `tests/round362_milestone.rs` |
| **R363 集成测试** | **4** | `tests/round363_io_layer.rs` |
| **pc-acpx 合计** | **66** | |
| pc-heartbeat 全量回归 | **928** | 无变化 |

### 关键测试覆盖

- `find_ancestor_bin`：
  - 找不到 → `None`
  - 在 start_dir 找到 → 返回绝对路径
  - 在祖先找到 → 返回绝对路径
  - Windows 优先 `.cmd` shim
  - `Platform::candidate_paths` 单元测试
- `fs_ops`：
  - `path_exists` 真假分支
  - `ensure_parent_dir` 多级创建 + bare filename
  - `write_file_atomically`：创建 + 覆盖 + 失败时清理（temp file 数量为 0）
  - 文件 mode 设置（Unix 0o600 验证）
- `settings`：
  - 默认 adapter_type
  - 空白字符串归一
  - 相对路径自动解析
  - caller 路径覆盖
- **round363 集成测试**：
  - `settings_to_stage_file_pipeline_writes_to_resolved_paths`
  - `default_adapter_type_flows_through_pipeline`
  - `find_ancestor_bin_then_write_atomically_round_trip`
  - `write_file_atomically_overwrites_with_changing_contents`

---

## 🧪 验证基线

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# 1. pc-acpx 全量（66/66 绿）
env -u SHELL rtk proxy cargo test -p pc-acpx

# 2. pc-heartbeat 无回归（928/928 绿）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --tests -- --test-threads=1

# 3. 格式
env -u SHELL rtk proxy cargo fmt --all
env -u SHELL rtk proxy cargo fmt --all -- --check

# 4. 编译
env -u SHELL rtk proxy cargo build --workspace --bins --message-format=short
```

---

## 📦 关键设计决策

### 1. `Platform` 抽象与 `detect()` 分离

```rust
pub enum Platform {
    Posix,
    Windows,
}

impl Platform {
    pub fn detect() -> Self { ... }      // 运行时探测
    pub fn candidate_paths(...) -> Vec<PathBuf> { ... }  // 平台分支
}
```

→ 测试可显式传 `Platform::Windows` 验证行为，不依赖 `cfg!(target_os)`。

### 2. 错误类型细化（path-tagged）

```rust
#[derive(Debug, Error)]
pub enum AcpxError {
    #[error("io error on `{path}`: {error}")]
    Io { path: PathBuf, #[source] error: std::io::Error },
    #[error("path `{0}` has no parent directory")]
    NoParent(PathBuf),
}
```

→ 调用方可直接 `format!("{err}")` 拿到带路径的 diagnostics，`#[source]` 保留原始 io::Error 链。

### 3. `write_file_atomically` 流程拆解

```rust
async fn write_file_atomically(input: WriteFileAtomicallyInput) -> Result<()> {
    ensure_parent_dir(&input.target).await?;
    let temp_path = compose_temp_path(&input.target);
    let write_result = write_temp_and_rename(&input, &temp_path).await;
    if let Err(error) = write_result {
        let _ = tokio::fs::remove_file(&temp_path).await;  // best-effort cleanup
        return Err(error);
    }
    // best-effort chmod (Unix only)
    Ok(())
}
```

→ 拆为 `ensure_parent_dir` + `write_temp_and_rename` 两个 private helper，提高可测性。

### 4. `write_file_atomically` 不在父级失败时创建空文件

```rust
let mut handle = tokio::fs::OpenOptions::new()
    .write(true)
    .create_new(true)  // 关键：若文件存在则失败
    .open(temp_path)
    .await
```

→ `create_new(true)` 防止意外覆盖已存在的临时文件（避免潜在的 race condition）。

### 5. `find_ancestor_bin` 树根终止

```rust
let parent = current.parent()?.to_path_buf();
if parent == current {
    return None;  // 到达根目录
}
```

→ 防止 `parent()` 返回 `Some("/")` 时无限循环。

### 6. `compose_temp_path` 包含 PID + UUID

```rust
fn compose_temp_path(target: &Path) -> PathBuf {
    let pid = std::process::id();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    ...
}
```

→ 即使同一进程并发调用也不会冲突；PID + UUID 双重保证唯一性。

---

## 📋 后续 R364+ 计划

### R364 (下一轮) — `buildRuntime` 拆分启动

- `AcpxPreparedRuntime` 数据结构
- `resolveBashedInAgentCommand` (gemini 边界 + find_ancestor_bin 集成)
- `buildStartupStepMetrics` (基础启动指标)

### R365 — `acp.handshake` 协议调用

- `AcpRuntime` trait 抽象
- `OpenSession` / `SendTurn` / `CloseSession` 边界
- `getStatus` 状态读取

### R366 — 错误恢复 + `startup-timing.ts`

- `classifyError` / `describeErrorDiagnostics`
- `readChildStderrTail` / `routeChildStderr`
- `startup-timing.ts`（304 行）

### R367 — Sandbox staging seam

- `prepareAdapterExecutionTargetRuntime`
- `stageAcpRemoteRuntime`
- `startAdapterExecutionTargetPaperclipBridge`

---

## 📊 完成度更新

| 维度 | R360 | R362 | R363 |
|---|---|---|---|
| pc-acpx 测试 | 0 | 47 | **66** |
| pc-heartbeat 测试 | 928 | 928 | 928 |
| 总测试数 | 928 | 975 | **994** |
| acpx-engine 子模块 | ~0% | ~67%（纯函数） | ~75%（+I/O） |
| 后端核心 | ~96% | ~96% | ~96% |

---

## 📝 总结

**R363 推进 acpx-engine Rust 化迁移到 I/O 层**：

- **新增 4 个模块**：`settings.rs` + `fs_ops.rs` + `bin.rs` + `error.rs`
- **新增 19 个测试**（15 单元 + 4 集成），保持 0 失败
- **pc-heartbeat 928 测试完全无回归**
- **核心原语就绪**：`resolve_engine_settings` + `write_file_atomically` + `find_ancestor_bin` 已为后续 R364+ 铺设
- **完成度**：acpx-engine 子模块从 67% 推进到 ~75%（I/O 层基础就绪）

**下一步**：R364 启动 `buildRuntime` 拆分（最关键的纯算法层），继续 B3.1 第三阶段。
