# R636 — middleware 补齐 batch 2 (validate / board-mutation-guard / error-handler 映射)

## Status

DONE — 三个 middleware + 错误映射扩展对齐 Node middleware/error-handler.ts 全部分支。
pc-http 全量 473 测试绿；pc-server 装配编译通过。

## Files added / modified

| Path | Status | Notes |
|---|---|---|
| crates/pc-http/src/middleware/validate.rs | new (~75 LOC) | serde_path_to_error 校验器 + Node 形状 zod details |
| crates/pc-http/src/middleware/board_mutation_guard.rs | new (228 LOC) | board actor origin/referer 守卫 + trusted-origin 解析 |
| crates/pc-http/src/middleware/mod.rs | modified | 注册 + 重新导出 |
| crates/pc-http/Cargo.toml | modified | + serde_path_to_error |
| crates/pc-http/src/error.rs | rewrite (~290 LOC) | Node error-handler.ts 全部分支 + Zod 形状 |
| apps/pc-server/src/main.rs | modified | 注册 board_mutation_guard_layer（auth 之后） |

## 与 Node 语义对齐

### validate (middleware/validate.ts)
- serde_path_to_error 携带字段路径，与 Zod issue.path 等价
- 失败返回 400 + {error: "Validation error", details: <zod 形态>}, details 形如 [{path:[...], message: "..."}]

### board-mutation-guard (middleware/board-mutation-guard.ts)
- 安全方法 (GET/HEAD/OPTIONS) 放行
- 非 board actor（agent / system）放行
- board actor 且 source 属于 {LocalImplicit, ApiKey, CloudTenant} 放行
- 其余 board mutation 要求 Origin / Referer 命中 trusted-origin 集合
- trusted-origin = 默认本地 + http(s)://<Host> + PAPERCLIP_PUBLIC_URL
- host 优先 x-forwarded-host 然后 Host 头
- 失败返回 403 + {error: "Board mutation requires trusted browser origin"}

### error-handler (Node middleware/error-handler.ts 全部分支)
- HttpError → 扁平 JSON: {error, code, reason, remediation, connection, subject, grantId, details}
- details.code == skill_policy_denied 走脱敏分支：暴露 reason, 隐藏 details
- details.code 属于 STRUCTURED_CONNECTION_ERROR_CODES
  (user_authorization_required / grant_revoked / needs_reauthorization /
   installation_required / connection_not_installed / subject_not_permitted)
  → 额外展开 connection / subject / grantId 顶层字段；remediation 允许 object/string
- ZodError → 400 + {error: "Validation error", details: <issues>}
- 其它 Error → 500 + {error: "Internal server error"}, 原始消息进 tracing.error

### 新增 ApiError 变体
- Http { status, message, details } — 服务端任意 status 的扁平错误体
- Validation(Value) — Zod/serde 校验失败，details 即 zod issues 数组
- ConflictWith { message, payload } — 顶层平展 payload（status 409）

### 测试覆盖
- error.rs 6 项：not_found_renders_node_shape / validation_renders_zod_shape /
  http_error_with_structured_connection_details /
  http_error_skill_policy_denied_redacts_details / internal_error_hides_message /
  conflict_with_flattens_payload
- validate 4 项：path 提取、空路径过滤、嵌套对象、缺失字段 message
- board_mutation_guard 13 项：safe method、agent 放行、API key/CloudTenant 放行、
  无 origin 拒绝、Origin 命中放行、Referer 命中放行、trusted-origin
  包含 Host / x-forwarded-host / PAPERCLIP_PUBLIC_URL

## Test results（真实执行输出）

```
cargo test -p pc-http --lib middleware       -> 108 passed; 0 failed
cargo test -p pc-http --lib validate         ->   4 passed; 0 failed
cargo test -p pc-http --lib board_mutation   ->  13 passed; 0 failed
cargo test -p pc-http --lib error            ->  17 passed; 0 failed
cargo test -p pc-http --lib                  -> 473 passed; 0 failed
cargo check -p pc-server                     -> Finished dev profile (0 error)
```

## Design decisions

1. build_node_body 单点函数：所有扁平化、脱敏、连接恢复字段展开都在 build_node_body
   中完成；ApiError::Http / Validation 各自只负责壳。
2. 服务端 status 自由度：保留 ApiError::Http { status, message, details }, 业务可显式
   指定 4xx 而不拘泥 BadRequest/Forbidden；与 Node new HttpError(400, msg, {code: ...}) 对齐。
3. board guard 注册顺序：放在 Extension(hostname_guard/trust_proxy) 之后、auth_layer 之前
   的 .route_layer(from_fn(...))。实测 axum 后注册的 route_layer 外层先执行 →
   请求顺序为 auth_layer → csrf_layer → board_mutation_guard → handler。
4. tracing 集中：5xx 走 tracing::error! 由 pc-telemetry 统一采集，不在 error.rs
   中内嵌额外结构（与 Node 走 logger 单一链路对齐）。

## Next

R637：运行时服务 batch 1（run-continuations / run-log-store / issue-liveness）。