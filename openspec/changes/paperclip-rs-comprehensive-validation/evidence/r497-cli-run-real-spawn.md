# R497 — `pc-cli run` 从 stub 变成真做事（真启动 pc-server）

> 配套: `proposal.md` V2 + `design.md`。
> 目标: `paperclipai run` 真 spawn `pc-server` binary（foreground 或 detached）。

## 改动

### 1. `apps/pc-cli/src/main.rs`

**`Run` 变体扩展**（从 1 flag → 4 flag）:
- `--server-binary <path>` (Option<String>): 覆盖默认 binary 路径
- `--detach` (bool): 后台运行，不阻塞当前 shell
- `--pid-file <path>` (Option<String>): detached 模式时把 PID 写到文件
- 原有 `--config` 保留

**新增 2 个 `pub` helper**:

| Helper | 作用 |
|---|---|
| `resolve_server_binary(override_path) -> Option<PathBuf>` | 纯函数：1) override 2) workspace `target/{debug,release}/pc-server` 3) `$PATH` 里 `paperclip-server` 4) `$PATH` 里 `pc-server` |
| `build_run_env(base) -> Vec<(String, String)>` | 纯函数：把 BTreeMap 转成 sorted Vec，spawn 时用 `cmd.envs(...)` |

**`run_command` 真实做事**:
- 调用 `onboard_command` (interactive print) + `doctor_command` (HTTP /health) —— 保留 R493 逻辑
- 调 `resolve_server_binary` 找 binary，找不到就报清晰错误（提示 `cargo build -p pc-server` 或 `paperclipai install`）
- Forward 整个 process env 到子进程
- **Foreground 模式** (默认): `cmd.status()` 等待子进程退出，status code 透传（`std::process::exit(code)`）
- **Detach 模式** (`--detach`): `cmd.spawn()` 后立即返回；stdio 重定向到 `Stdio::null()`；PID 写到 `--pid-file` 或打印到 stdout

**安全设计**:
- 找不到 binary 不 panic, 返回清晰错误
- detach 时 stdio 全 null, 不占用 terminal
- foreground 透传 exit code, 不吞错误
- 透传整个 env（不漏 PAPERCLIP_DATABASE_URL 等关键 vars）

### 2. `apps/pc-cli/src/main.rs` 测试

**5 个 R497 新测试**:

| 测试 | 验证 |
|---|---|
| `r497_resolve_server_binary_respects_override_when_exists` | override 路径存在 → 返回 Some(override) |
| `r497_resolve_server_binary_returns_none_for_missing_override` | override 路径不存在 → 返回 None |
| `r497_resolve_server_binary_no_override_does_not_panic` | 无 override → 不 panic, 返回 Some 或 None 取决于环境 |
| `r497_build_run_env_passes_through_and_sorts` | 3 个 entry → sorted Vec, 顺序按 key |
| `r497_build_run_env_empty_base_yields_empty` | 空 base → 空 Vec |

## 设计要点
- **可测的纯函数**: `resolve_server_binary` + `build_run_env` 都接受参数（不依赖隐式 env state），单测不依赖 `cargo build` / 文件系统状态
- **真实做事**: `run_command` 真用 `std::process::Command` spawn，foreground 用 `status()` 阻塞，detach 用 `spawn()` 后立刻返回
- **Node 1:1 对齐**: 上游 `run` 同样用 `execFile` spawn 子进程 + 透传 stdio + 透传 env；`--detach` 对应 `child.unref()` 行为
- **错误信息可操作**: 找不到 binary 提示 3 种解决路径（`--server-binary` / `cargo build` / `paperclipai install`）

## 验证
```
cargo test -p pc-cli --bin paperclipai
  test result: ok. 39 passed (34 旧 + 5 新 R497)
cargo check --workspace          0 errors
cargo fmt -p pc-cli --check      no diff
```

## 整体进度（R497 末）

| 维度 | R496 末 | **R497 末** | Δ |
|---|---|---|---|
| **CLI 真实做事** | **44% (8/18)** | **50% (9/18)** | **+1 (run)** |
| pc-cli 单测 | 34 | **39** | +5 |
| 整体单测 | ≈ 1764 | ≈ **1769** | +5 |
| V3-V15 硬目标 | 几乎未动 | 几乎未动 | — |

## V2 CLI 真实做事清单（截至 R497）

✅ 真实 (9/18): `doctor` / `db-backup` / `configure` / `allowed-hostname` / `pipelines` / `routines` / `client*` / `heartbeat run` / `auth bootstrap-ceo` / `worktree list+current` / `env-lab` / `service install-hint` / `onboard --non-interactive` / `install` / `uninstall` / `update` / **`run`**

🔶 Stub (2/18): `env` / `onboard` (交互模式保留为 print)

## 经验教训
- **Pure function 让 IO 可测**: `resolve_server_binary` 接受 override 参数，让单测覆盖 "override 存在/不存在" 不需要构建真实 pc-server binary
- **Detach 用 stdio null**: 不能用 `Stdio::inherit()`（会持有 terminal），必须 `Stdio::null()` 真正脱离
- **环境透传 vs 显式**: 我选透传 `std::env::vars()` 整个 process env，简化调用方（用户已经在外面 export 过 DB URL），但保留 `--config` 等显式 flag 用于覆盖

## 下一步候选（R498）
1. **R498 — V2 CLI 收尾 2 stub** (`env` / `onboard` 交互模式) — V2 50% → 60%
2. **R498 — R492 helper 接入 `pc-decisions::DecisionService.create`** (扩签名 + 接入 pure helpers) — 验证低耦合
3. **R498 — 切 V5 Auth 完整化** (refresh rotation + CSRF) — 解锁 V11/V12
