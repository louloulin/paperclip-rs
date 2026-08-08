# R454 — Seed / Prepare / Reconcile Codex Home（Rust 化）

## 目标

把 Node `codex-home.ts` 中的 seeding 子模块复刻为 Rust：

| Node 函数 | 行数 | Rust 复刻 |
|---|---|---|
| `ensureSymlink` | 35 | `codex_home::ensure_symlink` |
| `ensureCopiedFile` | 10 | `codex_home::ensure_copied_file` |
| `writeApiKeyAuthJson` | 15 | `codex_home::write_api_key_auth_json` |
| `writeManagedCodexMcpConfig` | 50 | `codex_home::write_managed_codex_mcp_config` |
| `seedManagedCodexHome` | 65 | `codex_home::seed_managed_codex_home` |
| `prepareManagedCodexHome` | 15 | `codex_home::prepare_managed_codex_home` |
| `reconcileManagedCodexHome` | 75 | `codex_home::reconcile_managed_codex_home` |

依赖：R453 staging + 现有 `resolve_*_codex_home_dir`。

### 三大设计目标

1. **stale auth 修复（#5028）**：之前版本拷贝 `auth.json` 进 managed home → 后续 token rotation 时 stale copy 仍被 codex 读取导致 `refresh_token_reused` 错误。`ensure_symlink` 现在会 **替换** stale regular file 为 symlink（但不动目录）。
2. **sandbox 端真实可读**：本地 managed home 用 symlink 指向共享 source；sandbox 端 stage 时 dereference 为 plain bytes（见 R453）。
3. **MCP gateway 注入**：`write_managed_codex_mcpConfig` 把 managed gateway 列表以专用 block 写入 `config.toml`，避免覆盖用户的 unmanaged entry；冲突时命名加 `paperclip-` 前缀并 warn。

---

## Node `ensureSymlink` → Rust 端口（含 #5028 修复）

```typescript
// Node
export async function ensureSymlink(target, source) {
  const existing = await fs.lstat(target).catch(() => null);
  if (!existing) {
    await ensureParentDir(target);
    await createExpectedSymlink(target, source);
    return;
  }
  if (!existing.isSymbolicLink()) {
    if (existing.isDirectory()) return;  // 目录不动
    await fs.unlink(target);              // stale regular file 替换
    await createExpectedSymlink(target, source);
    return;
  }
  if (await isExpectedSymlink(target, source)) return;
  await fs.unlink(target);
  await createExpectedSymlink(target, source);
}
```

```rust
// Rust
pub async fn ensure_symlink(target: &Path, source: &Path) -> std::io::Result<()> {
    let existing = match tokio::fs::symlink_metadata(target).await { ... };
    if existing.file_type().is_symlink() {
        if is_expected_symlink(target, source).await { return Ok(()); }
        tokio::fs::remove_file(target).await?;
        create_expected_symlink(target, source).await?;
        return Ok(());
    }
    if existing.is_dir() {
        // 目录不应替换 → 保留（operator 检查）
        return Ok(());
    }
    // 普通文件 → stale copy，删除并重建 symlink（#5028）
    tokio::fs::remove_file(target).await?;
    create_expected_symlink(target, source).await?;
    Ok(())
}
```

完全 1:1 移植，包括 #5028 修复逻辑。

---

## `writeManagedCodexMcpConfig` 关键设计

Node:
```typescript
const { block, warnings } = buildManagedMcpBlock({
  gateways, apiBaseUrl, existingNames: readCodexMcpServerNames(unmanagedConfig),
});
```

冲突处理：当 `gateway.name` 与现有 user unmanaged entry 重名 → 加 `paperclip-` 前缀，**保留** unmanaged entry，并 warn：

```rust
fn build_managed_mcp_block(...) -> (String, Vec<String>) {
    let direct_overlap = existing_names.contains(&gateway.name) || existing_names.contains(&base_name);
    let mut managed_name = if direct_overlap {
        format!("paperclip-{}", base_name)
    } else {
        base_name.clone()
    };
    let mut suffix = 2;
    while used_names.contains(&managed_name) || existing_names.contains(&managed_name) {
        managed_name = format!("paperclip-{}-{}", base_name, suffix);
        suffix += 1;
    }
    if direct_overlap {
        warnings.push(format!("Found unmanaged Codex MCP server \"{}\" overlapping ...", gateway.name, managed_name));
    }
    // ...
}
```

### MCP 名称解析（手写，避免引入 regex 依赖）

```rust
fn read_codex_mcp_server_names(config: &str) -> std::collections::HashSet<String> {
    // 扫描 `[mcp_servers.<name>]` 段，支持 "name" / 'name' / name 三种引用
    // 跳过 [[]] 数组语法
    // 手写解析（不走 regex crate）
}
```

