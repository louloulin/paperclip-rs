# R557 — pc-config-schema（Node config-schema.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/config-schema.ts` (205 LOC) 完整复刻到新 crate
`crates/pc-config-schema`。workspace crates 98 → **99**。

## 设计原则

### 1. serde 强类型 + `camelCase` rename 替代 zod schema
- Node 用 `z.object({...})` + `z.literal(1)` + `z.enum([...])`
- Rust 用 `#[derive(Serialize, Deserialize)] #[serde(rename_all = "camelCase")]` struct
- 字段命名 1:1 镜像 Node JSON 契约（camelCase 输出，Rust 内部 snake_case）

### 2. `Default` trait 替代 zod `.default(value)`
- 每个 sub-config 都实现 `Default`
- 所有路径默认值（embedded-postgres data dir / log dir / storage dir / secrets key path）作为 `pub const`
- 编译期常量 + 零运行时分配

### 3. `serde_json::Value` 入口 + typed 出口
- `parse_paperclip_config(&Value) -> Result<PaperclipConfig, _>`
- 业务侧可以传入原始 JSON，crate 内部做完整 serde 转换 + 语义校验

### 4. `superRefine` 语义校验独立函数
- zod 的 `superRefine` 用 `validate_paperclip_config(&PaperclipConfig)` 函数替代
- 错误用 `PaperclipConfigError::Semantic { message, path }` 表达（带 path 便于定位）
- 委托给 `pc-network-bind::validate_configured_bind_mode` 处理 bind 模式校验

### 5. 与 `pc-config` 严格分离
- `pc-config`：env 变量驱动的运行时 Config（Rust server 启动用）
- `pc-config-schema`：JSON 文件驱动的持久化 Config schema（onboard/configure/doctor 写盘用）
- 两 crate 互不依赖，业务侧按场景 import

### 6. pc-network-bind 扩展
- 新增 `Deserialize` derive 到 `BindMode` / `DeploymentMode` / `DeploymentExposure`
- 保持 backward-compat（仅新增 derive，不改字段或方法）

## 公开 API

```rust
// ----- enums -----
pub enum ConfigMetaSource { Onboard, Configure, Doctor }      // "onboard" / "configure" / "doctor"
pub enum LlmProvider { Claude, Openai }
pub enum DatabaseMode { EmbeddedPostgres, Postgres }
pub enum LoggingMode { File, Cloud }
pub enum AuthBaseUrlMode { Auto, Explicit }
pub enum StorageProvider { LocalDisk, S3 }
pub enum SecretProvider { LocalEncrypted, AwsSecretsManager }

// ----- structs -----
pub struct ConfigMeta { version: u32, updated_at, source }
pub struct LlmConfig { provider, api_key: Option<String> }
pub struct DatabaseBackupConfig { enabled, interval_minutes, retention_days, dir }
pub struct DatabaseConfig { mode, connection_string: Option<String>, embedded_postgres_data_dir, embedded_postgres_port, backup }
pub struct LoggingConfig { mode, log_dir }
pub struct ServerConfig { deployment_mode, exposure, bind: Option<BindMode>, custom_bind_host: Option<String>, host, port, allowed_hostnames, serve_ui }
pub struct AuthConfig { base_url_mode, public_base_url: Option<String>, disable_sign_up }
pub struct StorageLocalDiskConfig { base_dir }
pub struct StorageS3Config { bucket, region, endpoint: Option<String>, prefix, force_path_style }
pub struct StorageConfig { provider, local_disk, s3 }
pub struct SecretsLocalEncryptedConfig { key_file_path }
pub struct SecretsConfig { provider, strict_mode, local_encrypted }
pub struct TelemetryConfig { enabled }
pub struct UpdatesConfig { check_enabled }
pub struct PaperclipConfig { $meta, llm: Option, database, logging, server, telemetry, updates: Option, auth, storage, secrets }

// ----- default constants -----
pub const DEFAULT_EMBEDDED_POSTGRES_DATA_DIR: &str = "~/.paperclip/instances/default/db"
pub const DEFAULT_EMBEDDED_POSTGRES_PORT: u16 = 54329
pub const DEFAULT_BACKUP_DIR: &str
pub const DEFAULT_BACKUP_INTERVAL_MINUTES: u32
pub const DEFAULT_BACKUP_RETENTION_DAYS: u32
pub const DEFAULT_LOG_DIR: &str
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1"
pub const DEFAULT_SERVER_PORT: u16 = 3100
pub const DEFAULT_STORAGE_LOCAL_BASE_DIR: &str
pub const DEFAULT_S3_BUCKET: &str = "paperclip"
pub const DEFAULT_S3_REGION: &str = "us-east-1"
pub const DEFAULT_SECRETS_KEY_FILE_PATH: &str

// ----- error -----
pub enum PaperclipConfigError { Json(serde_json::Error), Semantic { message, path } }

// ----- functions -----
pub fn parse_paperclip_config(value: &serde_json::Value) -> Result<PaperclipConfig, PaperclipConfigError>
pub fn validate_paperclip_config(config: &PaperclipConfig) -> Result<(), PaperclipConfigError>
```

## 与上游 Node 差异

- **serde rename_all = "camelCase"**：结构字段 Rust snake_case，JSON 仍是 camelCase
- **Default trait**：替代 zod `.default({...})`
- **thiserror Error**：替代 Node throw new Error
- **Semantic { path, message }**：错误带 path（zod ctx.addIssue path 等价）
- **pub const 默认值**：替代 zod 内联默认值（编译期可用）

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-config-schema` | **23 passed** (1 internal + 22 integration) |
| `cargo fmt -p pc-config-schema` | ✅ 通过 |
| `cargo clippy -p pc-config-schema --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（23 个）

- **defaults 镜像 Node** (1): 全部 7 个路径默认值与 Node 一致
- **每个 sub-config defaults** (8): backup / database / logging / server / auth / storage / secrets / telemetry / updates
- **JSON 序列化** (3): 默认值序列化为 camelCase / round-trip
- **round-trip** (1): PaperclipConfig → JSON → PaperclipConfig 一致
- **semantic errors** (5): local_trusted+public / explicit base_url required / public requires explicit mode / public requires publicBaseUrl / bind mode
- **meta version** (1): version != 1 拒绝
- **full valid config** (1): 完整 JSON 解析 + 校验通过

## 集成待办（不在本轮范围）

- `pc-config`：用 `parse_paperclip_config` 替代内联配置加载
- `pc-server` 启动流程：读 `~/.paperclip/instances/default/config.json` → `parse_paperclip_config` → 转换为运行时 Config
- `pc-cli onboard` / `configure` / `doctor` 子命令：用 `PaperclipConfig` typed struct 替代裸 JSON 写入
- 端到端：mock 用户 onboard → 配置写入 → 读取 → 验证默认值应用
