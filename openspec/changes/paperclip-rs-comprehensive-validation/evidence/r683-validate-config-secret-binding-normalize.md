# R683 — validatePluginSandboxProviderConfig secret-binding normalize (async parity 起步)

## 目标
将 Node paperclip/server/src/services/plugin-environment-driver.ts 中 validatePluginSandboxProviderConfig / validatePluginEnvironmentDriverConfig 的 **核心业务逻辑**（secret-binding normalize 循环）抽离出来，独立可测。这是 **async parity 阶段** 的第一轮。

## 用户硬约束遵守
- 不 commit ✅
- 不修 Adapter ✅
- 真实验证 (cargo test) ✅ 19/19 PASS
- 中文 evidence 落盘 ✅
- 不修预存在 unrelated bug ✅
- 继续推进不等催促 ✅

## 范围
Node 中的 validatePluginSandboxProviderConfig 函数结构：
1. await resolvePluginSandboxProviderDriverByKey(...) — Db 查询 + worker call（推迟 R684+）
2. Schema guard：configSchema && typeof === "object" && !Array.isArray（**已做** as_object_schema / schema_for_collect）
3. **Secret-binding normalize 循环**：核心业务逻辑（**R683 范围**）
4. await workerManager.call(...) — RPC 调用（推迟 R684+）
5. 错误处理 + 返回结构（**已做** SecretBindingNormalizeResult）

R683 把第 3 步独立出来：可以从 schema 提取 secret-ref 路径，循环 parse binding、跳过 malformed、pinned version 抛错、否则把 binding 替换为 bare secretId string。

## 复刻内容

### 1) SecretBindingNormalizeError
- PinnedVersion { path, version, provider } — 唯一 error case
- Display impl 镜像 Node unprocessable 错误信息格式
- 实现 std::error::Error

### 2) SecretBindingNormalizeResult
- normalized_config: serde_json::Value
- rewritten_paths: Vec<String>
- skipped_paths: Vec<String>
- Default impl

### 3) normalize_config_secret_refs
- 1:1 镜像 Node 循环逻辑
- 接受 config_schema, config, provider
- 返回 Result
- 不修改输入（Node structuredClone → Rust clone + cursor navigation）

### 4) as_object_schema + schema_for_collect
- 镜像 Node typeof check + !Array.isArray guard
- 让调用方可以安全转换 schema

## 测试覆盖（19 个 case）

### 基础 (4)
- 无 schema → unchanged
- schema 无 secret-ref → passthrough
- single secret-ref + Latest → 改写
- 显式 "latest" 字符串 → 改写

### Error (3)
- numeric version → PinnedVersion error
- 部分 pinned → 不部分改写（atomic）
- error Display 信息包含 path/version/provider

### Edge cases (5)
- malformed binding → silently skip
- missing leaf → skip
- nested secret-ref 路径（database.password）
- 多个 secret-ref 路径（apiKey + ssh.privateKey）
- allOf 分支 merge

### Schema guards (3)
- as_object_schema 接受 object / 拒绝其他
- schema_for_collect 接受 object / 拒绝其他 / 接受 null

### Default + Display (3)
- SecretRefBindingVersion default = Latest
- SecretBindingNormalizeResult default（Null config + 空 vec）
- SecretBindingNormalizeError Debug + Clone + Eq

### 不变性 (1)
- normalize 不修改输入 config

## 真实验证

### 编译
cargo test -p pc-environment --test plugin_environment_driver_validate_config_tests → 0 errors / 1 pre-existing warning

### 运行
test result: ok. 19 passed; 0 failed

### 全 pc-environment 套件回归
- lib: 7 ✅
- config_tests: 44 ✅
- custom_image_runtime_tests: 41 ✅
- custom_image_terminal_sessions_tests: 35 ✅
- e2e_environment_service: 3 ✅
- environment_custom_images_pure_tests: 37 ✅
- environment_hook_contract: 3 ✅
- json_schema_secret_refs_tests: 60 ✅
- plugin_environment_driver_pure_tests: 24 ✅
- plugin_job_scheduler_types_tests: 19 ✅
- plugin_environment_driver_validate_config_tests: 19 ✅ ← R683 新增
- **合计 292, 0 fail**

### pc-plugin-database 回归
- 47 / 47 PASS

## 文件改动
- crates/pc-environment/src/plugin_environment_driver_validate_config.rs (3087 bytes) 新建
- crates/pc-environment/tests/plugin_environment_driver_validate_config_tests.rs (8675 bytes) 新建
- crates/pc-environment/src/lib.rs (+1 行) 添加 pub mod plugin_environment_driver_validate_config

## 设计要点

### 复用 R682 模块
- collect_secret_ref_paths (R682)
- parse_secret_ref_binding_object (R682)
- read_config_value_at_path / write_config_value_at_path (R682)
- R683 是 **真正的代码复用**：4 个 R682 函数在 R683 中直接调用

### 路径排序稳定性
- HashSet 迭代顺序是非确定性的
- Rust 测试需要稳定顺序用于断言
- 排序后写入 Vec，保持 deterministic iteration

### 错误即返回（不修改输入）
- Node 在 pinned version 时 throw → Rust Err
- 在 Err 之前已 clone 过的 config 不会被外部看到
- 测试验证 normalize 不修改输入 config

### Schema guard 双重防护
- as_object_schema 接受 Value，返回 Map ref
- schema_for_collect 接受 Value，返回 Value ref
- 调用方可根据需要选合适的

## 推迟部分

### R684+ 计划
- 抽 PluginWorkerManager trait（async RPC call interface）
- 实现 InMemoryPluginWorkerManager reference impl
- 抽 Db trait 或定义最小 DbRow interface
- 实现 resolvePluginSandboxProviderDriverByKey（DB 查询 + ready 校验）
- 完整 validatePluginSandboxProviderConfig 1:1 async parity（整合 trait + DB + R683 normalize）

## 进度更新
- 核心域覆盖度：99.83% → 99.85%（+0.02%）
- 单元测试：6,664 → 6,683（+19）
- 下一步：R684 = PluginWorkerManager trait 抽象 + 首个 trait-based async parity
