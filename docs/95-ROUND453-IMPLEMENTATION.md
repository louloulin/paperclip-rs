# R453 — Codex Home Staging (Rust 化)

## 目标

把 Node `codex-home.ts` 中的 staging 子模块复刻为 Rust：

| Node 函数 | 行数 | Rust 复刻 |
|---|---|---|
| `stageCodexHomeForSync` | 35 | `codex_home_staging::stage_codex_home_for_sync` |
| `stageCodexHomeEntry` (private) | 25 | `stage_codex_home_entry` |
| `stageDirectorySecure` (private) | 50 | `stage_directory_secure` |
| `stageContainedSubtree` (private) | 60 | `stage_contained_subtree` |
| `isResolvedPathInside` (private) | 10 | `is_resolved_path_inside` |

依赖：R452（auth merge 已完成）。

### 三大设计目标

1. **白名单精确上传**：只 stage `auth.json` / `config.toml` / `config.json` / `instructions.md` / `skills/`，避免 `*.sqlite` / `plugins/` / `sessions/` 等运行时状态。
2. **符号链接 dereference 为字节**：auth.json 是指向 shared source 的 symlink（保持单次使用 refresh token 鲜活），staging 时 dereference 到 plain file。
3. **fail-closed 严格安全**：每个 file 0600 / 每个 dir 0700；任何意外 I/O 错 → 清理 partial dir + 重抛。

---

## Node `stageCodexHomeForSync` → Rust 端口

### 关键映射

```js
// Node
const stagedHome = await fs.mkdtemp(path.join(os.tmpdir(), ...));
for (const entry of CODEX_SYNC_ALLOWLIST) {
  await stageCodexHomeEntry(effectiveCodexHome, stagedHome, entry);
}
return stagedHome;
```

```rust
// Rust
let staged_home = tokio::task::spawn_blocking(move || {
    let mut template = std::env::temp_dir();
    template.push(format!(".{}", prefix));
    let unique = format!(
        "{}.{}.{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        uuid::Uuid::new_v4().simple()
    );
    template.push(unique);
    std::fs::create_dir(&template)?;
    std::fs::set_permissions(&template, std::fs::Permissions::from_mode(0o700))?;
    Ok::<PathBuf, std::io::Error>(template)
}).await??;
for entry in CODEX_SYNC_ALLOWLIST {
    stage_codex_home_entry(effective, &staged_home, entry).await?;
}
```

### ELOOP 错误码处理

```rust
let resolved = match fs::canonicalize(&entry_source).await {
    Ok(p) => p,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
    Err(e) if e.raw_os_error() == Some(62) /* ELOOP on macOS, 40 on linux */ => continue,
    Err(e) => return Err(e),
};
```

ELOOP 错误码在 macOS 是 62，Linux 是 40。用 `raw_os_error()` 兼容两个平台。

### 递归 async fn → BoxFuture

```rust
fn stage_contained_subtree<'a>(
    source_dir: &'a Path,
    target_dir: &'a Path,
    containment_root: &'a Path,
    active_path: &'a mut Vec<PathBuf>,
) -> futures_core::future::BoxFuture<'a, std::io::Result<()>> {
    Box::pin(async move {
        // ... 递归调用 stage_contained_subtree(...).await
    })
}
```

Rust 编译器禁止无限大小 future，递归 async fn 必须 indirection 用 `BoxFuture`。

---

## 核心算法解析

### `stage_contained_subtree` 路径环检测

```rust
if active_path.contains(&resolved) {
    continue;
}
active_path.push(resolved.clone());
// 递归
stage_contained_subtree(&resolved, &entry_target, containment_root, active_path).await?;
active_path.pop();
```

`active_path` 记录当前正在打开的目录的真实路径。`back -> .` 风格的 symlink 会让 `resolved` 被循环指向 → 检测到后跳过。

### 顶层 vs 嵌套链接的逃逸检测

Node 注释明确：
> `sourceDir`'s *direct* children are the Paperclip-injected skill symlinks that intentionally point into a shared skill store *outside* `CODEX_HOME/skills/`, so each child is allowed to resolve anywhere

