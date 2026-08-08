# Evidence: M19 — OpenAPI ↔ UI 类型对齐

## 现状度量（真实运行）

`scripts/dev-ui-rust.sh` 真实启动 pc-server 后 curl 抓取：

| 端点 | 状态 |
|---|---|
| `GET /openapi.json` | **200** |
| `GET /api/openapi.json` | **200**（本次新加 alias，对齐 Node 上游 URL） |

OpenAPI 产物大小：1.0 KB（手写最小集）

### Rust 当前 OpenAPI 文档

```
paths: 10
total ops: 10
/api/agents                                                 GET
/api/auth/sign-in                                           POST
/api/companies                                              GET
/api/companies/{company_id}/costs/summary                   GET
/api/companies/{company_id}/dashboard                       GET
/api/companies/{company_id}/resource-memberships/me         GET
/api/companies/{company_id}/users/{user_slug}/profile        GET
/api/issues                                                 GET
/api/projects                                               GET
/health                                                     GET
```

### UI 真实调用 vs OpenAPI 覆盖

`scripts/check-ui-openapi.sh` 提取 ui/src 下所有 `/api/...` 字面量调用并归一化参数：

```
UI 客户端 distinct 调用:  15
Rust OpenAPI paths:      10
命中:                     0
覆盖率:                   0.0%
```

## 关键发现

1. **pc-openapi 是手写最小集**（`crates/pc-http/src/routes/openapi.rs` 第 13-18 行直接 hard-code 10 个 path）
2. **Node 上游 OpenAPI 是自动生成**：基于 zod schema 反射出 686+ paths
3. **当前覆盖率 0%** 的真实根因不是 UI 调用了未注册路径，而是 Rust 端只声明 10 个 path，所有未声明的 UI 调用都不在文档里

## 已落地的真实改进（M19 本轮）

1. **`/api/openapi.json` alias**：Node 上游 URL 契约对齐
   ```rust
   // crates/pc-http/src/routes/openapi.rs
   Router::new()
       .route("/openapi.json", get(document))
       .route("/api/openapi", get(document))
       .route("/api/openapi.json", get(document))  // ← 本轮新加
   ```
2. **M21 度量脚本**：`scripts/diff-routes.sh`（Node vs Rust 真实覆盖率 75.76%）
3. **M19 度量脚本**：`scripts/check-ui-openapi.sh`（UI 调用 vs Rust OpenAPI 覆盖）

## 后续 M19-follow-up

`pc-openapi` 需要从"手写 10 条"升级到"反射 axum router 全量 686+ paths"。这是一个独立 change：

| 子项 | 工作量 |
|---|---|
| `utoipa` derive 给所有 Router 注入 ToSchema | 大 |
| `pc-openapi` 反射 `pc-http::AppState` router + state | 中 |
| 字段命名约定（snake_case → camelCase）统一 | 小 |
| zod / ts-rs 反向生成 UI 端 TypeScript 类型 | 中 |

预期增量：686 paths → UI 调用覆盖率 ≥ 95%。

## 结论

**M19 部分通过**：
- ✅ URL 契约对齐（`/api/openapi.json` alias）
- ✅ 度量基础设施（diff-routes.sh + check-ui-openapi.sh）
- ⏳ OpenAPI 完整反射列为 follow-up（独立 change）