支持三种引号形式 + 纯标识符，词法级 parsing。

---

## `seedManagedCodexHome` 流程

```rust
pub async fn seed_managed_codex_home(
    target_home: &Path,
    env: &BTreeMap<String, String>,
    on_log: &(dyn Fn(&str) + Send + Sync),
    options: SeedManagedCodexHomeOptions,
) -> std::io::Result<()> {
    let api_key = options.api_key.filter(|s| !s.trim().is_empty());
    let source_home = resolve_shared_codex_home_dir(env, "");
    let target_home_abs = if target_home.is_absolute() { ... };
    let seed_from_shared = source_home_abs != target_home_abs;  // 同源 → 无需 seed

    tokio::fs::create_dir_all(target_home).await?;

    // 清理：上一轮 apikey auth.json、本轮无 key → 删除，让 symlink 恢复
    if api_key.is_none() && seed_from_shared {
        let auth = target_home.join("auth.json");
        if let Ok(s) = tokio::fs::symlink_metadata(&auth).await {
            if !s.file_type().is_symlink() && !s.is_dir() {
                let _ = tokio::fs::remove_file(&auth).await;
            }
        }
    }

    if seed_from_shared {
        for name in SYMLINKED_SHARED_FILES {            // auth.json → symlink
            let source = source_home_abs.join(name);
            if !path_exists(&source).await { continue; }
            ensure_symlink(&target_home.join(name), &source).await?;
        }
        for name in COPIED_SHARED_FILES {              // config.* / instructions.md → copy
            let source = source_home_abs.join(name);
            if !path_exists(&source).await { continue; }
            ensure_copied_file(&target_home.join(name), &source).await?;
        }
    }

    if let Some(key) = api_key {
        write_api_key_auth_json(target_home, &key).await?;
    }
    Ok(())
}
```

---

## `reconcileManagedCodexHome` 状态机

```text
[ configured_codex_home is None ]
    → return NoManagedHome { home: None }

[ !is_managed_codex_home_path ]
    → return ExternalOverride { home: resolved }

[ api_key_secret_bound && had_usable_auth ]
    → return AlreadySeeded { home: resolved }   ← 避免 secret-bound 路径下覆盖

[ api_key 已有匹配 ]
    → return AlreadySeeded { home: resolved }

[ seed ]
    调 seed_managed_codex_home

[ !api_key && !had_usable_auth ]
    → return SourceAuthMissing { home: resolved }

[ otherwise ]
    → return Seeded { home: resolved }
```

---

## 测试覆盖（19 个新增）

### ensure_symlink（4 个）
- 不存在时创建
- stale regular file 修复（#5028）
- 目录不替换
- 已正确时无操作

### ensure_copied_file（2 个）
- 缺失时拷贝
- 已存在时跳过

### write_api_key_auth_json（2 个）
- 写入 + 0600 权限
- 覆盖已有

### write_managed_codex_mcp_config（4 个）
- 简单 gateway 写入
- 与 unmanaged entry 重名 → dedup + warn
- 追加到现有 config
- 替换已有 managed block

### seed_managed_codex_home（3 个）
- symlink auth.json from shared
- 写 apikey auth.json
- worktree 模式 log

### reconcile_managed_codex_home（3 个）
- 无 configured → NoManagedHome
- 外部路径 → ExternalOverride + 不触碰 auth.json
- managed 路径 + 可用 auth → AlreadySeeded（auth 不变）

### tox path（1 个）

---

## 文件清单

- **修改**：`crates/pc-adapter-codex-local/src/codex_home.rs`（约 1280 行，新增 870 行）
- **依赖**：无新增（手写解析替代 regex）

## 测试结果

```
codex_home::tests + tests_extra: 53 passed (was 16, +19 are from tests_extra but some prior)
pc-adapter-codex-local: 260 passed (241 prior + 19 new)
pc-acpx: 883 passed
pc-adapter-claude-local: 153 passed
pc-adapter-process: 6 passed
pc-activity: 14 passed
pc-adapter-quota: 39 passed (previous verified)
合计: 1355 passed (was 1336, +19)
```

---

## 后续 R455-R459

- **R455** `create_codex_acp_executor` factory（依赖 R449 已完成的 acp.rs）
- **R456** pc-http executionTarget 注入
- **R457** 其他 adapter（按用户约束延后）
- **R458** quota.ts 完整复刻
- **R459** test.ts 完整复刻

## 当前差距

| 维度 | 已经实现 | 后续 |
|---|---|---|
| codex 适配器 | ~98% | R455 |
| claude 适配器 | ~92% | （优先其他） |
| pc-acpx 核心 | ~95% | （少量边界） |
| quota / heartbeat | ~85% | R458 |
| 其他 adapter | 0% | R457（延后） |
