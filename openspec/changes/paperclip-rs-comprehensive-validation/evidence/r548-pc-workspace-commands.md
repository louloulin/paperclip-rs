# R548 — pc-workspace-commands（Node workspace-commands.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/workspace-commands.ts` (208 LOC) 完整复刻到新 crate
`crates/pc-workspace-commands`。workspace crates 89 → **90**。

## 设计原则

### 1. 强类型 enum 替代字符串字面量
- Node 用 `"service" | "job"`（kind）、`"shared" | "ephemeral"`（lifecycle）
- Rust 用 `enum WorkspaceCommandKind` / `WorkspaceCommandLifecycle` / `WorkspaceCommandSourceKey`
- 每个 enum 提供 `as_str()` 输出字符串，保持 wire-compat

### 2. 输入 `&serde_json::Value`，输出 typed structs
- Node 输入是 `Record<string, unknown>`，输出是 `WorkspaceCommandDefinition` 接口
- Rust 镜像：`fn list_*(Option<&Value>) -> Vec<WorkspaceCommandDefinition>`
- `Value` 是 serde_json 标准类型，业务侧自由切换

### 3. Helper 复用 + 防御性编程
- `read_non_empty_string` / `slugify` / `derive_workspace_command_id` 都是纯函数
- 接受 `Option<&str>` / `Option<&Value>`，None 自动返回 None / 不抛 panic
- `read_command_entries` 自动过滤非对象元素

### 4. dedup 算法透明
- `unique_workspace_command_id` 用 `HashSet<String>` 跟踪已用 id
- 冲突时 fallback 为 `<id>-<source>-<index+1>`，与 Node 完全等价
- 算法可读、可测、不依赖任何全局状态

### 5. service matching 评分透明
- `score_workspace_runtime_service_match` 返回 `i32`：
  - `-1`：command 不匹配（硬性排除）
  - `100`：configIndex 完全匹配（最强）
  - `0..=10`：软匹配累加（name +4 / command +4 / cwd +2）
- 与 Node `scoreWorkspaceRuntimeServiceMatch` 行为完全等价

### 6. `match_workspace_runtime_service_to_command` 直接返回 `Option<usize>` index
- Node 返回 `T | null`（对象）
- Rust 直接返回 `Option<usize>`（切片 index）
- 调用方通过 `&runtime_services[idx]` 访问原对象，避免 clone

## 公开 API

```rust
pub enum WorkspaceCommandKind { Service, Job }
impl WorkspaceCommandKind { pub fn as_str(self) -> &'static str }

pub enum WorkspaceCommandLifecycle { Shared, Ephemeral }
impl WorkspaceCommandLifecycle { pub fn as_str(self) -> &'static str }

pub enum WorkspaceCommandSourceKey { Commands, Services, Jobs }
impl WorkspaceCommandSourceKey { pub fn as_str(self) -> &'static str }

pub struct WorkspaceCommandSource { kind: WorkspaceCommandSourceKey, index: usize }

pub struct WorkspaceCommandDefinition {
    pub id: String,
    pub name: String,
    pub kind: WorkspaceCommandKind,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub lifecycle: Option<WorkspaceCommandLifecycle>,
    pub service_index: Option<usize>,
    pub disabled_reason: Option<String>,
    pub raw_config: serde_json::Value,
    pub source: WorkspaceCommandSource,
}

pub struct WorkspaceRuntimeServiceMatchInput {
    pub config_index: Option<usize>,
    pub service_name: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub id: String,
}

pub fn list_workspace_command_definitions(workspace_runtime: Option<&serde_json::Value>) -> Vec<WorkspaceCommandDefinition>
pub fn list_workspace_service_command_definitions(workspace_runtime: Option<&serde_json::Value>) -> Vec<WorkspaceCommandDefinition>
pub fn find_workspace_command_definition(workspace_runtime: Option<&serde_json::Value>, workspace_command_id: Option<&str>) -> Option<WorkspaceCommandDefinition>
pub fn score_workspace_runtime_service_match(command: &WorkspaceCommandDefinition, runtime_service: &WorkspaceRuntimeServiceMatchInput) -> i32
pub fn match_workspace_runtime_service_to_command(command: &WorkspaceCommandDefinition, runtime_services: Option<&[WorkspaceRuntimeServiceMatchInput]>) -> Option<usize>
```

## 与上游 Node 差异

- **snake_case**：Rust 命名习惯（`service_index` / `disabled_reason`）
- **usize 替代 number**：index 类型严格
- **Option<usize> index**：`match_*` 函数返回切片下标（Node 返回对象）
- **`&serde_json::Value`**：替代 `Record<string, unknown>`，业务侧可换成 `simd-json` / `napi` 等

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-workspace-commands` | **27 passed** (0 internal + 27 integration) |
| `cargo fmt -p pc-workspace-commands` | ✅ 通过 |
| `cargo clippy -p pc-workspace-commands --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（27 个集成）

- **command-first runtime** (3): 服务+任务解析 / legacy fallback / 空输入
- **entries 过滤** (2): 非对象过滤 / service_index 自增
- **lifecycle** (2): service ephemeral / job 永远 null
- **name 解析** (2): label/title fallback / 默认名 fallback
- **id 唯一性** (1): 重复 id 自动 dedup
- **字符串 trim** (1): 前后空格处理
- **disabled_reason** (1): 透传
- **list_services** (1): 过滤 job
- **find_*** (3): 找到 / 未找到 / blank id
- **match_*** (6): service_index / 拒绝 mismatched command / name+command+cwd / 拒绝零分 / 选最高分 / 空列表
- **score_*** (3): 命令不匹配返回 -1 / index 匹配返回 100 / index 不匹配返回 -1

## 集成待办（不在本轮范围）

- `pc-project`：用 `list_workspace_command_definitions` 解析 `projectWorkspaceRuntime` 配置
- `pc-routines`：消费 `find_workspace_command_definition` 解析 routine 启动参数
- `pc-run-liveness`：用 `match_workspace_runtime_service_to_command` 关联服务↔定义
- `pc-adapter-*`：用 `score_workspace_runtime_service_match` 选择健康服务实例
- 端到端：mock runtime config → 启动 → match → 验证服务正确映射
