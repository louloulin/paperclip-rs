# R545 — pc-home-paths（Node home-paths.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/home-paths.ts` (92 LOC) 完整复刻到新 crate
`crates/pc-home-paths`。workspace crates 86 → **87**。

## 设计原则

### 1. Env trait abstraction
- 用 `Env` trait 抽象 `std::env::var(...)` 和 `dirs::home_dir()`
- 生产用 `StdEnv` 实现，测试用 `MockEnv` 注入确定值
- 完全消除全局状态依赖，测试 100% deterministic

### 2. Single-root aggregator
- 新增 `resolve_paperclip_instance_paths` 一次性返回所有子路径
- 替代上游 11 个单独函数调用（业务代码只需一次调用即可拿完整 layout）
- 同时保留每个单独函数以镜像 Node API（向后兼容 + 测试性更强）

### 3. 强类型 + 错误处理
- `PaperclipInstanceInput<'a>` wrapper 替代 `{ homeDir?, instanceId? }`
- `PaperclipInstancePaths` struct 替代 11 个独立返回值
- `HomePathError::InvalidInstanceId` enum 替代 Node `throw new Error(...)`
  - 用 `thiserror` crate 派生 `Error` trait

### 4. `~` 展开规则与 Node 完全一致
- `"~"` → home dir
- `"~/foo"` → `<home>/foo`
- `"~foo"` (no slash) → literal
- 绝对/相对路径原样返回

### 5. Instance ID 严格校验
- 字符集 `[a-zA-Z0-9_-]+` 防止 path traversal
- 拒绝 `../escape`, `a/b`, `a b` 等
- 与上游 `PATH_SEGMENT_RE` 完全等价

## 公开 API

```rust
pub const DEFAULT_PAPERCLIP_INSTANCE_ID: &str = "default"
pub const PAPERCLIP_CONFIG_BASENAME: &str = "config.json"
pub const PAPERCLIP_ENV_FILENAME: &str = ".env"

pub trait Env {
    fn home_dir(&self) -> Option<PathBuf>
    fn var(&self, name: &str) -> Option<String>
}

pub struct StdEnv;  // 生产实现

pub enum HomePathError {
    InvalidInstanceId(String),  // #[error(...)] via thiserror
}

pub struct PaperclipInstanceInput<'a> { home_dir: Option<&'a str>, instance_id: Option<&'a str> }
pub struct PaperclipInstancePaths { home, instance_id, root, config_path, env_path, embedded_postgres_dir, logs_dir, secrets_key_file, storage_dir, backup_dir }

// 11 个 resolver + 1 个 aggregator
pub fn expand_home_prefix<E: Env>(env: &E, value: &str) -> PathBuf
pub fn resolve_paperclip_home_dir<E: Env>(env: &E, home_override: Option<&str>) -> PathBuf
pub fn resolve_paperclip_instance_id<E: Env>(env: &E, instance_id_override: Option<&str>) -> Result<String, HomePathError>
pub fn resolve_paperclip_instance_root<E: Env>(env: &E, input: PaperclipInstanceInput<'_>) -> Result<PathBuf, HomePathError>
pub fn resolve_paperclip_instance_config_path<E: Env>(env: &E, input: PaperclipInstanceInput<'_>) -> Result<PathBuf, HomePathError>
pub fn resolve_paperclip_config_path_for_instance<E: Env>(...) -> Result<PathBuf, HomePathError>
pub fn resolve_paperclip_env_path_for_config(config_path: &Path) -> PathBuf
pub fn resolve_default_embedded_postgres_dir<E: Env>(...) -> Result<PathBuf, HomePathError>
pub fn resolve_default_logs_dir<E: Env>(...) -> Result<PathBuf, HomePathError>
pub fn resolve_default_secrets_key_file_path<E: Env>(...) -> Result<PathBuf, HomePathError>
pub fn resolve_default_storage_dir<E: Env>(...) -> Result<PathBuf, HomePathError>
pub fn resolve_default_backup_dir<E: Env>(...) -> Result<PathBuf, HomePathError>
pub fn resolve_home_aware_path<E: Env>(env: &E, value: &str) -> PathBuf

pub fn resolve_paperclip_instance_paths<E: Env>(env: &E, input: PaperclipInstanceInput<'_>) -> Result<PaperclipInstancePaths, HomePathError>
```

## 与上游 Node 差异

- **`Env` trait 抽象**：消除 `process.env` / `os.homedir()` 全局状态依赖
- **Result 错误类型**：替代 `throw new Error(...)`
- **`PaperclipInstancePaths` 聚合器**：新增，11 个 resolver 一次性返回
- **空字符串 trim 后 fallback**：当 override / env 都 trim 后为空，自动 fallback 到下一优先级

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-home-paths` | **35 passed** (1 internal + 34 integration) |
| `cargo fmt -p pc-home-paths -- --check` | ✅ 通过 |
| `cargo clippy -p pc-home-paths --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（34 个集成测试 + 1 internal）

- **expand_home_prefix / resolve_home_aware_path** (7): bare ~ / ~/foo / 绝对路径 / 相对路径 / 中间 ~ / 缺失 home / alias 一致性
- **resolve_paperclip_home_dir** (7): override 优先 / env 优先 / trim override / 空 override fallback / trim env / 默认 fallback / ~ 展开
- **resolve_paperclip_instance_id** (7): override 优先 / env 优先 / 默认 / 拒绝非法字符 / trim 后空 fallback / 拒绝 path traversal / trim env
- **root + config + env** (6): 默认 home + id / 默认 id / PAPERCLIP_HOME env / config.json / alias / env 路径
- **Sub-directory resolvers** (1): db / logs / secrets/master.key / data/storage / data/backups 全部 layout
- **Aggregate resolver** (4): 完整 layout / PAPERCLIP_HOME env / 默认 id / error propagation
- **Constants** (1): 3 个常量稳定性

## 集成待办（不在本轮范围）

- `pc-server` 启动入口：用 `resolve_paperclip_instance_paths` 替换内联路径拼装
- `pc-config` 加载逻辑：用 `resolve_paperclip_instance_config_path` + `resolve_paperclip_env_path_for_config`
- `pc-storage` / `pc-secrets` / `pc-backup` / `pc-migrate`：用 `resolve_default_*_dir` 替换 ad-hoc 拼装
- 端到端 smoke：用 mock env 跑一遍启动 → config 加载 → secret 写入 → DB migrate
