# R505 — 5 个 DTO schemas 接入 `pc-http::routes::openapi` (`/openapi.json` + `/openapi.yaml`)

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R505 路线图。
> 目标: 把 R504 定义的 5 个 DTO schemas 实际注入 `/openapi.json` 与 `/openapi.yaml` 响应, 让 UI client 可以拉真实 schemas 生成 TS 类型。

## 改动

### 1. `crates/pc-openapi/src/builder.rs` — 新增 `register_schema_value`

**新增 `OpenApiRegistry::register_schema_value(name, Value) -> &mut Self`**:
- 接受原始 `serde_json::Value`, 绕过 `SchemaRef::Inline` 的 `#[serde(flatten)]` 序列化陷阱
- `#[serde(flatten)]` 对 `serde_json::Value` 字段不可靠: 它会丢 `type` 和 `required` 这类与 serde 内部名字冲突的 key
- 新方法直接把 value 存到 `components.schemas`, serialize 时原样输出

### 2. `crates/pc-openapi/src/schema.rs` — 新增 `SchemaRef::Raw` 变体

```rust
pub enum SchemaRef {
    Named { reference: String },           // {$ref: "..."}
    Inline { schema: Value },              // {flattened keys} — 有 bug
    Raw(#[serde(serialize_with=...)] Value), // {verbatim}
}
```

**新增 `serialize_raw_value` helper**: 用 `serde::Serialize::serialize` 把 Value 原样写出。

**配套更新**:
- `register_schema` 的 match 加 `SchemaRef::Raw(_) => self.components.schemas.insert(n, schema)`
- `builder.rs::tests::schema_ref_refs_helper` 加 `SchemaRef::Raw(_) => panic!` 分支

### 3. `crates/pc-openapi/src/dto_schemas.rs` — 切换到 `register_schema_value`

```rust
pub fn register_core_dtos(reg: &mut OpenApiRegistry) {
    reg.register_schema_value("Decision", decision_schema());
    reg.register_schema_value("Company", company_schema());
    reg.register_schema_value("Issue", issue_schema());
    reg.register_schema_value("Agent", agent_schema());
    reg.register_schema_value("HeartbeatRun", heartbeat_run_schema());
}
```

**修了一个 serialization bug**: 之前用 `register_schema` (走 SchemaRef::Inline + flatten) 时, `type: "object"` 和 `required` 都从 wire format 丢失。R505 修复后 wire 输出完整。

### 4. `crates/pc-http/src/routes/openapi.rs` — `inject_dto_schemas` 注入

**新增纯函数 `inject_dto_schemas(body: &mut Value)`**:
- 内部 build `OpenApiRegistry::builder()` → `register_core_dtos` → `.build().to_json_value()`
- 提取 `components.schemas` 节点, merge 到传入的 `body["components"]`
- 不动 `body["components"]["securitySchemes"]` (hand-rolled path scan 的责任)

**`build_openapi_body` 在末尾调 `inject_dto_schemas(&mut body)`**:
- 单一 source-of-truth: pc-openapi 拥有 schemas, pc-http 拥有 paths/security
- 注入是 idempotent (重跑不破坏已有结构)

### 5. `crates/pc-http/Cargo.toml` — 加 pc-openapi dep

`pc-openapi = { path = "../pc-openapi" }` — 让 pc-http 能调 `pc_openapi::OpenApiRegistry` + `register_core_dtos`。

## 测试 (5 个 R505 新测试)

| 测试 | 验证 |
|---|---|
| `r505_core_dto_schemas_present_in_body` | `inject_dto_schemas` 后 `body.components.schemas` 含 5 个 schema |
| `r505_decision_schema_in_body_has_required_fields` | `Decision.required` 含 `id`, `companyId`, `title`, `body`, `options`, `status`, `expiresAt` |
| `r505_company_schema_preserves_status_enum` | `Company.properties.status.enum` 含 `active`/`paused`/`archived` |
| `r505_security_schemes_coexist_with_schemas` | `securitySchemes` 与 `schemas` 在同一 `components` 对象下并存 |
| `r505_yaml_body_also_contains_schemas` | YAML 序列化包含 5 个 schema 名 + securitySchemes |

## 验证

```
cargo test -p pc-openapi --lib           23 passed (含 SchemaRef::Raw 新测试)
cargo test -p pc-http --lib routes::openapi  14 passed (3 pre + 6 R503 + 5 R505)
cargo check --workspace                  0 errors (169 warnings 全是 pre-existing pc-http dead_code)
rustfmt 4 个改动文件                     0 diffs
```

## 设计要点 (高内聚低耦合)

1. **`SchemaRef::Raw` 而不是改 `SchemaRef::Inline`**: 保留 `Inline` 的 flatten 行为 (其他 caller 可能依赖), `Raw` 是 opt-in bypass
2. **`inject_dto_schemas` 是纯函数**: 不依赖 AppState, 测试无需 mock; 真实 `build_openapi_body` 在末尾一行调用
3. **hand-rolled + pc-openapi 混合**: paths 来自扫描 `crates/pc-http/src/routes/*.rs` (R503 已建), schemas 来自 `register_core_dtos` (R504 已建), 各管各的 key, 不冲突
4. **修了一个 serialization bug**: 之前 wire 格式丢 `type` / `required`, 现在完整

## 教训

1. **`#[serde(flatten)]` on `serde_json::Value` 是反模式**: serde flatten 需要 struct fields 来 flatten, 对 `Value` 不可靠。R505 引入 `SchemaRef::Raw` 作为旁路
2. **测试要测 wire format, 不是测试 helper**: pc-openapi 的 `r504_schemas_serialize_to_openapi_3_1` 只断言 schema name 出现, 没断言 `required` 在场, 所以 bug 没被发现。R505 加 `r505_decision_schema_in_body_has_required_fields` 才暴露
3. **Pure function 利于测试**: `inject_dto_schemas(&mut Value)` 不需要 AppState, 5 个测试都是 ms 级
4. **disk space 是真问题**: cargo incremental 编译巨大, target 30GB+, 一次 disk full 让 cargo build 中断, 留下 corrupt target dir 引发后续编译失败. 必须 `rm -rf target` 重来

## V3 真实进度更新

- **R503 末**: ~50% (serializers + YAML + 3.1.0 + 701 .route() 扫描)
- **R504 末**: ~55% (5 个 DTO schemas 定义在 pc-openapi, 未接入路由)
- **R505 末**: **~65%** — schemas 真正进入 `/openapi.json` 和 `/openapi.yaml` 响应体
- **R506+ 待做**: path-level requestBody + responses (56 主路由覆盖), per-path schemas 关联

## 下一步 (R506+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R506** | V3 继续: path-level request/response schemas (覆盖 56 主路由最常用 10 个) | V3 65% → 75% |
| **R507** | V5 Auth: refresh token rotation | V5 55% → 70% |
| **R508** | V6 路由补全: companies 子路由 5 个 | V6 86% → 95% |
