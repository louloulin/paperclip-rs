# R511 — Activity POST route + PluginEventBus wire（97.59% → 97.76%）

## 目标

补 1 个真缺漏 route（`POST /api/companies/:company_id/activity`）并把 `PluginEventBus` 接入主流程，让业务事件真正触发 plugin 订阅者。

## 改动清单

### 1. `crates/pc-http/src/routes/companies.rs` (+441 行)

#### 新增结构（Node schema 适配层）
- `CreateActivityBody` — 接受 Node 形状 input（`actorType/actorId/action/entityType/entityId/agentId/details`）
- `ActivityListQuery` — GET 列表查询参数

#### 新增辅助函数
- `parse_activity_kind(action: &str) -> ActivityKind` — Node free-form action string → 22 个 typed `ActivityKind` 映射，未知 action 落回 `Other`
- `convert_to_activity_event(company_id, body) -> Result<ActivityEvent, ApiError>` — Node-shaped input → 强类型 `ActivityEvent` 转换
- `convert_to_plugin_event(event) -> PluginEvent` — `ActivityEvent` → `PluginEvent`（plugin bus envelope）

#### 新增 route handler
- `create_activity` — `POST /api/companies/:company_id/activity`
  - 3 步 fanout：
    1. `state.activity.emit(event)` → ActivityLog（持久化）
    2. `state.plugin_event_bus.emit(plugin_event)` → PluginEventBus（plugin 订阅者）
    3. `state.realtime.publish(LiveEvent)` → UI 实时更新

#### 复用现有 route
- 修改 `companies.rs:197` 的现有 `GET /api/companies/:company_id/activity` route
- 改为 `get(list_company_activity_route).post(create_activity)` — 同一路径两个 verb

#### 新增测试模块（15 tests）
- `parse_activity_kind_maps_known_actions` / `falls_back_to_other`
- `create_activity_body_parses_camel_case` / `defaults_actor_type_to_system`
- `activity_list_query_parses_camel_case` / `all_optional`
- `convert_to_activity_event_maps_system_actor` / `maps_user_actor_with_uuid` / `rejects_invalid_entity_uuid` / `rejects_invalid_user_actor_uuid` / `preserves_unknown_action_in_payload` / `merges_agent_id_and_details`
- `convert_to_plugin_event_maps_user_actor` / `maps_anonymous_actor` / `maps_system_actor`

### 2. `crates/pc-http/src/state.rs` (166b1e8 已提交)
- AppState 新增 `plugin_event_bus: Arc<PluginEventBus>` 字段
- `AppState::new()` 用 `Arc::new(PluginEventBus::new())` 默认初始化
- 与 `plugin_bus: Arc<NotificationBus>`（worker→host）形成对称

## 验证

| 验证项 | 结果 |
|---|---|
| `cargo test -p pc-http --lib` | **274/274 passed**（259 → 274，新增 15） |
| `cargo build -p pc-server` | Finished, 0 errors |
| `bash scripts/e2e-full-stack.sh` | **17/17 passed** (5.6s) |
| 路由覆盖率 | **97.59% → 97.76%**（missing 14 → 13） |

## 关键设计决策

### 1. 强类型 vs free-form
Node schema 用 free-form `action: string`，Rust 用 typed `ActivityKind` enum（22 种）。
- 已知 action → 映射到 enum（类型安全）
- 未知 action → `ActivityKind::Other` + 原始 string 保留在 `payload.action`
- 后续 plugin handler 可用 typed kind 做 pattern matching

### 2. Actor 适配
Node 用 `actorType: string` + `actorId: string`，Rust 用结构化 `ActivityActor` enum：
- `"user"` → `User { id, name }`（name 暂用 actorId，待后续接 user repo）
- `"agent"` → `Agent { id, name }`
- `"system"` → `System { component }`
- `"plugin"` → `Plugin { plugin_id, plugin_key }`
- 未知 type → `System { component: actorId }`（fallback）

### 3. 路由冲突处理
原 `companies.rs:197` 已有 `GET /api/companies/:company_id/activity`（DB-backed `list_company_activity_route`）。
发现冲突后**不重写**现有 handler，而是用 `get(...).post(create_activity)` 组合，让两个 verb 共存。
- GET：DB-backed，UI 列表查询（已有）
- POST：in-memory ActivityLog + PluginEventBus emit（新加）

### 4. Dead code 清理
初次实现写了 `list_activity` handler（in-memory 版），与现有 `list_company_activity_route` 功能重复。
移除 dead code（33 行）— 遵循"高内聚低耦合"原则，避免同一功能多份实现。

## 与 Node paperclip 的设计哲学差异

Rust paperclip-rs 在 R511 引入的额外安全/质量属性：
1. **类型安全 event 类型** — Node free-form string vs Rust 22 个 typed `ActivityKind`
2. **结构化 Actor** — Node `{type, id}` vs Rust enum with structured fields
3. **未知 action 透明保留** — 不丢失信息，向后兼容
4. **3 步 fanout 单点定义** — 业务事件 → 日志 + plugin + realtime 在一个 handler 内完成

## 提交

```
5222d67 feat(M25-activity-routes): 补 POST activity + wire PluginEventBus（97.59% → 97.76%）
 3 files changed, 450 insertions(+), 16 deletions(-)
```
