# R576 — `/api/companies/:company_id/events/ws` 公司范围 WS

**状态**: ✅ 完成 (2026-08-12)

## 1. 背景

M19 路由审计显示 UI 客户端在
`components/transcript/useLiveRunTranscripts.ts` 中真实调用
`/api/companies/{companyId}/events/ws`，但 Rust 端之前未实现。

## 2. 实现

### 2.1 新增模块 `crates/pc-http/src/routes/company_events_ws.rs` (286 LOC)

```rust
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/api/companies/:company_id/events/ws",
        get(handler),
    )
}
```

### 2.2 设计要点

- **路径 = scope**: `:company_id` 在 URL 里强制服务器按公司过滤
  - 客户端不需要发 subscribe 帧（避免漏发导致数据泄露）
- **复用鉴权**: `authorize_ws` from `live_events`（同一规则集）
- **复用 realtime bus**: 通过 `ws_state.realtime.subscribe_with_resume()`
  获取 `(replay, receiver)`
- **强制公司过滤**: `event_company_id_matches()` 仅放行 `event.company_id == Some(path_company_id)`
- **Lag-tolerant**: `RecvError::Lagged(_)` 不断开连接（与 SSE 流一致）
- **Keepalive**: 处理 Ping → Pong，关闭消息正常结束

### 2.3 与 `/api/live-events` 的区别

| 维度 | `/api/live-events` | `/api/companies/:id/events/ws` |
|---|---|---|
| Scope 来源 | 客户端 subscribe 帧 | URL 路径参数 |
| Company 过滤 | 客户端可选 | 强制 |
| 多公司支持 | 是 | 单公司 |
| 适用场景 | 系统级实时总线 | 公司 dashboard 实时更新 |

## 3. 测试

### 3.1 Lib 单元测试（4 个）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r576_match_when_company_id_equals` | company_id 匹配 |
| 2 | `r576_mismatch_when_company_id_differs` | 不同 company_id 不匹配 |
| 3 | `r576_mismatch_when_company_id_missing` | 缺 company_id 字段不匹配 |
| 4 | `r576_router_exposes_path` | router() 编译 |

### 3.2 集成测试（6 个 in tests/r576_company_events_ws.rs）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r576_ws_query_deserializes_camel_case` | camelCase 解析 |
| 2 | `r576_ws_query_deserializes_snake_case` | 缺字段默认 None |
| 3 | `r576_ws_query_default_empty` | 空对象解析 |
| 4 | `r576_live_event_with_company_id_matches_path` | with_company 设置 |
| 5 | `r576_live_event_without_company_id_filtered_out` | 无 company_id 被过滤 |
| 6 | `r576_router_path_uses_company_id_param` | router() 可调用 |

### 3.3 测试统计

```
$ cargo test -p pc-http --lib
test result: ok. 381 passed; 0 failed   # 377 pre + 4 R576 new

$ cargo test -p pc-http --test r576_company_events_ws
test result: ok. 6 passed; 0 failed
```

## 4. 无回归验证

- pc-http lib: 377 → **381** (+4)
- pc-http integration: +6
- 其它 crate 无变化

## 5. 设计亮点

### 5.1 路径 = scope 的安全优势

把 company_id 放在 URL 路径里（而不是 query 或 subscribe 帧）有 2 个安全优势：

1. **客户端无法绕过过滤**: 漏发 subscribe 帧不会泄露跨公司数据
2. **日志可审计**: 每个 WS 连接的 URL 是天然审计点（无需解析帧内容）

### 5.2 与 SSE (`/api/realtime/stream`) 的选择

Node 上游 UI 选择 WS 而非 SSE，可能是为了：
- **双向**: 客户端发 ping/控制帧（虽 R576 仅响应 ping）
- **兼容性**: 部分浏览器 EventSource 受同源策略限制更严

### 5.3 Lag-tolerant 设计

```rust
Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
    continue; // 跳过中间事件，不断开
}
```

当 broadcast 通道拥塞时（旧 subscriber 处理慢），Rust tokio broadcast 会
返回 `Lagged` 而不是阻塞。R576 选择 `continue` 而非断开连接——客户端
的 `resume` 参数可在重连时补偿。

## 6. 下一步

R577: 为剩余 13 个 UI client paths 添加 OpenAPI path_schema_hint
条目，让 UI↔OpenAPI 覆盖率从 0% → 100%。
