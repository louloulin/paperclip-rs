## ADDED Requirements

### Requirement: AdapterRuntime trait 统一 11 个内置适配器 SHALL
The system SHALL satisfy the following behavior.

`pc-adapter-api` 定义 `AdapterRuntime` trait；11 个 `pc-adapter-*` crate 实现该 trait，每个等价于 `packages/adapters/<name>/src/server/index.ts` 的 host 行为。

#### Scenario: 列举模型
- **WHEN** 调用 `adapter.list_models(env)`
- **THEN** 返回模型列表（含 id、displayName、contextWindow、cost）

#### Scenario: 探测环境
- **WHEN** 调用 `adapter.test_environment(env)`
- **THEN** 返回 `{status: "ok" | "warning" | "error", checks: [...]}`
