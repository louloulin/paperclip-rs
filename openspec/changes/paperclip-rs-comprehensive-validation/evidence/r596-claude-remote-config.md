# R596 Claude 远程配置物化

## 范围

对齐 Node `packages/adapters/claude-local/src/server/claude-config.ts`：

- `buildRemoteClaudeConfigMaterializationCommand`
- `materializeRemoteClaudeConfig`
- seed 目录同步到 SSH 远端
- `.credentials.json` / `credentials.json` 的 HOME 回退
- seed 已存在凭据时不覆盖

## Rust 实现

- `crates/pc-adapter-claude-local/src/claude_remote_config.rs`
  - 纯 shell 命令构造
  - `BridgeCommandRunner` 注入式执行
  - SSH runner 路由
  - 本地 seed → SSH `config-seed` staging
  - 远程环境继承变量 sanitization
- `crates/pc-adapter-claude-local/src/lib.rs`
  - 远程 Claude 执行中接入
  - config 物化发生在 Paperclip bridge 启动之前
  - 物化后设置 `CLAUDE_CONFIG_DIR`
- `crates/pc-adapter-claude-local/tests/r596_remote_claude_config.rs`
  - shell quoting
  - seed 递归复制
  - seed 凭据优先级
  - HOME 凭据回退
  - local / sandbox target 拒绝策略

## 验证

```text
cargo test -p pc-adapter-claude-local --test r596_remote_claude_config
→ 4 passed

cargo test -p pc-adapter-claude-local --lib
→ 421 passed

git diff --check
→ passed
```

严格 clippy 命令已执行，但当前 workspace 既有 `pc-adapter-api` lint 阻断：
`models_env.rs`、`plugin_store.rs`、`registry_bootstrap.rs` 和
`pc-adapter-api/src/lib.rs` 存在与本模块无关的 `-D warnings` 问题；本次新增
`claude_remote_config.rs` 未出现在 clippy 错误列表中。

## 未完成边界

- sandbox provider runner 尚未由 Rust 实现，当前显式返回配置错误而不是静默成功。
- 本轮真实执行验证使用本地 `LocalProcessBridgeRunner`；SSH staging 复用已有
  `sync_directory_to_ssh` 与 `SshCommandManagedRuntimeRunner`，完整 SSH lab 回归需在
  磁盘空间恢复后执行。
