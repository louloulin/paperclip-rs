# R570 — R-INTEGRATION-10: pc-workspace-commands → pc-cli

**状态**: ✅ 完成 (2026-08-12)

## 1. 目标

将 R548 创建的 `pc-workspace-commands` crate（提供 workspace_runtime 配置解析的
catalog helpers：`list_workspace_command_definitions`、`find_workspace_command_definition`
等）接入 `pc-cli`，新增 `workspace-commands {list|get}` 子命令，
让运维无需启动 server 即可探查 workspace_runtime 配置。

## 2. 设计

`pc-workspace-commands` 是一个**配置解析器**（不是静态 catalog）— 它从
`workspace_runtime` JSON 配置中提取命令定义。所以 CLI 集成采用 file-based
模式：接受 `--config <path>` 参数读取 JSON，再用 helper 提取。

## 3. 集成实现（apps/pc-cli/src/main.rs + Cargo.toml）

### 3.1 新增依赖

```toml
# apps/pc-cli/Cargo.toml
pc-workspace-commands = { path = "../../crates/pc-workspace-commands" }
```

### 3.2 新增 subcommand

```rust
WorkspaceCommands {
    #[command(subcommand)]
    action: WorkspaceCommandsAction,
}

enum WorkspaceCommandsAction {
    List {
        #[arg(long)] config: PathBuf,
        #[arg(long)] service_only: bool,
    },
    Get {
        #[arg(long)] config: PathBuf,
        #[arg(long)] id: String,
    },
}
```

### 3.3 Dispatch

```rust
Command::WorkspaceCommands { action } => workspace_commands_command(action),
```

### 3.4 handler

```rust
fn workspace_commands_command(action: WorkspaceCommandsAction) -> Result<()> {
    match action {
        List { config, service_only } => {
            let raw = std::fs::read_to_string(&config)?;
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            let defs = if service_only {
                list_workspace_service_command_definitions(Some(&value))
            } else {
                list_workspace_command_definitions(Some(&value))
            };
            // render table
        }
        Get { config, id } => {
            let raw = std::fs::read_to_string(&config)?;
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            match find_workspace_command_definition(Some(&value), Some(&id)) {
                Some(def) => { /* render */ }
                None => { println!("not found"); }
            }
        }
    }
}
```

## 4. 真实 CLI 验证

```bash
$ cat /tmp/test_workspace_runtime.json | jq '.commands | length'
3

$ ./target/debug/paperclipai workspace-commands list --config /tmp/test_workspace_runtime.json
id                           kind     lifecycle  name
------------------------------------------------------------------------
claude-code-cli              service  shared     Claude Code CLI
codex-cli                    service  shared     Codex CLI
smoke-test                   job      -          Smoke Test

$ ./target/debug/paperclipai workspace-commands list --config /tmp/test_workspace_runtime.json --service-only
id                           kind     lifecycle  name
------------------------------------------------------------------------
claude-code-cli              service  shared     Claude Code CLI
codex-cli                    service  shared     Codex CLI

$ ./target/debug/paperclipai workspace-commands get --config /tmp/test_workspace_runtime.json --id claude-code-cli
id:        claude-code-cli
name:      Claude Code CLI
kind:      service
lifecycle: shared
command:   claude-code --serve
cwd:       /workspace

$ ./target/debug/paperclipai workspace-commands get --config /tmp/test_workspace_runtime.json --id nope
workspace command `nope` not found in /tmp/test_workspace_runtime.json
```

## 5. 测试 (apps/pc-cli/tests/r570_workspace_commands.rs)

7 个集成测试：

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r570_list_returns_all_three_commands` | list 返回 3 个命令 |
| 2 | `r570_list_service_only_filters_jobs` | service-only 过滤 job |
| 3 | `r570_list_empty_config_returns_empty` | empty/None config 返回空 |
| 4 | `r570_find_command_by_id_resolves` | by-id 查找返回完整定义 |
| 5 | `r570_find_command_unknown_id_returns_none` | unknown id / None → None |
| 6 | `r570_lifecycle_strings_round_trip` | enum 字符串渲染 |
| 7 | `r570_disabled_reason_surfaces_in_definition` | disabled_reason 字段透传 |

## 6. 无回归验证

```bash
$ cargo test -p pc-workspace-commands --test r548_workspace_commands
test result: ok. 27 passed; 0 failed

$ cargo test -p pc-cli --test r570_workspace_commands
test result: ok. 7 passed; 0 failed

$ cargo build -p pc-cli --bin paperclipai
warning: pc-cli (bin paperclipai) generated 1 warning
Finished `dev` profile
```

## 7. 设计亮点

### 7.1 File-based CLI（无 server 依赖）

`paperclipai workspace-commands` 完全离线运行 — 不需要启动 server、不需要 DB。
运维 / CI 可独立探查 workspace_runtime 配置合法性。

### 7.2 单点真相

CLI 通过 `pc-workspace-commands` helpers 解析，与 server 端 runtime matching
使用**同一套解析逻辑**（保证 CLI 输出和 server 实际行为一致）。

### 7.3 完整字段透传

`list` 命令展示 id / kind / lifecycle / name；
`get` 命令额外展示 command / cwd / disabled_reason。
operator 排错时能直接看到所有关键字段。

## 8. 累计 R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | pc-mentions → pc-issues | ✅ R562 |
| 3 | pc-pipeline-case-type → pc-pipelines | ✅ R563 |
| 4 | pc-adapter-type → 各 adapter crate | ✅ R564 |
| 5 | pc-portability-fidelity → pc-portability | ✅ R565 |
| 6 | pc-execution-workspace-guards → pc-http | ✅ R566 |
| 7 | pc-external-objects → pc-issue-references | ✅ R567 |
| 8 | pc-app-definitions → pc-http route | ✅ R568 |
| 9 | pc-trust-policy → pc-authz | ✅ R569 |
| 10 | **pc-workspace-commands → pc-cli** | ✅ **R570** |
| 11 | pc-api-routes → pc-http | 待做 |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**10/12 = 83%**

## 9. 下一步

- **R571**: R-INTEGRATION-11 — pc-api-routes → pc-http

