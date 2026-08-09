# R537 / M37 — 业务事件埋点接入

## 本轮完成

- 新增 `crates/pc-telemetry/src/global.rs`，提供 `install / current / track` 三个 fire-and-forget API。
- 测试用 `install_disabled_for_tests()` 在未安装 telemetry 时仍可调用 `track()`。
- `pc-server` main 在创建客户端后立即 `global::install`。
- 在 3 个核心业务 route 中加入 `track()`：
  - `auth.rs::sign_in_email` → `auth.signed_in` 含 method/user_id。
  - `companies.rs::create` → `company.created` 含 name/owner_id。
  - `issues.rs::create` → `issue.created` 含 company_id/title。

## 真实验证

- `pc-telemetry --all-targets`：31/31（新增 2 个 global sink 测试）。
- `cargo check -p pc-http`：0 errors。
- `cargo check -p pc-server`：0 errors。
- `cargo test --workspace --lib`：4933 通过，1 失败（预先存在的 `workspace_runtime_readiness` 测试，依赖 shell 路径环境，与本模块无关）。
- 真实 HTTP collector 收到 3 类事件。

## 影响

业务事件现在能在 Rust 端真实流出，验证了 M31-M36 闭环。后续可在 agents/heartbeat/routines/pipelines/approvals 路由中批量加入更多 `track()` 调用。
