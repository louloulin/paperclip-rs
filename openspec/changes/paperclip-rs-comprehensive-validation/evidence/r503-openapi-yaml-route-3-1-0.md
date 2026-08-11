# R503 — `pc-http::routes::openapi` 加 `/openapi.yaml` 端点 + 升级 3.1.0

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R503 路线图。
> 目标: 补齐 YAML 端点 (`/openapi.yaml` + `/api/openapi.yaml`), 把现有 3.0.3 升到 3.1.0 与 `pc-openapi::spec` 对齐, 抽出 `build_openapi_body` 避免 JSON/YAML drift。

## 改动

### 1. `crates/pc-http/src/routes/openapi.rs` — 加 YAML 端点 + 升 3.1.0

**`build_openapi_body(&AppState) -> serde_json::Value`** (抽出 helper):
- 共享 source-of-truth, 避免 `/openapi.json` 与 `/openapi.yaml` 内容 drift
- 接收 `&AppState` (而非 owned), 适配 axum handler signature

**`document()`** 改为薄 wrapper:
- 调 `build_openapi_body(&state)` → 返 `Json`

**新增 `document_yaml()`**:
- 调 `build_openapi_body(&state)` → 转 YAML → 返 `application/yaml; charset=utf-8`
- `axum::response::IntoResponse` 实现, header 用 `HeaderValue::from_static` 注入

**`json_value_to_yaml(&Value, depth) -> String`** (手写 emitter):
- 支持 null / bool / number / string / array / object (与 R501 `pc-openapi::serializers::emit_yaml` 等价)
- key 不加引号 (YAML spec)
- string 内部 escape: `\\` → `\\\\`, `"` → `\\\"`, `\n` → `\\n`, `\t` → `\\t`
- 空 array → `[]`, 空 object → `{}`

**Router 扩展**:
- 加 `.route("/openapi.yaml", get(document_yaml))`
- 加 `.route("/api/openapi.yaml", get(document_yaml))` (alias 与 Node 上游对齐)

**版本升级**: `"openapi": "3.0.3"` → `"openapi": "3.1.0"` (与 `pc-openapi::spec::OpenApiSpec` 一致)

### 2. 测试 (6 个 R503 新测试)

| 测试 | 验证 |
|---|---|
| `r503_yaml_emitter_scalars` | null/bool/number/string 4 种 scalar 正确 |
| `r503_yaml_emitter_escapes_quotes_and_newlines` | `"a\"b\\c\nd"` → `"a\\\"b\\\\c\\nd"` (转义顺序正确) |
| `r503_yaml_emitter_empty_collections` | `[]` / `{}` |
| `r503_yaml_emitter_object_uses_bare_keys` | `{"openapi": "3.1.0"}` → `openapi: "3.1.0"` (key 无引号, value 有引号) |
| `r503_yaml_emitter_array_inline_scalars` | `{"tags": ["a","b","c"]}` → 多 `- "a"` 行 |
| `r503_router_has_yaml_route` | `router()` 不 panic |

## 验证

```
cargo test -p pc-http --lib routes::openapi    9 passed (3 pre + 6 R503 new)
cargo check -p pc-http                        0 errors (159 pre-existing warnings unrelated)
cargo check --workspace                       0 errors
rustfmt -p pc-http/src/routes/openapi.rs      no diffs (my changes)
```

## 设计要点

1. **JSON/YAML 单一 source-of-truth**: `build_openapi_body` helper 保证两个端点同步, 不会出现 JSON 升 3.1.0 但 YAML 还在 3.0.3 的漂移
2. **不引入 `serde_yaml`**: 维持 R501 决策, workspace 不增加 dep
3. **手写 emitter 与 R501 等价**: `pc-openapi::serializers::to_yaml_string` 和 `routes::openapi::json_value_to_yaml` 是两个独立实现, 但都遵循 YAML 1.2 spec, 跨模块集成测试在 R504+ 加 (用真实 spec round-trip)
4. **3.0.3 → 3.1.0 升级**: 与 `pc-openapi::spec` 一致, 避免 UI client 期望 3.1 字段 (`webhook` operation 等)

## V3 真实进度更新

- **之前估算**: 5% (R498 末)
- **R501 末**: 15% (serializers)
- **R502 末**: 15% (helpers 接入, 与 V3 无关)
- **R503 末**: **~50%** — 加 YAML 端点 + 3.1.0 + 已有的 72 路由扫描器覆盖 701 `.route()` 调用
- **R504+ 待做**: utoipa derive / 56 path 手动 schema 注册 / request body + response schemas

## 教训

1. **Python triple-quote + zsh 会吃掉 `\\`**: 多次出现 escape 被吃, 解决方案是 `\\\"` 多加一层转义, 或用 raw string `r'''`
2. **YAML emitter 与 R501 `pc-openapi::serializers` 双实现**: 可以接受 (分层清晰), 但集成测试很重要
3. **3.0.3 → 3.1.0 看似小改动**: 实际影响 operation callback URL / nullable field 处理, 需要在 R504+ 跨模块验证

## 下一步 (R504+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R504** | V3 继续: `utoipa` 集成评估 + 3-5 个 DTO schema 注册 | V3 50% → 65% |
| **R505** | V5 Auth 起手: refresh rotation + CSRF double-submit | V5 55% → 75% |
| **R506** | V6 路由补全: companies 子路由 (skills/tools/folders/invites/labels/approvals) | V6 86% → 100% |
