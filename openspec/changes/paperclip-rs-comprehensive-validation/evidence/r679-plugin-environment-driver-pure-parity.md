# R679 — plugin-environment-driver.ts pure function parity

## 目标
将 Node paperclip/server/src/services/plugin-environment-driver.ts (570 行) 中的 pure function 部分 1:1 复刻到 Rust。

## 范围
- 整个文件 570 行含 22 个 export
- pure function: 2 ✅ 已做
- async function（依赖 Db / PluginWorkerManager）: 17 个（推迟，待依赖下沉）
- interface type: 2
- 内部 helper: 1

## 复刻内容
### 1) 常量（1:1 镜像）
- RPC_OVERHEAD_BUFFER_MS = 30_000
- DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS = 2_000

### 2) pluginDriverProviderKey
- 顺序 pluginKey:driverKey，分隔符半角冒号
- 不做 trim / normalize / 大小写处理
- 空字符串产出 :（与 Node 一致）

### 3) resolvePluginExecuteRpcTimeoutMs
Node 优先级规则：
1. requestedTimeoutMs 是有限数 > 0 → trunc(requestedTimeoutMs)
2. 否则 config["timeoutMs"] 是有限数 > 0 → trunc(config["timeoutMs"])
3. 否则 None
4. 返回 Some(baseMs + 30_000) 或 None

Rust 实现：
- requested_timeout_ms: Option<f64>（None = undefined）
- config: &serde_json::Value（保留动态结构）
- f64::is_finite() / trunc() as u64
- saturating_add 防溢出
- 字符串 / null / 负数 / NaN / ±Infinity 全部 fallback 到 None

## 测试覆盖（24 个 case）
### provider_key 覆盖（4 个）
- basic, uuid-like, 空字符串, 含冒号

### 常量覆盖（1 个）
- 30_000 / 2_000 精确相等

### resolve timeout 覆盖（19 个）
- requested 各路径（>0, =0, <0, None, NaN, ±Infinity, 0.0001, u64::MAX, 优先于 config）
- config 各路径（无, =0, <0, string, null, float trunc, u64, 含额外 keys）

## 真实验证
### 编译
cargo test -p pc-environment --test plugin_environment_driver_pure_tests → 0 errors / 4 warnings（pre-existing）

### 运行
test result: ok. 24 passed; 0 failed

### 全 pc-environment 套件回归
- lib: 7 ✅
- config_tests: 44 ✅
- custom_image_runtime_tests: 41 ✅
- custom_image_terminal_sessions_tests: 35 ✅
- e2e_environment_service: 3 ✅
- environment_hook_contract: 3 ✅
- plugin_environment_driver_pure_tests (R679): 24 ✅
- 合计 157, 0 fail

### pc-plugin-database 回归
- 47 / 47 PASS（确保 R673 不破）

## 文件改动
- crates/pc-environment/src/plugin_environment_driver_pure.rs (2818 bytes) 新建
- crates/pc-environment/tests/plugin_environment_driver_pure_tests.rs (5599 bytes) 新建
- crates/pc-environment/src/lib.rs (+6 行) 添加 mod + pub use

## 设计要点
### 类型设计
- PluginEnvironmentDriverKey 是独立 pub struct（缩小 Node Pick）
- 完整 PluginEnvironmentConfig 已在 config.rs，不重复定义

### JSON 灵活性
- 接 &serde_json::Value 而非 Map（最接近 Node Record<string, unknown>）
- 严格只读 timeoutMs 字段

### 数值类型
- f64 接 requested_timeout_ms（NaN / Infinity 必须可表达）
- u64 接最终结果（物理超时非负）
- saturating_add 防溢出

### 测试可达性
- mod 私有 + 顶层 pub use → 模块是 crate 内部细节，符号公开 API
- 测试用 pc_environment::{plugin_driver_provider_key, ...} 引用

## 推迟部分（17 async function）
全部依赖 PluginWorkerManager trait + json-schema-secret-refs 下沉。
- R682+ 计划：先在 pc-repos 抽 trait，再分批做 async parity。

## 进度更新
- 核心域覆盖度：99.7% → 99.72%
- 单元测试：6,524+ → 6,548（+24）
- 下一步：R680 = plugin-job-scheduler.ts (752 行) 的 pure 部分 + 类型
