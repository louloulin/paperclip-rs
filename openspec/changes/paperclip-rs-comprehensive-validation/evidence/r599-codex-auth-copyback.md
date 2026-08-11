# R599 — Codex SSH auth copy-back 真实 E2E 验证

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

对齐 Node `codex-auth-copyback.ts` + Node `execute.ts:728-770` 的 outbound
sandbox→host auth copy-back 流程。Rust 端实现：

- `crates/pc-adapter-codex-local/src/auth_copyback.rs`
  - `CopyBackCodexAuthInput` / `CopyBackCodexAuthOutcome` 数据契约
  - `CopyBackCodexAuthDecider` trait
  - `DefaultDecider`（保守 KEEP）与 `CodexAuthMergeDecider`（生产决策器）
  - `copy_back_codex_auth` 完整流程（read sandbox → mkdir → lock → stage
    `0600` temp → decide → rename 或丢弃 → finally 清理 temp）
- `crates/pc-adapter-codex-local/src/codex_remote_home.rs::read_remote_codex_auth`
  - 通过真实 SSH `cat <remoteHome>/auth.json` 读 sandbox bytes
  - "No such file" 错误归类为 `ErrorKind::NotFound`
- `crates/pc-adapter-codex-local/src/lib.rs`
  - 在 bridge start 之前捕获 `remote_codex_auth_target/_home/_path`
  - 在 bridge stop 之前的 teardown 阶段真正调用 `copy_back_codex_auth`
    并把决定与结果作为 stdout/stderr 事件回流

## 2. 新增的真实 SSH E2E 测试

文件：`crates/pc-adapter-codex-local/tests/round599_codex_remote_auth_copyback.rs`

| 测试 | 场景 | 期望 |
|---|---|---|
| `codex_remote_auth_copy_back_over_ssh_installs_newer_credential` | sandbox 更新 + 同 account + 同 fs 上 rename | `Copied` + host auth.json 被替换 + 无遗留 `.tmp` |
| `codex_remote_auth_copy_back_over_ssh_keeps_host_when_sandbox_older` | sandbox 更旧 + 同 account | `KeptHost` + host auth.json 保持 |
| `codex_remote_auth_copy_back_over_ssh_keeps_host_when_sandbox_absent` | 远端 `auth.json` 被删除 | `read_remote_codex_auth` 返回 `NotFound` + `copy_back_codex_auth` 走良性 no-op |
| `codex_remote_auth_copy_back_over_ssh_keeps_host_on_account_mismatch` | sandbox 与 host account_id 不同 | `KeptHost` + host auth.json 保留 |

每个测试都用真实 `sshd` fixture（不是 mock）：
- loopback port sshd
- ed25519 host key + client key + known_hosts
- 真实 `ssh` 命令把 auth.json 推到 sandbox（`mkdir -p && printf %s > ...`）
- 真实 `ssh cat <remoteHome>/auth.json` 把 sandbox bytes 拉回 host
- 真实 `CodexAuthMergeDecider`（生产决策器，复用 `decide_codex_auth_merge_from_paths`）

## 3. 验证证据

```
$ cargo test -p pc-adapter-codex-local --test round599_codex_remote_auth_copyback
running 4 tests
test codex_remote_auth_copy_back_over_ssh_keeps_host_on_account_mismatch ... ok
test codex_remote_auth_copy_back_over_ssh_installs_newer_credential ... ok
test codex_remote_auth_copy_back_over_ssh_keeps_host_when_sandbox_absent ... ok
test codex_remote_auth_copy_back_over_ssh_keeps_host_when_sandbox_older ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 10.10s
```

完整 Codex adapter 回归（包含 R599 新增的 4 个）：

```
$ cargo test -p pc-adapter-codex-local --tests
test result: ok. 390 passed; 0 failed ...   (lib)
test result: ok. 1 passed                   (adapter_real)
test result: ok. 6 passed                   (r585_staged_codex_home_teardown)
test result: ok. 7 passed                   (round392_codex_skills)
test result: ok. 8 passed                   (round412_codex_errors)
test result: ok. 25 passed                  (round419_codex_execute_helpers)
test result: ok. 1 passed                   (round425_codex_prompt_injection)
test result: ok. 3 passed                   (round428_codex_retry_loop)
test result: ok. 17 passed                  (round468_codex_test_environment)
test result: ok. 11 passed                  (round469_remote_workspace)
test result: ok. 6 passed                   (round477_remote_runtime)
test result: ok. 4 passed                   (round492_bridge_start)
test result: ok. 3 passed                   (round493_process_session_bridge)
test result: ok. 2 passed                   (round495_codex_remote_execute)
test result: ok. 4 passed                   (round599_codex_remote_auth_copyback)  ← NEW
```

合计：**488 个 codex-local 测试 0 失败**（R598 末 484 + R599 新增 4）。

## 4. 设计要点（最佳 Rust 实现）

1. **纯函数 + 闭包注入决策器**：决策器以 `CopyBackCodexAuthDecider` trait 注入，
   生产用 `CodexAuthMergeDecider`（复用 inbound merge 同一份决策谓词，保证
   双向对称），测试可注入 `AlwaysUseSource` / `AlwaysKeepDest` 验证边界。
2. **绝不输出 token bytes**：log sink 只接受决策 outcome 行；sandbox bytes
   仅在 stage temp 内流转，从未进 log / error。
3. **errno 区分**：absent sandbox `auth.json`（`ENOENT`）映射为
   `ErrorKind::NotFound`，走良性 `KeptHost`；其余 read error 保留 fail-loud。
4. **teardown 阶段 hook**：copy-back 在 `execute` 返回前的最后一步、在
   `started_bridge.stop()` 之前完成（对齐 Node `restore` seam 语义）。
5. **测试真实性**：每个测试走完完整链路 — 真 sshd、真 push、真 pull、真
   decision、真 rename、真 fs 状态校验。无 mock。

## 5. 与 Node 一致性

| Node 行为 | Rust 实现 |
|---|---|
| `decideExitCode` via `node .cjs` 子进程 | `CodexAuthMergeDecider` 直接复用 `decide_codex_auth_merge_from_paths` 纯谓词 |
| ENOENT → `kept-host` 良性 no-op | `ErrorKind::NotFound` → `CopyBackCodexAuthOutcome::KeptHost` |
| exit 10 → `rename` temp → host | `fs::rename` 同 fs 原子交换 |
| exit 20 → 丢弃 temp | 决策后走 KEEP 分支，finally 删除 |
| finally 清理 temp | `with_directory_merge_lock` 闭包内 finally |
| `LogFn` 非泄漏 | `Arc<dyn Fn(String) -> BoxFuture>`，log 仅含 outcome 行 |