**顶层**（`skills/<entry>`）：允许任意 resolve（共享 skill store）
**嵌套**（`skills/<subdir>/<entry>`）：resolve 必须落在 `<subdir>` containment root 内

```rust
// stage_directory_secure: 顶层直接 accept
if entry_stat.is_dir() {
    // 进入 nested 模式：containment root = resolved (subdir)
    stage_contained_subtree(&resolved, &entry_target, &resolved, &mut vec![resolved.clone()]).await?;
} else if entry_stat.is_file() {
    // 顶层 file link：直接 dereference（共享 skill 内容）
    let bytes = fs::read(&resolved).await?;
    write_file_0600(&entry_target, &bytes).await?;
}

// stage_contained_subtree: 嵌套必须 contain
if !is_resolved_path_inside(&resolved, containment_root) {
    continue;
}
```

---

## 测试覆盖（18 个）

### stage_codex_home_for_sync 端到端（10 个）
- 返回 temp dir 路径
- run_id 注入到 dir name
- 仅白名单项（不含 sqlite / plugins / sessions / tmp）
- `auth.json` 是 plain file（不是 symlink）+ 内容等于 source bytes
- `skills/*` dereference 到 plain bytes
- `config.toml` / `config.json` / `instructions.md` 保留
- 普通文件 mode = 0600
- staging dir mode = 0700
- 缺失 `auth.json`（keyring 模式）→ 跳过
- dangling `auth.json` symlink → 跳过

### 安全性（3 个）
- 顶层 skill 文件 symlink（指向 shared store）允许
- 嵌套 skill symlink 逃出 containment root → 拒绝
- 自指 circular skill symlink (`back -> .`) → 不无限递归

### 辅助函数单测（4 个）
- `is_resolved_path_inside` 自身 / 子 / 祖先 / trailing-prefix collision

### 生命周期（1 个）
- 失败时清理 partial dir

---

## 与现有 `codex_home.rs` 的责任边界

| 关注点 | `codex_home.rs` | `codex_home_staging.rs` |
|---|---|---|
| **目的** | 路径解析 + 基础 home 判断 | 跨 runtime staging 准备 |
| **核心函数** | `resolve_shared/managed_codex_home_dir` / `is_managed_codex_home_path` / `codex_home_has_usable_auth` | `stage_codex_home_for_sync` |
| **副作用** | 只读 + 单 auth.json 探查 | 创建新 tmpdir + 权限设置 + 符号链接 dereference |
| **调用场景** | execute path 解析 CODEX_HOME | sandbox-managed-runtime 上传前 stage |

两者都共享 `CODEX_SYNC_ALLOWLIST`（同一字符串数组）。

---

## 文件清单

- **新建**：`crates/pc-adapter-codex-local/src/codex_home_staging.rs`（约 580 行）
- **修改**：`crates/pc-adapter-codex-local/src/lib.rs`（新增 `pub mod codex_home_staging;`）

## 测试结果

```
codex_home_staging::tests: 18 passed, 0 failed
pc-adapter-codex-local: 241 passed (223 prior + 18 new)
pc-acpx: 883 passed
pc-adapter-claude-local: 153 passed
pc-adapter-process: 6 passed
pc-activity: 14 passed
pc-adapter-quota: 39 passed
合计: 1336 passed, 0 failed (was 1318, +18)
```

---

## 后续 R454-R459

- **R454** `prepare_managed_codex_home` + `seed_managed_codex_home` + `reconcile_managed_codex_home`（依赖 R453 + `ensure_symlink` + `write_managed_codex_mcp_config`）
- **R455** `create_codex_acp_executor` factory
- **R456** pc-http executionTarget 注入
- **R457** 其他 adapter（按用户约束延后）
- **R458** quota.ts 完整复刻
- **R459** test.ts 完整复刻

## 当前差距

| 维度 | 已经实现 | 后续 |
|---|---|---|
| codex 适配器 | ~96% | R454-R456 |
| claude 适配器 | ~92% | （优先其他） |
| pc-acpx 核心 | ~95% | （少量边界） |
| quota / heartbeat | ~85% | R458 |
| 其他 adapter | 0% | R457（延后） |
