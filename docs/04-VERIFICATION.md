# Build / Verification

## 编译 / 测试（需要 Rust toolchain ≥ 1.80 + PostgreSQL ≥ 14）

```bash
# Workspace build
cargo build --workspace

# Run all tests
cargo test --workspace

# Lint
cargo clippy --workspace --all-targets -- -D warnings

# Format check
cargo fmt --all --check
```

CI（GitHub Actions 推荐工作流）：

```yaml
- name: cargo build
  run: cargo build --workspace --locked
- name: cargo test
  run: cargo test --workspace --locked
- name: cargo clippy
  run: cargo clippy --workspace --all-targets --locked -- -D warnings
- name: cargo fmt
  run: cargo fmt --all -- --check
```

## 数据库迁移（开发机 / CI）

```bash
# 应用 migrations
export MULTICA_DATABASE_URL='postgres://multica:multica@127.0.0.1:5432/multica'
multica-migrate run --dir migrations

# 列出已应用
multica-migrate status

# JSON 报告
multica-migrate run --dir migrations --json
```

## 服务器 / CLI

```bash
# 后端
export MULTICA_DATABASE_URL='postgres://multica:multica@127.0.0.1:5432/multica'
export MULTICA_PORT=3500
./target/release/multica-server

# 健康检查
curl -s http://127.0.0.1:3500/api/health
# {"status":"ok","service":"multica-rs","version":"...","db":"healthy"}

# OpenAPI
curl -s http://127.0.0.1:3500/api/openapi.json | jq .

# CLI
./target/release/multica health
./target/release/multica version
```

## 当前 M0 范围覆盖

| 能力 | crate | 状态 |
| --- | --- | --- |
| env 配置 | `mc-config` | ✅ |
| home paths | `mc-config::home_paths` | ✅ |
| 错误模型 + HTTP 状态 | `mc-errors` | ✅ |
| tracing + 横幅 + 重写 | `mc-telemetry` | ✅ |
| sqlx 连接池 | `mc-db::pool` | ✅ |
| 健康检查 | `mc-db::health` | ✅ |
| 迁移 runner | `mc-db::migrate` | ✅ |
| migrate CLI | `mc-migrate` | ✅ |
| 领域类型（24 个文件） | `mc-core` | ✅ |
| Actor 注册表 | `mc-core::actor` | ✅ |
| session / cookie / API key / PAT / verification / password | `mc-auth` | ✅ |
| 资源 × 动作 × 主体授权 | `mc-authz` | ✅ |
| 事件总线 + envelope | `mc-realtime` | ✅ |
| WS handler | `mc-ws` | ✅ |
| local-disk / s3 storage | `mc-storage` | ✅ |
| AES-GCM secret cipher | `mc-secrets` | ✅ |
| feature flag catalog | `mc-feature-flags` | ✅ |
| OpenAPI spec | `mc-openapi` | ✅ |
| plugin JSON-RPC protocol | `mc-plugin-protocol` | ✅ |
| 仓储 trait skeleton | `mc-repos` | ✅ |
| axum router + middleware | `mc-http` | ✅ |
| multica-server | `apps/mc-server` | ✅ |
| multica CLI | `apps/mc-cli` | ✅ |
| 0001_init.up.sql（核心实体） | `migrations/` | ✅ |

## M1+ 路线

| 里程碑 | 计划 |
| --- | --- |
| M1 | workspace / member / invitation / PAT / verification routes + services + auth 完整化 |
| M2 | issue / comment / inbox / reactions / triage |
| M3 | runtime / agent / task queue + 26 个 runtime adapter |
| M4 | chat / project / squad |
| M5 | autopilot / wakeup / cron |
| M6 | skill / plugin host (multica 自有) / package / hook |
| M7 | 6 个 channel（Slack / Lark / DingTalk / WeCom / Telegram / Custom） |
| M8 | VCS / GitHub / MCP |
| M9 | activity / onboarding / cloud / feedback |
| M10 | UI 兼容 / 性能 / 文档 / release |

## 已知约束

1. **actor 抽象**：当前使用简化版（`Arc<dyn AnyActor>`），替代 kameo 的 typed actor；后续 milestone 如需 swap，可平滑替换。
2. **sqlx 仓储**：`mc-repos` 当前仅有 trait skeleton；M1 起按实体填充。
3. **数据库迁移**：当前 1 个迁移文件（核心实体）；M2+ 持续追加，复用 multica schema。
4. **runtime adapter**：26 个 crate 待 M3 落地；当前仅 `mc-adapter-pi-local` 可直接复用 paperclip-rs 实现（尚未声明 workspace 成员）。