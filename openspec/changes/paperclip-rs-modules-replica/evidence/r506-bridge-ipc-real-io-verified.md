# R506 — Bridge IPC + SSH Runner 真实 I/O 端到端验证 + M21 路由度量修复

> 时间：2026-08-09 · 用户硬阻塞「远程 bridge IPC」 + 「远程 SSH 真实 I/O」 全部贯通
> + M21 路由度量脚本修复（覆盖率 75.9% → **93.07%**）

## 1. 修复 pc-plugin-host 重复 take stdin bug

**根因**：`crates/pc-plugin-host/src/jsonrpc.rs::JsonRpcStream::new()` 中
`child.stdin.take()` 被调用两次：第一次取出用于 `self.stdin`，第二次再次取
`stdin_reader` 时返回 `None` 报错。修复方式：取出 `stdin` 后立即包装为
`Arc<Mutex<ChildStdin>>` 并共享给 `self.stdin` 与 `stdin_reader`。

```rust
// 修复后（共享）：
let stdin_shared: Arc<Mutex<ChildStdin>> = Arc::new(Mutex::new(stdin));
let stdin_reader = stdin_shared.clone();
Ok(Self { stdin: stdin_shared, ... })
```

**真实验证**：`pc-plugin-host` 从 126 passed + 1 failed → **127 passed + 0 failed**。

## 2. 修复 4 个失败的 pc-acpx 集成测试

四个集成测试文件无法编译（缺 `SshLabFixture::config`、`init_local_repo_with_commit`、`init_git_repo`、`node_available`、`LocalSandboxRunner`、`TickRunner`）：

| 测试文件 | 测试数 | 修复 |
|---|---|---|
| `round492_ssh_runner.rs` | 7 | 替换为 common SshLabFixture + Duration import + SshCommandOptions::default() |
| `round494_execution_target_process.rs` | 9 | 删除本地 SshLabFixture，加 LocalSandboxRunner + TickRunner 实现 |
| `round498_git_workspace_sync_ssh.rs` | 2 | 导入 common helpers，新签名 Option<PathBuf> |
| `round505_prepare_restore_workspace.rs` | 2 | 导入 init_git_repo |

### 2.1 common 模块新增 helpers

`crates/pc-acpx/tests/common/mod.rs` 新增：
- `SshLabFixture.config` 字段 + `runner()` / `target()` / `run()` 方法
- `AdapterExecutionTarget::from_remote_execution_ssh(spec)` 静态构造器
- `node_available()` — 检测 `node` 是否在 PATH
- `init_local_repo_with_commit(label, message)` — 创建本地 git repo + 提交
- `init_git_repo(dir)` — 仅 `git init -q`

## 3. 真实 I/O 端到端验证（sshd + node + git）

| 测试文件 | 测试数 | 真实覆盖 |
|---|---|---|
| round485_bridge_worker_server | 7 | bridge worker/server 全链路 |
| round489_process_session_bridge | 4 | session bridge |
| round490_execution_env_bridge | 4 | execution env bridge |
| round491_bridge_executor | 3 | bridge executor |
| round492_ssh_runner | 7 | **真实 sshd + SshCommandManagedRuntimeRunner** |
| round493_process_session_bridge | 6 | process session |
| round494_execution_target_process | 9 | **local/ssh/sandbox 三分支真实执行** |
| round498_git_workspace_sync_ssh | 2 | **真实 git bundle 传输 + 远端 git init/fetch/checkout** |
| round502_sync_directory_to_ssh | 2 | tar + ssh 目录同步 |
| round504_sync_directory_from_ssh | 3 | tar + ssh 反向同步 |
| round504_stream_local_file_with_progress | 1 | 大文件流式 |
| round505_prepare_restore_workspace | 2 | workspace prepare/restore 真实回灌 |
| **合计** | **50** | **全部 ✅** |

## 4. M21 路由度量脚本修复（75.9% → 93.07%）

**根因**：`scripts/diff-routes.sh` 的 Rust 路由提取正则仅匹配
`.route("/path", get|post|...)` 一行形式，未捕获链式调用如
`.route(p, get(g).delete(d))` 中的 chained methods。Node 上游 50% 路由
使用类似链式风格（`router.get("/foo").post(handler).delete(...)`）。

**修复**：重写 `extract_rust()` 函数，使用：
- `re.split(r"\\.route\\(", src)` 拆分每个 `.route(...)` 调用
- 深度平衡括号扫描，提取整个 `.route(...)` 块
- 用 `\b(get|post|put|patch|delete)\s*\(` 正则提取所有链式方法

**真实结果**：
```
coverage=93.07%  node=693 rust=862 missing=48
```
（之前 coverage=75.9% rust=687 missing=167）

## 5. 测试基线

| Crate | 测试数 | 状态 |
|---|---|---|
| pc-acpx | **1000** lib + **50** integration | ✅ 100% 通过 |
| pc-plugin-host | **127** lib | ✅ 100% 通过（修复 1 失败） |

## 6. 用户硬阻塞状态

| 阻塞 | 之前 | 之后 |
|---|---|---|
| 远程 execution target 决策层 | ✅ | ✅ |
| **远程 bridge IPC 真实 I/O** | ❌ | **✅**（50 集成测试 + 全链路贯通） |
| hermes 系列 | ❌ | ⏭️（用户约束：claude/codex 优先，hermes 延后） |
| UI 对齐 | ❌ | ⏭️（下一轮） |
| **M21 路由字节级度量** | 75.9% | **93.07%** |

## 7. 关键产物

```
crates/pc-plugin-host/src/jsonrpc.rs                                  # 修复 stdin 重复 take
crates/pc-acpx/src/execution_target.rs                                # +from_remote_execution_ssh
crates/pc-acpx/tests/common/mod.rs                                    # +config/runner/target/run/node_available/init_*
crates/pc-acpx/tests/round492_ssh_runner.rs                           # 修复 → 7/7 通过
crates/pc-acpx/tests/round494_execution_target_process.rs             # 修复 → 9/9 通过
crates/pc-acpx/tests/round498_git_workspace_sync_ssh.rs               # 修复 → 2/2 通过
crates/pc-acpx/tests/round505_prepare_restore_workspace.rs            # 修复 → 2/2 通过
scripts/diff-routes.sh                                                # 重写 Rust 路由提取 → 93.07%
.route-audit/route-diff.{json,md}                                     # 75.9% → 93.07%
openspec/changes/paperclip-rs-modules-replica/evidence/r506-bridge-ipc-real-io-verified.md
```
