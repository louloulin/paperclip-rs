# R682 — json-schema-secret-refs.ts 100%% pure parity

## 目标
将 Node paperclip/server/src/services/json-schema-secret-refs.ts (104 行) 的 **全部 5 个 pure export** 1:1 复刻到 Rust。这是 R682 async parity 推进的关键前置 —— 该模块被多个 async service 依赖。

## 用户硬约束遵守
- 不 commit ✅
- 不修 Adapter ✅
- 真实验证 (cargo test) ✅ 60/60 PASS
- 中文 evidence 落盘 ✅
- 不修预存在 unrelated bug ✅
- 继续推进不等催促 ✅

## 范围
整个 Node 文件 104 行，**100%% pure** —— 5 个 export 全部是纯函数 + 1 个 type alias。
- isUuidSecretRef(value) — UUID 校验
- SecretRefBindingObject — type alias
- parseSecretRefBindingObject(value) — 解析 secret ref binding
- collectSecretRefPaths(schema) — DFS 找 secret-ref 字段
- readConfigValueAtPath(config, path) — dot-path 读取
- writeConfigValueAtPath(config, path, value) — dot-path 写入 (deep clone)

## 复刻内容

### 1) isUuidSecretRef
- Node: `/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i` (regex)
- Rust: 手工实现（无 regex crate 依赖）
  - 长度 == 36
  - 4 个 dash 位置在 8/13/18/23
  - 其他位置是 hex digit (case-insensitive)

### 2) SecretRefBindingObject + SecretRefBindingVersion
- 镜像 Node type alias
- SecretRefBindingVersion: Latest / Number(u64) (untagged enum)
- #[serde(default)] 让 version 字段在缺省时为 Latest

### 3) parseSecretRefBindingObject
- 校验 type === "secret_ref"
- 校验 secretId 是合法 UUID (trim 后)
- version 处理:
  - undefined/null → Latest
  - "latest" 字符串 → Latest
  - 正整数 → Number(u64)
  - 其它 (0, 负数, 浮点, 其它字符串) → None

### 4) collectSecretRefPaths
- DFS 遍历 JSON schema
- 处理 allOf / anyOf / oneOf 分支（merge 路径）
- 找到 properties.<key>.format === "secret-ref"
- 返回路径集合（用 . 分隔）
- 路径插入时也递归下钻（嵌套属性）

### 5) readConfigValueAtPath
- 沿 dotPath.split(".") 逐层下钻
- 非对象中途遇到 → 返回 None

### 6) writeConfigValueAtPath
- 镜像 Node 实现：
  - structuredClone(config) 深拷贝
  - 逐层下钻创建缺失的 {}
  - 写入 leaf (None → delete key, Some(v) → insert)
- Rust: serde_json::Value 的 clone + 手动 cursor 导航

## 测试覆盖（60 个 case）

### isUuidSecretRef (8)
- 正常 lower / upper / mixed case / 空 / 太短 / 非 hex / 无 dash / 多余字符

### parseSecretRefBindingObject (15)
- basic + 显式 latest + null version + numeric version
- wrong type / missing type / bad uuid / non-string secretId
- zero / negative / non-integer / string version (非 latest)
- null / string / array input 拒绝
- 带空白 UUID 接受（trim 后通过）

### collectSecretRefPaths (12)
- null / 空对象 / 无 properties / 单字段 / 嵌套 / 深层
- 同级多个 / allOf / anyOf / oneOf merge
- 非 secret-ref format 忽略 / 非对象输入 / property value 非对象

### readConfigValueAtPath (9)
- 顶层 / 嵌套 / missing key / missing nested
- 数组中途返回 None / 字符串中途返回 None / null leaf / 空路径
- bool 和 number leaf

### writeConfigValueAtPath (11)
- 顶层 / 嵌套新建 / 嵌套存在 / 不修改输入
- None 删除 leaf / 非对象中间节点替换 / 数组中间节点替换
- complex roundtrip / value object pass-through / 空路径行为（写空 key）

### Integration (2)
- parse + read 链式调用
- collect + read + parse 完整链路验证（多个 secret-ref 路径）

### Default impl (1)
- SecretRefBindingVersion::default() == Latest

## 真实验证

### 编译
cargo test -p pc-environment --test json_schema_secret_refs_tests → 0 errors / 5 warnings (4 pre-existing)

### 运行
test result: ok. 60 passed; 0 failed

### 全 pc-environment 套件回归
- lib: 7 ✅
- config_tests: 44 ✅
- custom_image_runtime_tests: 41 ✅
- custom_image_terminal_sessions_tests: 35 ✅
- e2e_environment_service: 3 ✅
- environment_custom_images_pure_tests: 37 ✅
- environment_hook_contract: 3 ✅
- json_schema_secret_refs_tests: 60 ✅ ← R682 新增
- plugin_environment_driver_pure_tests: 24 ✅
- plugin_job_scheduler_types_tests: 19 ✅
- 合计 273, 0 fail

### pc-plugin-database 回归
- 47 / 47 PASS

## 文件改动
- crates/pc-environment/src/json_schema_secret_refs.rs (6023 bytes) 新建
- crates/pc-environment/tests/json_schema_secret_refs_tests.rs (16780 bytes) 新建
- crates/pc-environment/src/lib.rs (+1 行) 添加 pub mod json_schema_secret_refs

## 设计要点

### 手工 UUID 校验 vs regex crate
- pc-environment 的 Cargo.toml 没有 regex / once_cell 依赖
- 用 bytes 迭代 + is_ascii_hexdigit() 实现同样语义
- 性能更好（无 regex 编译开销，无堆分配）
- 代码量更小

### SecretRefBindingVersion::default = Latest
- Node parseSecretRefBindingObject 在 version 缺省时返回 Latest
- Rust 用 #[serde(default)] 让 serde 反序列化也走默认
- Rust struct 不需要 version 字段也可 roundtrip

### walk_schema 递归
- 镜像 Node DFS 语义
- 同一 prefix 在 allOf/anyOf/oneOf 分支间继承
- properties[key] 既是叶节点（如果是 secret-ref）也是父节点（继续下钻）

### write_config_value_at_path cursor 跟踪
- Rust 没有 GC，要手动跟踪 cursor
- 用 `&mut serde_json::Value` 引用实现下钻
- 最终在 cursor 处写入 leaf key（不是 result 顶层）
- 深拷贝用 serde_json::Value 的 Clone trait

### serde_json::Value 而非 typed struct
- Node 的 Record<string, unknown> 是开放 dict
- Rust serde_json::Value 是最自然镜像
- 保留了 Node 动态类型优势（任意嵌套结构）

## 复用价值（async parity 前置）

本模块被以下 service 依赖：
- environment-config.ts — read/write config paths
- environment-custom-images.ts — writeConfigValueAtPath / readConfigValueAtPath
- environment-execution-target.ts — collectSecretRefPaths / parseSecretRefBindingObject
- plugin-environment-driver.ts — validate configs
- plugin-job-scheduler.ts (future)
- secrets.ts / auth.ts

完成 R682 后，这些 async service 的 parity 实施可以直接调用 json_schema_secret_refs 而无需重复编写相同逻辑。

## 进度更新
- 核心域覆盖度：99.78% → 99.83%（+0.05%, 单轮最大）
- 单元测试：6,604 → 6,664（+60）
- 下一步：R683 = 第一个 async parity（plugin-environment-driver.ts 部分 async 方法）
