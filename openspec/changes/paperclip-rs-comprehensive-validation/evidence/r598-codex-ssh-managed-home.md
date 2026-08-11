# R598 Codex SSH managed home

## Node 对齐

本轮对齐 Node `codex-local/src/server/execute.ts` 的远程 home asset seam：

- `stageCodexHomeForSync`
- `prepareRemoteManagedRuntime` 的 `home` asset
- 远程执行前重映射 `env.CODEX_HOME`

## Rust 实现

- `crates/pc-adapter-codex-local/src/codex_remote_home.rs`
  - 复用 `codex_home_staging` 白名单和 symlink containment 保护
  - 复用 `sync_directory_to_ssh` 的真实 tar/SSH transport
  - 输出隔离的 `<remoteCwd>/.paperclip-runtime/codex/home`
- `crates/pc-adapter-codex-local/src/lib.rs`
  - `CodexLocalAdapter::execute` 在 bridge 启动前执行 SSH managed-home staging
  - 最终子进程环境使用远程 `CODEX_HOME`
- `crates/pc-adapter-codex-local/tests/round495_codex_remote_execute.rs`
  - 真实 sshd staging 验证

## 真实验证

```text
cargo test -p pc-adapter-codex-local --test \
  round495_codex_remote_execute \
  codex_managed_home_stages_allowlist_over_real_ssh -- --nocapture
→ 1 passed, 1 filtered out (10.09s)

cargo test -p pc-adapter-codex-local --tests
→ 484 passed (14 suites)

cargo check -p pc-adapter-codex-local
→ 0 errors

rustfmt --check + git diff --check
→ passed
```

断言包含：`auth.json`、`config.toml`、`skills/**` 上传成功，运行态
`sessions.sqlite` 不被上传。

## 未完成边界

- sandbox provider runner 尚未实现，Codex sandbox 仍保持原有 fallback 语义。
- remote auth copy-back 生命周期尚未接入；当前只完成入站 home staging。
- workspace 级 `cargo clippy -D warnings` 仍被既有约 690 条 lint 阻断，主要位于
  `pc-acpx`、`pc-activity`、`pc-adapter-api`，不是本轮新增逻辑引入。
