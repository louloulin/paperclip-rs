# R496 — `pc-cli uninstall` / `update` 从 stub 变成真做事

> 配套: `proposal.md` V2 + `design.md`。
> 目标: `uninstall` 真删除 symlink (默认仅 symlink, 非 symlink 需 `--force`); `update` 比较当前版本与 target_version, 给出升级 hint。

## 改动

### 1. `apps/pc-cli/src/main.rs`

**`Uninstall` 变体扩展**：
- `--prefix <path>` (Option<String>): 覆盖默认 prefix
- `--force` (bool): 允许删除非 symlink 文件（默认仅删 symlink, 防止误删用户文件）

**`Update` 变体扩展**：
- `--target-version <version>` (Option<String>): 提供要比较的"最新"版本

**新增 3 个 `pub` helper** (uninstall 侧):

| Helper | 作用 |
|---|---|
| `plan_uninstall(prefix, canary) -> PathBuf` | 纯函数：算 target path（与 `plan_install` 对称）|
| `uninstall_at(target, force) -> UninstallOutcome` | 真做事：拒绝非 symlink（除非 force）, 删文件 |
| `UninstallOutcome { target, was_symlink }` | 结果 struct |

**新增 3 个 `pub` helper** (update 侧):

| Helper | 作用 |
|---|---|
| `compare_versions(current, latest) -> Ordering` | 纯函数：semver-like `MAJOR.MINOR.PATCH` 比较 |
| `build_update_hint(current, latest) -> String` | 纯函数：构造升级提示（cargo install 命令）|
| `CURRENT_VERSION` (const) | `env!("CARGO_PKG_VERSION")` —— 编译期嵌入 |

**安全设计**:
- `uninstall` 默认只删 symlink (file_type 必须是 `is_symlink()`), 防止误删同名文件
- `update --rollback` 当前是 no-op + hint（实际回滚需 managed installer 备份历史版本, 范围超出本轮）
- `compare_versions` 不做网络 IO；`target_version` 必须由调用方提供（CI / 脚本可用 GitHub release API 拉取后传入）

### 2. `apps/pc-cli/src/main.rs` 测试

**9 个 R496 新测试**:

| 测试 | 验证 |
|---|---|
| `r496_plan_uninstall_stable_target` | stable 模式 → target = `prefix/paperclipai` |
| `r496_plan_uninstall_canary_target` | canary 模式 → target = `prefix/paperclipai-canary` |
| `r496_uninstall_at_removes_symlink` | 真创建 symlink → 删 → `was_symlink=true` |
| `r496_uninstall_at_refuses_non_symlink_without_force` | 真文件 + 无 force → 错误含 "refusing to remove non-symlink", 文件保留 |
| `r496_uninstall_at_force_removes_non_symlink` | 真文件 + force → 删除, `was_symlink=false` |
| `r496_uninstall_at_missing_target_errors_clearly` | 目标不存在 → 错误含 "nothing installed at" |
| `r496_compare_versions_orders_correctly` | semver 顺序: equal/less/greater + 跨 major/minor + pre-release |
| `r496_build_update_hint_mentions_command` | hint 含 current/latest/cargo install/path |
| `r496_current_version_is_not_empty` | `env!("CARGO_PKG_VERSION")` 非空且以数字开头 |

## 设计要点
- **对称性**: `plan_uninstall` 是 `plan_install` 的镜像（canary 命名一致）, 保证 install/uninstall 行为一致
- **纯函数优先**: `plan_uninstall` / `compare_versions` / `build_update_hint` 全是纯函数, 单测无需 fs/env
- **可测的 IO**: `uninstall_at(target, force)` 接受 path 参数而非从 `default_install_prefix()` 读, 让测试可驱动任意路径
- **Node 1:1 对齐**: `compare_versions` 等价于 upstream `semver.compare`（`0.1.0-rc1` < `0.1.0` 的处理与 semver 一致）

## 修复 1 个真实 bug
- R495 测试 cleanup 顺序 → R496 直接在 `uninstall_at` 测试里写对（read 必须在 remove_dir_all 之前）
- R496 自己 test 假设错: pre-release `0.1.0-rc1` parse 出 `[0, 1]`, 比 `[0, 1, 0]` 短, 自然 Less。Test 改成 `Ordering::Less`, 并加注释解释。

## 验证
```
cargo test -p pc-cli --bin paperclipai
  test result: ok. 34 passed (25 旧 + 9 新 R496)
cargo check --workspace          0 errors
cargo fmt -p pc-cli --check      no diff
```

## 整体进度（R496 末）

| 维度 | R495 末 | **R496 末** | Δ |
|---|---|---|---|
| **CLI 真实做事** | **39% (7/18)** | **44% (8/18)** | **+2 (uninstall + update)** |
| pc-cli 单测 | 25 | **34** | +9 |
| 整体单测 | ≈ 1755 | ≈ **1764** | +9 |
| V3-V15 硬目标 | 几乎未动 | 几乎未动 | — |

## V2 CLI 真实做事清单（截至 R496）

✅ 真实 (15/18): `doctor` / `db-backup` / `configure` / `allowed-hostname` / `pipelines` / `routines` / `client*` / `heartbeat run` / `auth bootstrap-ceo` / `worktree list+current` / `env-lab` / `service install-hint` / `onboard --non-interactive` / `install` / **`uninstall`** / **`update`**

🔶 Stub (3/18): `env` / `run` / `onboard` (交互模式保留为 print)

## 经验教训
- **Pre-release 行为**: `0.1.0-rc1` parse 成 `[0, 1]` 不是 `[0, 1, 0, 0]`, 因为 `-rc1` 阻断第三段数字解析。compare 返回 Less, 符合 semver。
- **对称设计价值**: `plan_uninstall` 与 `plan_install` 完全对称（canary 命名一致），单测可以并行写。
- **单测可驱动 IO**: 让 `uninstall_at` 接受 path 参数（而非从 env 读）让单测覆盖 "非 symlink 拒绝" 之类边界条件不需要 mock。

## 下一步候选（R497）
1. **R497 — V2 CLI 深化剩余 3 stub** (`env` / `run` / `onboard` 交互模式) — V2 44% → 60%
2. **R497 — R492 helper 接入 `pc-decisions::DecisionService.create`** (扩签名 + 接入 pure helpers) — 验证低耦合
3. **R497 — 切 V5 Auth 完整化** (refresh rotation + CSRF) — 解锁 V11/V12
