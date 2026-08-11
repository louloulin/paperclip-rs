# R606 — Gemini-local adapter config_schema 模块

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

把 `pc-adapter-gemini-local` 从 4 模块 / 21 测试 推进到 5 模块 / 26 测试。

新模块 `config_schema.rs` 对齐 Node
`packages/adapters/gemini-local/src/server/config-schema.ts`：

- 6 字段 UI schema: `engine` / `agentCommand` / `mode` /
  `nonInteractivePermissions` / `stateDir` / `warmHandleIdleMs`
- 4 个 ACP 字段携带 `visible_when` meta（`engine ∈ {acp, auto}` 才显示）

## 2. 关键设计

1. **enum FieldType**（Select / Number / Text）代替字符串 — 编译期检查
2. **ConfigField + ConfigOption + FieldMeta + VisibleWhen** 4 个结构体
   与 Node TypeScript 字段一一对应（serde rename_all = "snake_case"）
3. **`acp_visible()` 工厂**：所有非 `engine` 字段统一挂 `acp_visible()` meta
4. **默认值常量**：`DEFAULT_ACP_ENGINE_*` 三个常量与 schema 默认值一致

## 3. 测试

```
$ cargo test -p pc-adapter-gemini-local --lib
test result: ok. 26 passed; 0 failed   (从 21 → 26，新增 5)
```

新增覆盖（5 个）：
- `schema_has_six_fields` — schema 长度 == 6
- `engine_field_has_three_options` — engine 选项 = {auto, cli, acp}，无 meta
- `acp_fields_carry_visible_when_meta` — 5 个 ACP 字段全部带 visible_when
- `defaults_align_with_constants` — 3 个默认值与常量一致
- `schema_serializes_to_json` — JSON 序列化所有字段有 key/label/type

## 4. 模块拆分（gemini-local R606 末）

| 模块 | 行数 | 职责 |
|---|---|---|
| `config_schema.rs` | 218 | UI schema (本轮) |
| `execute_helpers.rs` | 94 | env 构建 + billing type + skills home |
| `gemini_stream_json.rs` | 453 | JSONL 流解析 + 错误分类 |
| `skills.rs` | 490 | skills 快照 |
| `lib.rs` | 275 | Adapter execute 整合 |

## 5. 整体架构现状（R606 末）

| Adapter | Rust 子模块数 | Rust 测试数 | Node execute 行数 |
|---|---|---|---|
| hermes | 9 | 79 | 596 |
| hermes-gateway | 4 | 25 | 959 |
| claude-local | 6 | ~80 | 1270 |
| codex-local | 13 | ~140 | 1504 |
| **gemini-local** | **5** | **26** | 759 |
| opencode-local | 5 | 39 | 720 |
| grok-local | 5 | 38 | 588 |
| cursor-local | 4 | ~40 | 763 |
| pi-local | 4 | ~40 | 847 |
| cursor-cloud | 1 | (stub) | **611** |
| openclaw-gateway | 1 | (stub) | **1491** |

## 6. 整体进度更新

| 域 | R605 末 | R606 末 |
|---|---|---|
| shared/ 契约 | 85% | 85% |
| server/ 路由 | 92% | 92% |
| server/ middleware | 60% | 60% |
| server/ services | 58% | 58% |
| server/ repos | 85% | 85% |
| UI client | 35% | 35% |
| CLI | 60% | 60% |
| 验证层 | 45% | 45% |
| **Adapters** | **83%** | **84%** ↑ |
| **总计** | **~89%** | **~89.5%** ↑ |

workspace lib tests passing: ~7,105+

## 7. R607+ 计划

| 优先级 | Adapter | Node 行数 | 计划 |
|---|---|---|---|
| P1 | cursor-cloud | 611 + 186 + 67 | R607 多 round（云端 SDK → Rust HTTP trait + fake server） |
| P1 | openclaw-gateway | 1491 | R608-R609 多 round（最大 stub） |
| P2 | Architecture: AdapterEnvironmentCheck 提取到 pc-acpx | — | R607.5 重构 |
| P2 | G8 quota.ts 完整复刻 | — | R610+ |
| P2 | G9 plugin-host Node SDK 互操作 | — | R611+ |
