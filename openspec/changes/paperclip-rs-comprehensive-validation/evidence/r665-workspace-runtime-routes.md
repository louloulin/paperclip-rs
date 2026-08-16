# R665 - workspace-runtime route 完整接入

## 目标

把 `pc-core` 中已实现的 `workspace_runtime_readiness` 和 `workspace_realization` 域
暴露为 HTTP route，与 Node `services/workspace-runtime.ts` + `services/workspace-runtime-read-model.ts`
对齐，提供 backend 一致性。

## 实现

### 1. 新增 `crates/pc-http/src/routes/workspace_runtime.rs` (349 行)

5 个 endpoint（pure function wrapper，不需要 DB）：

| Method | Path | 说明 |
|--------|------|------|
| GET | `/api/workspace-runtime/health` | 子系统健康（公开路径） |
| POST | `/api/workspace-runtime/readiness-timeout` | 调用 `pc_core::workspace_runtime_readiness::resolve_workspace_runtime_readiness_timeout_sec` |
| POST | `/api/workspace-runtime/is-dev-service` | 调用 `pc_core::workspace_runtime_readiness::is_paperclip_dev_runtime_service` |
| POST | `/api/workspace-runtime/realization/parse` | 调用 `pc_core::workspace_realization::read_workspace_realization_request` |
| POST | `/api/workspace-runtime/realization/build` | 构造 v1 realization request JSON（dry-run，不需要 DB） |

设计要点：
- 全部为 pure function wrapper（不查 DB，不引入 side effect）
- 输入/输出 JSON 字段命名对齐 Node 端 camelCase（如 `timeoutSec`、`service_name`、`provision_command`）
- 错误情况返回 None / 空字段而不是 500，方便客户端降级

### 2. 合并到 routes/mod.rs

```rust
pub mod workspace_runtime;
// ...
.merge(workspace_runtime::router())
```

### 3. 扩展 `is_public_auth_path` 通用化

修改 `crates/pc-http/src/middleware/auth.rs`：
- 之前：硬编码 `/health`、`/api/health`
- 现在：通用 `/api/<subsystem>/health` 全部豁免
- 这样未来新增 subsystem health 自动公开，无需逐个加白名单

### 4. 单测（5 个新增）

```
test routes::workspace_runtime::tests::is_dev_service_matches_paperclip_dev ... ok
test routes::workspace_runtime::tests::realization_parse_rejects_wrong_version ... ok
test routes::workspace_runtime::tests::realization_parse_accepts_v1 ... ok
test routes::workspace_runtime::tests::readiness_timeout_uses_explicit_value ... ok
test routes::workspace_runtime::tests::readiness_timeout_dev_server_heuristic_90s ... ok

test result: ok. 5 passed; 0 failed
```

并更新 `public_auth_path_whitelist` 测试验证 subsystem health 豁免。

## 真实 curl 验证

### LOCAL_TRUSTED 模式

```
1. GET /api/workspace-runtime/health:
   → 200 {status: ok, endpoints: [4 paths]}

2. POST readiness-timeout (explicit=120):
   → {timeout_sec: 120, dev_server_heuristic: false}

3. POST readiness-timeout (npm run dev):
   → {timeout_sec: 90, dev_server_heuristic: true} (启发式)

4. POST readiness-timeout (echo hi):
   → {timeout_sec: 30, dev_server_heuristic: false} (默认)

5. POST is-dev-service (paperclip-dev):
   → {is_dev: true, reason: name_or_command_matches_paperclip_dev}

6. POST is-dev-service (postgres):
   → {is_dev: false, reason: no_match}

7. POST realization/parse (v1):
   → parsed 完整 WorkspaceRealizationRequest JSON, version_matched: true

8. POST realization/parse (v2):
   → parsed: null, version_matched: false

9. POST realization/build:
   → 完整 v1 realization request JSON, version: 1
```

### AUTHENTICATED 模式（R664 + R665 联合验证）

```
1. /api/health → 200 (公开路径)
2. /api/workspace-runtime/health → 200 (subsystem health 也公开)
3. /api/companies → 403 (R664 require_board_layer 拒绝 anonymous)
4. /api/workspace-runtime/readiness-timeout → 403 (拒绝 anonymous)
```

## 测试无 regression

- `pc-http` lib tests：**483 passed**（478 → 483，新增 5 个 workspace_runtime 测试）
- `pc-http` middleware::auth tests：**10 passed**（含 public_auth_path_whitelist 扩展）
- `pc-server` 编译 + 真实启动验证（15.48s build）
- 启动时间 159ms，access log 正常

## 关键文件改动

| 文件 | 改动 |
|------|------|
| crates/pc-http/src/routes/workspace_runtime.rs | 新增 349 行（5 endpoint + 5 单测） |
| crates/pc-http/src/routes/mod.rs | `+ pub mod workspace_runtime; + .merge(workspace_runtime::router())` |
| crates/pc-http/src/middleware/auth.rs | 通用化 `is_public_auth_path` 支持 `/api/*/health` |

## 后续

- R666: issue-* 子服务（approvals / visibility / recovery-actions）
- R667: environment-* / tool-* 细化
- R668: 集成 e2e + Node 兼容验收（终验）
