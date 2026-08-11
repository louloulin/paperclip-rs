# R509 — ValidationError/ErrorResponse schemas + 422/500 错误响应 + 8 路由 hints

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R509 路线图。
> 目标: 加 error response schemas, 让每个 operation 的 responses 含 422 (POST/PATCH) + 500 (all), 扩展 path hints 到 32 路由 (57% coverage)。

## 改动

### 1. `crates/pc-openapi/src/dto_schemas.rs` — 3 个新 error schemas

**`validation_error_schema()`** (单 field-level error):
```json
{
  "type": "object",
  "properties": {
    "field": {"type": "string", "description": "Dot-path to offending field"},
    "code": {"type": "string", "description": "Machine-readable error code"},
    "message": {"type": "string", "description": "Human-readable error message"}
  },
  "required": ["field", "code", "message"]
}
```

**`validation_error_list_schema()`** (422 wrapper):
```json
{
  "type": "object",
  "properties": {
    "errors": {"type": "array", "items": {"$ref": "#/components/schemas/ValidationError"}},
    "traceId": {"type": ["string", "null"], "description": "Distributed trace id"}
  },
  "required": ["errors"]
}
```

**`error_response_schema()`** (generic 4xx/5xx):
```json
{
  "type": "object",
  "properties": {
    "code": {"type": "string"},
    "message": {"type": "string"},
    "traceId": {"type": ["string", "null"]}
  },
  "required": ["code", "message"]
}
```

**Register**: `register_core_dtos` 加 3 schemas (15 → 18)
**CORE_DTO_NAMES**: 15 → 18

### 2. `crates/pc-http/src/routes/openapi.rs` — `build_responses_block` 签名升级

**新签名**: `build_responses_block(response_schema: Option<&str>, has_request_body: bool) -> Value`

**响应集合**:
| Status | 条件 | 描述 |
|---|---|---|
| 200 | response_schema=Some → content+`$ref` | OK |
| 200 | response_schema=None → minimal `{description: OK}` | OK (no body) |
| 401 | always | Unauthorized |
| 404 | always | Not Found |
| **422** | **has_request_body=true** | **ValidationErrorList `$ref`** |
| **500** | **always** | **ErrorResponse `$ref`** |

**Scanner 集成**: scanner 在调用 `build_responses_block` 时传 `request_body.is_some()`

### 3. 8 个新路由 hints

| Path | Method | Request | Response |
|---|---|---|---|
| `/api/issues/{id}` | GET | — | Issue ⭐R509 |
| `/api/decisions/{id}` | GET | — | Decision ⭐R509 |
| `/api/pipelines/{id}` | GET | — | Pipeline ⭐R509 |
| `/api/routines/{id}` | GET | — | Routine ⭐R509 |
| `/api/pipelines` | POST | Pipeline | Pipeline ⭐R509 |
| `/api/routines` | POST | Routine | Routine ⭐R509 |
| `/api/routines/{id}` | PATCH | Routine | Routine ⭐R509 |
| `/api/heartbeat` | POST | — | HeartbeatRun ⭐R509 |

**总计**: 24 → **32 hints** (57% 路由覆盖)

### 4. Coverage 测试升级 (24 → 32 hints)

**`r506_path_schema_hint_coverage_includes_all_thirty_two`**: 32 case 全覆盖

## 测试 (9 个新 R509 tests)

### pc-openapi (6 tests)

| 测试 | 验证 |
|---|---|
| `r509_validation_error_has_required_fields` | ValidationError.required: field, code, message |
| `r509_validation_error_list_uses_array_ref` | ValidationErrorList.errors 是 array of `$ref:ValidationError` |
| `r509_error_response_required_code_and_message` | ErrorResponse.required: code, message |
| `r509_error_response_trace_id_is_nullable` | `["string", "null"]` pattern |
| `r509_schemas_round_trip_through_yaml` | YAML 含 ValidationError / ValidationErrorList / ErrorResponse |
| `r509_register_core_dtos_registers_eighteen` | 18 schemas 总数 |

### pc-http (3 tests + 6 hint tests = 9 total)

| 测试 | 验证 |
|---|---|
| `r509_responses_block_includes_422_when_request_body_present` | POST/PATCH 有 422 `$ref:ValidationErrorList` |
| `r509_responses_block_omits_422_when_no_request_body` | GET 不含 422 |
| `r509_responses_block_always_includes_500_error_response` | 所有 ops 含 500 `$ref:ErrorResponse` |
| `r509_issues_item_get_returns_issue` | item GET → Issue |
| `r509_decisions_item_get_returns_decision` | item GET → Decision |
| `r509_pipelines_post_round_trips` | POST → Pipeline |
| `r509_routines_post_round_trips` | POST → Routine |
| `r509_routines_patch_round_trips` | PATCH → Routine |
| `r509_heartbeat_post_returns_run` | POST → HeartbeatRun |

## 验证

```
cargo test -p pc-openapi --lib           40 passed (34 pre + 6 R509 new)
cargo test -p pc-http --lib routes::openapi 43 passed (34 pre + 9 R509 new)
cargo check --workspace                  0 errors (171 pre-existing pc-http warnings)
rustfmt 2 changed files                  0 diffs
```

## 设计要点

1. **错误响应按 verb 区分**: 422 仅 POST/PATCH/PUT (有 body), 500 所有 ops (server errors are universal)
2. **`has_request_body: bool` 参数**: 让 build_responses_block 不依赖外部状态, 易测试 (3 个独立 case + 4 个组合 case)
3. **ErrorResponse 是 generic 4xx/5xx**: 401/403/404/409 复用同一个 schema, 而 422 单独用 ValidationErrorList (field-level detail 重要)
4. **`traceId` is nullable**: 服务端可以填 (从 request id 关联), 也可以为 None (legacy 路径)
5. **GET 不含 422**: GET 没有 body, 422 永不触发; 避免误导 API consumer

## V3 真实进度更新

- **R508 末**: ~85% (15 schemas, 24 hints)
- **R509 末**: **~90%** — 18 schemas (3 error shapes) + 32 hints (4 issues/decisions GET item + pipelines POST + routines POST/PATCH + heartbeat POST) + 422/500 错误响应
- **R510+ 待做**: 剩 24 路由 hints (admin / sub-resources / websocket) + pagination query params + pagination cursor schema

## 教训

1. **错误响应是 schema 演化的关键**: 不引入 ValidationError/ErrorResponse, UI client 永远拿不到 422 detail, 必须有 wire format 协议
2. **has_request_body 参数化**: 之前用 `hint.request.is_some()` 内联判断, R509 显式化参数, 测试更易写 (4 个组合 case 都覆盖)
3. **DELETE 不返回 body**: 但仍可能有 500 (server error during delete), 所以 DELETE 也有 500, 只是没 422
4. **500 跨所有 verb**: 服务端 bug 是 universal, 不是"GET 不会 500"

## 下一步 (R510+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R510** | V3 继续: 剩 24 路由 hints + pagination cursor schema | V3 90% → 95% |
| **R511** | V5 Auth: refresh token rotation (30d sliding) | V5 55% → 70% |
| **R512** | V6 路由补全: companies 子路由 5 个 | V6 86% → 95% |
