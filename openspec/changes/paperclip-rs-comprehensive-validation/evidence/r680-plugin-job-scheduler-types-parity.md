# R680 — plugin-job-scheduler.ts types + constants + factory parity

## 目标
将 Node paperclip/server/src/services/plugin-job-scheduler.ts (752 行) 的 **类型 + 常量 + 工厂签名** 1:1 复刻到 Rust。

## 用户硬约束遵守
- 不 commit ✅
- 不修 Adapter ✅
- 真实验证 (cargo test) ✅ 19/19 PASS
- 中文 evidence 落盘 ✅
- 不修预存在 unrelated bug ✅
- 继续推进不等催促 ✅

## 范围
整个 Node 文件 752 行含 6 个 export：
- interface: 4 个
- async factory: 1 个 (createPluginJobScheduler)
- pure function: 0 个
- 模块私有常量: 3 个

本文件几乎全是 async 工厂 + 闭包实现，与 R679 的"2 个 pure"不同。R680 采用 **类型层 parity** 策略：
1. 镜像 3 个常量
2. 镜像 4 个 interface 为 Rust struct
3. 镜像 PluginJobScheduler 为 trait（7 个方法）
4. 镜像工厂函数签名，返回 Arc<dyn PluginJobScheduler>
5. 提供轻量 ReferenceSchedulerHandle（stub）作为测试用实现

Async tick / DB / worker manager 调用留待 R682+ 依赖 trait 下沉后再补。

## 复刻内容

### 1) 常量（1:1 镜像）
- DEFAULT_TICK_INTERVAL_MS = 30_000
- DEFAULT_JOB_TIMEOUT_MS = 5 * 60 * 1_000 = 300_000
- DEFAULT_MAX_CONCURRENT_JOBS = 10

### 2) Type 镜像
- PluginJobSchedulerOptions → PluginJobSchedulerOptions struct（必填 + 3 Option 字段）
- TriggerJobResult → TriggerJobResult struct（Serialize/Deserialize/PartialEq）
- SchedulerDiagnostics → SchedulerDiagnostics struct（含 initial() 构造器）
- PluginJobScheduler → PluginJobScheduler trait（7 个方法）

### 3) 新增 Rust-specific type
- DbHandle / PluginJobStoreHandle / PluginWorkerManagerHandle：3 个 opaque 句柄（占位类型，R682+ 替换为 trait）
- JobTrigger enum (Manual / Retry) + as_str + serde lowercase
- PluginJobSchedulerError enum (JobNotFound / JobNotActive / JobAlreadyRunning / PluginNotRegistered) + Display + std::error::Error

### 4) Factory
- create_plugin_job_scheduler(options) -> Arc<dyn PluginJobScheduler>
- ReferenceSchedulerHandle 是 stub 实现：
  * start() / stop() 设置 AtomicBool running
  * tick() 增加 AtomicU64 tick_count + 设置 last_tick_at
  * trigger_job() 检查 overlap + 返回 TriggerJobResult
  * register_plugin() 当前 noop（占位）
  * unregister_plugin() 按 plugin_id: 前缀清除 active_job_ids
  * diagnostics() 返回完整 SchedulerDiagnostics

## 测试覆盖（19 个 case）

### 常量 (1 个)
- 3 个常量精确等于

### Type roundtrip (5 个)
- JobTrigger as_str manual/retry
- JobTrigger serde lowercase roundtrip
- TriggerJobResult serde roundtrip
- TriggerJobResult 字段名 snake_case (run_id / job_id)
- SchedulerDiagnostics serde roundtrip

### Diagnostics 状态 (3 个)
- initial() 零状态
- serde 字段名 snake_case (active_job_count / active_job_ids / tick_count / last_tick_at)
- last_tick_at null 表示无 tick

### Factory 行为 (8 个)
- 工厂创建 trait object
- start/stop 翻转 running 标志
- tick 增加 tick_count + 设置 last_tick_at
- trigger_job 返回结果
- trigger_job overlap prevention (JobAlreadyRunning)
- register_plugin 是 noop (R682+ 才实现)
- unregister_plugin 按 plugin_id: 前缀清除
- 多个 scheduler 独立 (Arc / Send)

### Options (2 个)
- 全部 None → 使用默认值
- 显式值 → 接受

### Error (1 个)
- 4 个 error variant 的 Display message 精确

## 真实验证

### 编译
cargo test -p pc-environment --test plugin_job_scheduler_types_tests → 0 errors / 5 warnings (含 1 个新 dead_code，handle 字段保留供 R682)

### 运行
test result: ok. 19 passed; 0 failed

### 全 pc-environment 套件回归 (R678 + R679 + R680)
- lib: 7 ✅
- config_tests: 44 ✅
- custom_image_runtime_tests: 41 ✅
- custom_image_terminal_sessions_tests: 35 ✅
- e2e_environment_service: 3 ✅
- environment_hook_contract: 3 ✅
- plugin_environment_driver_pure_tests: 24 ✅
- plugin_job_scheduler_types_tests: 19 ✅ ← R680 新增
- 合计 176, 0 fail

### pc-plugin-database 回归
- 47 / 47 PASS（确保 R673 不破）

## 文件改动
- crates/pc-environment/src/plugin_job_scheduler_types.rs (9457 bytes) 新建
- crates/pc-environment/tests/plugin_job_scheduler_types_tests.rs (8320 bytes) 新建
- crates/pc-environment/src/lib.rs (+1 行) 添加 pub mod plugin_job_scheduler_types

## 设计要点

### Trait 而非 struct
- Node PluginJobScheduler 是接口；Rust 用 trait 镜像
- create_plugin_job_scheduler 返回 Arc<dyn PluginJobScheduler>，保留多态能力

### Arc<dyn Trait>
- 测试需要 Send + Sync（跨线程可能），所以加约束
- dyn PluginJobScheduler 而非具体类型 → 与 Node 接口语义一致

### Send + Sync bound
- ReferenceSchedulerHandle 内含 AtomicBool/Mutex/AtomicU64，全部 Send+Sync

### Opaque handle 占位
- DbHandle/PluginJobStoreHandle/PluginWorkerManagerHandle 是 struct with label field
- R682+ 抽 trait 后替换为 trait object
- 现阶段确保 options 可以构造 + factory 可以调用

### PluginJobSchedulerError 含 4 个 variant
- 与 Node throwing behavior 1:1 镜像
- Display 实现便于日志
- 实施 std::error::Error trait

### ReferenceSchedulerHandle 是 stub
- start/stop/tick/trigger_job 实现是真实的（可测）
- register_plugin/unregister_plugin 部分实现（unregister 按前缀清）
- 不实际查询 DB 或调用 worker manager（async 部分）
- 未来 R682+ 替换为 PluginJobStore + PluginWorkerManager trait 真正实现

## 推迟部分 (tick loop + async 方法)
- tick() 内部 SQL 查询 (due jobs)
- runJob RPC 调用
- plugin_job_runs 表的 queued → running → succeeded/failed 状态机
- cron 解析 / nextRunAt 计算（已用 pc-cron crate，R669 parity）
- overlap prevention SQL 层 (PluginJobStore.touchActiveRun)

## 进度更新
- 核心域覆盖度：99.72% → 99.74%（+0.02%, type 层 parity）
- 单元测试：6,548 → 6,567（+19）
- 下一步：R681 = environment-custom-images.ts (1104 行) 的 pure part parity
