# R495 — `pc-cli install` 从 stub 变成真做事（创建 symlink）

> 配套: `proposal.md` V2 + `design.md`。
> 目标: `paperclipai install` 真在用户 prefix 创建一个指向当前 binary 的 symlink, 默认拒绝覆盖, `--force` 允许覆盖。

## 改动

### 1. `apps/pc-cli/src/main.rs`

**`Install` 变体扩展**：
- `--prefix <path>` (Option<String>): 覆盖默认 prefix（默认 `$HOME/.local/bin`）
- `--force` (bool): 允许覆盖已有 symlink

**新增 3 个 `pub`/private helper**：

| Helper | 类型 | 作用 |
|---|---|---|
| `default_install_prefix() -> PathBuf` | private | 读 `HOME` env, 返回 `$HOME/.local/bin`（无 unsafe）|
| `plan_install(current_exe, prefix, canary) -> InstallOutcome` | `pub` | 纯函数：算 source/target path（canary 模式用 `paperclipai-canary`）|
| `install_command(canary, prefix, force) -> Result<()>` | private | 调度：检查现有 → mkdir → symlink/copy → PATH 提示 |

**`InstallOutcome` struct** (pub)：`{ source: PathBuf, target: PathBuf }`，纯数据。

**真实做事路径**：
- `std::env::current_exe()` 找当前 binary
- `std::fs::create_dir_all(prefix)` 创 prefix
- `std::os::unix::fs::symlink(source, target)` 真创建 symlink
- 非 Unix fallback 到 `std::fs::copy`（Windows 支持）
- 检查 `PATH` 是否含 prefix，没有则提示用户加 `export PATH=...`

**安全设计**：
- 不使用 `unsafe` 块（`forbid(unsafe_code)` workspace lint）
- 不自动检测 root（避免 `geteuid` → unsafe）；root 用户显式 `--prefix /usr/local/bin`
- 默认拒绝覆盖已有 symlink（`anyhow::bail!`），必须 `--force`
- 错误信息字面化，CI 可 grep

### 2. `apps/pc-cli/src/main.rs` 测试

**6 个 R495 新测试**：

| 测试 | 验证 |
|---|---|
| `r495_plan_install_stable_target_name` | canary=false → target 是 `prefix/paperclipai` |
| `r495_plan_install_canary_target_name` | canary=true → target 是 `prefix/paperclipai-canary` |
| `r495_default_install_prefix_uses_home` | 默认 prefix 以 `.local/bin` 结尾 |
| `r495_install_refuses_to_overwrite_without_force` | 目标已存在 → 错误含 "refusing to overwrite" |
| `r495_install_creates_symlink_in_fresh_prefix` | 干净 prefix → 创建 symlink, `read_link` 解析回 source |
| `r495_install_force_overwrites_existing_symlink` | `--force` → 旧 sentinel 文件被替换 |

**1 个 test-only helper**：`install_command_with_paths` 暴露 refactor 后的 install 逻辑（去掉 `current_exe` 依赖），让单测可驱动任意 prefix/source。

## 设计要点
- **高内聚**：`plan_install` 是纯函数（path math），不碰 fs。
- **低耦合**：`install_command` 只调 `plan_install` + std::fs + std::os::unix::fs；不拉任何 service crate 依赖。
- **Node 1:1 对齐**：`install-store.ts` 同样使用 `$HOME/.local/bin`（XDG-style）+ root override；`canary` channel 命名约定 `*-canary` 与 upstream 一致。
- **测试不依赖 mock**：直接操作真实 `std::env::temp_dir()`，每次用 `Uuid::new_v4()` 隔离，cleanup 兜底。
- **修复 1 个真实 bug**：`remove_dir_all(&tmp)` 必须在 `read_link` / `read` 之后——初版我把 cleanup 放在前面，导致 symlink test fail。已修。

## 验证
```
cargo test -p pc-cli --bin paperclipai
  test result: ok. 25 passed (19 旧 + 6 新 R495)
cargo check --workspace          0 errors
cargo fmt -p pc-cli --check      no diff
```

## 整体进度（R495 末）

| 维度 | R494 末 | **R495 末** | Δ |
|---|---|---|---|
| **CLI 真实做事** | **33% (6/18)** | **39% (7/18)** | **+1 (install)** |
| pc-cli 单测 | 19 | **25** | +6 |
| 整体单测 | ≈ 1749 | ≈ **1755** | +6 |
| V3-V15 硬目标 | 几乎未动 | 几乎未动 | — |

## V2 CLI 真实做事清单（截至 R495）

✅ 真实 (13/18): `doctor` / `db-backup` / `configure` / `allowed-hostname` / `pipelines` / `routines` / `client*` / `heartbeat run` / `auth bootstrap-ceo` / `worktree list+current` / `env-lab` / `service install-hint` / `onboard --non-interactive` / **`install`** (R495 新)

🔶 Stub (5/18): `uninstall` / `update` / `env` / `run` / `onboard` (交互模式保留为 print)

## 经验教训
- **测试 cleanup 顺序**：对真实文件系统的测试，cleanup 必须在 read 之后——否则 read 失败时会留下 garbage 目录。
- **`forbid(unsafe_code)` 限制**：不能用 `libc::geteuid` 检测 root，改用"显式 `--prefix`"更安全（auditable）。
- **跨平台 fallback**：symlink 在 Unix 是 POSIX 原语；非 Unix fallback 到 copy 是正确选择（Windows 需 dev mode / admin 才能 symlink）。

## 下一步候选
1. **R496 — V2 CLI 深化 `uninstall` / `update` / `run`** (剩 3-4 stub) — V2 39% → 60%
2. **R496 — R492 helper 真实接入 `pc-decisions::DecisionService.create`** — 验证低耦合
3. **R496 — 切 V5 Auth 完整化**（refresh rotation / CSRF）— 解锁 V11/V12
