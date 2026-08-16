# R694 / UI-1 Evidence — OpenAPI → TS Client Types 闭环

**日期**: 2026-08-16  
**Round**: R694 (UI-1 收尾)  
**Status**: ✅ 完成

## 目标

把 pc-http 注册的 757 routes + 52 component schemas 自动生成 TypeScript 客户端类型,供 `ui/src/api/client.ts` 实际消费,打通 Rust → UI 类型链条。

## 1. 缺口发现

openapi.json dump 后(689 paths / 41 schemas),`openapi-typescript` 报 11 处 `$ref` 解析失败:

```
Can't resolve $ref at #/paths/~1api~1health/get/responses/200/content/application~1json/schema
Can't resolve $ref at #/paths/~1api~1auth~1get-session/get/responses/200/content/application~1json/schema
Can't resolve $ref at #/paths/~1api~1auth~1profile/get/responses/200/content/application~1json/schema
Can't resolve $ref at #/paths/~1api~1auth~1profile/patch/requestBody/content/application~1json/schema
Can't resolve $ref at #/paths/~1api~1companies~1{company_id}~1audit~1agent-actions.csv/get/responses/200/content/application~1json/schema
Can't resolve $ref at #/paths/~1api~1companies~1{company_id}~1events~1ws/get/responses/200/content/application~1json/schema
Can't resolve $ref at #/paths/~1api~1health~1dev-server~1restart/get/responses/200/content/application~1json/schema
Can't resolve $ref at #/paths/~1api~1issues~1{issue_id}~1file-resources~1content/get/responses/200/content/application~1json/schema
Can't resolve $ref at #/paths/~1api~1plugins~1{plugin_id}~1bridge~1stream~1{channel}/get/responses/200/content/application~1json/schema
```

根因:R577 UI client paths 在 `pc-http::routes::openapi` 引用了 11 个 schema 名(`Health`, `Session`, `UserProfile`, `UserProfileUpdate`, `DevServerRestart`, `JsSource`, `AssetContent`, `FileResourceContent`, `BridgeStream`, `CsvExport`, `LiveEventStream`),但 `pc-openapi::dto_schemas::register_core_dtos` 只注册了 41 个 schemas,**没有这些**。

## 2. 修复 (R694)

在 `crates/pc-openapi/src/dto_schemas.rs` 中:

- 新增 11 个 schema helper 函数 (与现有 `pipeline_schema()` 等一致风格):
  - `health_schema()` — `/api/health` 返回 health snapshot
  - `dev_server_restart_schema()` — `/api/health/dev-server/restart` 确认
  - `session_schema()` — `/api/auth/get-session` 会话 payload
  - `user_profile_schema()` — `/api/auth/profile` 用户档案
  - `user_profile_update_schema()` — `PATCH /api/auth/profile` body
  - `js_source_schema()` — `/api/adapters/{type}/ui-parser.js` JS source
  - `asset_content_schema()` — `/api/assets/{asset_id}/content` 资产 envelope
  - `file_resource_content_schema()` — issue file-resources envelope
  - `bridge_stream_schema()` — plugin bridge WebSocket frame
  - `csv_export_schema()` — CSV 导出 envelope
  - `live_event_stream_schema()` — company events WebSocket frame
- 在 `register_core_dtos` 中注册 11 个新 schema
- 在 `CORE_DTO_NAMES` 数组添加 11 个新名字
- 调整 8 处 schema_count 断言 `41 → 52` (R507/R508/R509/R510/R511/R513 测试)
- 调整 pc-http R522 测试断言 `41 → 52`
- 新增 13 个 R694 测试 (验证每个新 schema 的 required 字段和 enum 限制)

总计文件增长 1763 → 2136 行 (+373 行)。

## 3. 验证

### 3.1 pc-openapi 单元测试

```
cargo test -p pc-openapi
test result: ok. 79 passed; 0 failed
```

增量: 66 → 79 (+13 R694 tests), 0 fail。

### 3.2 pc-http lib 测试

```
cargo test -p pc-http --lib
test result: ok. 495 passed; 0 failed
```

0 regression。

### 3.3 UI-1 dump 集成测试

```
PAPERCLIP_DUMP_OPENAPI=1 cargo test -p pc-http --test ui1_openapi_dump_contract
test ui1_openapi_dump_path_count_meets_threshold ... ok
test ui1_openapi_dump_operation_ids_are_unique ... ok
test ui1_openapi_dump_has_top_level_keys ... ok
test ui1_openapi_dump_writes_to_well_known_path ... ok
test result: ok. 4 passed; 0 failed
```

