## ADDED Requirements

### Requirement: 领域实体以 Rust 强类型表达 SHALL
The system SHALL satisfy the following behavior.

Paperclip 后端涉及 Company/Agent/Issue/Case/Project/Approval/Decision/Routine/Pipeline/Environment/ExecutionWorkspace/HeartbeatRun 等核心实体，所有实体在 `pc-core` crate 中以 Rust `pub struct` 表达，构造器 `new()` 在不变量违反时返回 `Result<Self, CoreError>`。

#### Scenario: 构造合法实体
- **WHEN** 调用 `Agent::new(config)` 且 config 满足所有不变量（name 非空、adapter 在注册表中、max_concurrent_runs ≥ 1）
- **THEN** 返回 `Ok(Agent)`，字段填充且时间戳为 `now()`

#### Scenario: 拒绝非法实体
- **WHEN** 调用 `Agent::new(config)` 且 `name == ""`
- **THEN** 返回 `Err(CoreError::EmptyName)`

### Requirement: 实体之间通过 ID 而非引用关联 SHALL
The system SHALL satisfy the following behavior.

所有跨实体关系使用 `Uuid` ID（newtype `Id<T>`）而非 `&T` 引用，避免 crate 间循环依赖；仓储层负责 JOIN 查询。

#### Scenario: Issue 通过 company_id 关联 Company
- **WHEN** 创建 `Issue::new(company_id, ...)`
- **THEN** `company_id: Id<Company>`，仓储查询时执行 SQL JOIN

### Requirement: 时间戳统一为 UTC with timezone SHALL
The system SHALL satisfy the following behavior.

所有时间字段使用 `chrono::DateTime<Utc>`；DB 列定义为 `timestamp with time zone`。

#### Scenario: 时间戳序列化
- **WHEN** 实体序列化为 JSON
- **THEN** 时间戳以 RFC 3339 + `Z` 后缀输出
