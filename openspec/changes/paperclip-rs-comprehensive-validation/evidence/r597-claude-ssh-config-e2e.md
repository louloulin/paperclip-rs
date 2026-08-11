# R597 Claude SSH config 真实端到端验证

## 验证范围

在既有 `round496` 真实 `sshd` fixture 上验证 R596 的完整远程配置链路：

1. 启动临时真实 `sshd`。
2. 通过 `sync_directory_to_ssh` 使用真实 `tar` + `ssh` 将本地 Claude seed 上传到远端。
3. 通过 `SshCommandManagedRuntimeRunner` 执行配置物化 shell 命令。
4. 通过 SSH `cat` 读取最终远端文件，而不是读取本地镜像目录。

## 断言

- `settings.json` 从 seed 复制成功。
- seed 中的 `credentials.json` 优先于远端 `$HOME/.claude/credentials.json`。
- 远端 HOME 中仅存在的 `.credentials.json` 被补齐。
- shell 路径包含空格时仍可安全执行。

## 结果

```text
cargo test -p pc-adapter-claude-local --test \
  round496_claude_remote_execute \
  claude_remote_config_stages_seed_and_materializes_over_ssh -- --nocapture
→ 1 passed, 1 filtered out (10.22s)

cargo test -p pc-adapter-claude-local --tests
→ 498 passed (14 suites)

rustfmt --edition 2021 --check \
  crates/pc-adapter-claude-local/src/claude_remote_config.rs \
  crates/pc-adapter-claude-local/src/lib.rs \
  crates/pc-adapter-claude-local/tests/round496_claude_remote_execute.rs
→ passed
```

## 当前边界

- 本轮验证的是 SSH transport；sandbox provider runner 仍需下一轮实现。
- Codex remote config path 尚未复用该 Claude 模块，列入后续 G6。