### 3.4 OpenAPI ref 解析闭环

重新生成 `openapi.json` 后,扫描 `$ref`:

```
Total refs: 49 / Schemas: 52 / Missing: 0
```

0 missing refs。

### 3.5 openapi-typescript 生成

```
node scripts/generate-ui-types.mjs
[generate-ui-types] openapi.json -> paths=689 schemas=52
[generate-ui-types] invoking openapi-typescript CLI ...
✨ openapi-typescript 7.13.0
🚀 /Users/louloulin/.../openapi.json → /Users/louloulin/.../ui-types/openapi-schema.d.ts
[generate-ui-types] wrote ui-types/openapi-schema.d.ts (1444499 bytes)
```

- 689 paths → 49,871 行 TS
- 1.44 MB
- 0 个解析错误 (之前是 11 个 `$ref` 错误)

### 3.6 TypeScript strict 验证

写了一个 `.tmp/check_dts.ts`,import 11 个新 schema 类型并构造值:

```typescript
import { components, operations } from '../ui-types/openapi-schema';
type Health = components['schemas']['Health'];
const _h: Health = { status: 'ok', version: '1', uptime: 1.0 };
// ... 10 more
```

```
tsc --noEmit --strict --skipLibCheck --ignoreConfig check_dts.ts
exit=0
```

TypeScript strict 模式 0 错误。生成的类型完整可消费。

## 4. 工具链

### `scripts/generate-ui-types.mjs` (新增 49 行)

```javascript
// 读取 openapi.json, 验证 paths/schemas 计数, 调用 openapi-typescript CLI
// 输出 ui-types/openapi-schema.d.ts, 报告生成字节数
```

### `package.json` scripts (新增 3 个)

```json
{
  "scripts": {
    "generate:ui-types": "node scripts/generate-ui-types.mjs",
    "dump:openapi": "PAPERCLIP_DUMP_OPENAPI=1 cargo test -p pc-http --test ui1_openapi_dump_contract ui1_openapi_dump_writes_to_well_known_path -- --nocapture",
    "ui:types": "npm run dump:openapi && npm run generate:ui-types"
  }
}
```

## 5. 关键文件

- `crates/pc-openapi/src/dto_schemas.rs` — 11 个新 schema helper + 13 个新 tests
- `crates/pc-http/src/routes/openapi.rs:2650` — R522 测试断言更新
- `crates/pc-http/src/routes/openapi.rs:834-960` — R577 UI client paths (引用源)
- `crates/pc-http/tests/ui1_openapi_dump_contract.rs` — 4 个 dump 集成测试
- `scripts/generate-ui-types.mjs` — 新生成器脚本
- `package.json` — devDeps + 3 scripts
- `openapi.json` — 689 paths / 52 schemas (816 KB)
- `ui-types/openapi-schema.d.ts` — 49,871 行 (1.44 MB)

## 6. 影响

- **UI-1 完成**: Rust 后端 → TS 客户端类型 链路 100% 通
- **前端可消费**: `ui/src/api/client.ts` 可直接 `import type { operations } from "@/openapi-schema"`
- **类型驱动**: 后续 UI-3 接入会用这些 types 保证 endpoint 一致性
- **0 ref dangling**: redoc 之前报 11 处错误,现在 0
- **0 regression**: pc-openapi 79 + pc-http lib 495 + UI-1 dump 4,共 578 tests 全 PASS

## 7. 后续 (UI-2 / UI-3)

- **UI-2**: 前端路由 ↔ 后端 endpoint 1:1 mapping 核查 (复用 `scripts/check-ui-openapi.sh`)
- **UI-3**: 核心页面 UI 真实连入 (Agent / Pipeline / Environment / Plugin 等)
  - 用 `ui-types/openapi-schema.d.ts` 重构 `ui/src/api/client.ts`
  - 真实启动 `paperclip-server` + curl/UI 调用验证

## 8. 已知无关失败

`crates/pc-http/tests/access_http_contract.rs::board_key_create_persists_real_sha256_hash_and_returns_one_time_token` 失败,token format 从 `pcp_board_xxx` 变成 `pk_xxx`。
**这是预存在的 unrelated bug**,与 R694 改动无关,按用户硬约束 #5 不修复。
