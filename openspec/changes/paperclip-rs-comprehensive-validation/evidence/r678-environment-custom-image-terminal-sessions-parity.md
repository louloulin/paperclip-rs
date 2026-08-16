# R678 — environment-custom-image-terminal-sessions 1:1 parity

## 目标

完整复刻 Node `server/src/services/environment-custom-image-terminal-sessions.ts` (353 行)
+ Node `server/src/services/environment-custom-image-setup-session-utils.ts` (32 行) 到
`crates/pc-environment/src/custom_image_terminal_sessions.rs` + setup_session_utils.rs。

## 工作产出

### 1. 新增文件

| 路径 | 行数 | 内容 |
|---|---:|---|
| `crates/pc-environment/src/custom_image_setup_session_utils.rs` | 71 | 4 个 pure date / metadata helpers |
| `crates/pc-environment/src/custom_image_terminal_sessions.rs` | 530 | 2 pure exports + 2 in-memory class + 6 types + 2 constants |
| `crates/pc-environment/tests/custom_image_terminal_sessions_tests.rs` | 459 | 35 个 unit test |

### 2. 依赖挂接（`crates/pc-environment/Cargo.toml`）

- `chrono` (workspace, features=["serde"])
- `rand` (workspace)
- `base64` "0.22"
- `thiserror` (workspace)
- 既有 `serde` / `serde_json` / `sha2` / `uuid`

### 3. 1:1 parity 矩阵

#### setup_session_utils (32 行)

| Node export | Rust 实现 | 状态 |
|---|---|---|
| `readCustomImageSetupSessionCompanyId` | `read_custom_image_setup_session_company_id` | ✅ |
| `readNullableDate` | `read_nullable_date` | ✅ |
| `readFutureDate` | `read_future_date` | ✅ |
| `requireFutureCustomImageSetupExpiry` | `require_future_custom_image_setup_expiry` | ✅（返回 `Result`, 错误用 `SetupSessionExpiredError`） |

#### terminal_sessions (353 行)

| Node export | Rust 实现 | 状态 |
|---|---|---|
| `parseCustomImageSetupSshCommand` | `parse_custom_image_setup_ssh_command` | ✅ |
| `validateCustomImageSetupSshPayload` | `validate_custom_image_setup_ssh_payload` | ✅ |
| `EnvironmentCustomImageTerminalSessionStore` | `EnvironmentCustomImageTerminalSessionStore` (in-memory + Mutex) | ✅ |
| `environmentCustomImageTerminalSessionStore` | `ENVIRONMENT_CUSTOM_IMAGE_TERMINAL_SESSION_STORE` (LazyLock singleton) | ✅ |
| `EnvironmentCustomImageTerminalConnectionRegistry` | `EnvironmentCustomImageTerminalConnectionRegistry` (Arc-wrapped + Mutex) | ✅ |
| `environmentCustomImageTerminalConnectionRegistry` | `ENVIRONMENT_CUSTOM_IMAGE_TERMINAL_CONNECTION_REGISTRY` | ✅ |
| `DEFAULT_TERMINAL_SESSION_TOKEN_TTL_MS` | 同名常量 | ✅ |
| `TERMINAL_SESSION_TOKEN_BYTES` | 同名常量 | ✅ |
| `ParsedCustomImageSetupSshCommand` | `pc_environment::ParsedCustomImageSetupSshCommand` | ✅ |
| `EnvironmentCustomImageTerminalSessionRecord` | 同名 | ✅ |
| `MintedEnvironmentCustomImageTerminalSession` | 同名 | ✅ |
| `EnvironmentCustomImageTerminalPayloadValidationFailureCode` | 同名 enum | ✅ |
| `EnvironmentCustomImageTerminalPayloadValidationResult` | 同名 enum（tagged by `ok`） | ✅ |
| `EnvironmentCustomImageTerminalConnectionClose` | `Box<dyn Fn(String) + Send + Sync>` | ✅ |

### 4. 关键设计要点

- **SessionStore**: `Mutex<HashMap<String, StoredSession>>` —— Node 用 plain Map，
  Rust 用 `Mutex` 包裹以满足线程安全（rustc static LazyLock 要求 Sync）
- **ConnectionRegistry**: 用 `Arc<RegistryInner>` 拆解 `self` 生命周期，让 `add()`
  返回的 `impl FnOnce() + Send + 'static` unregister closure 可安全捕获 inner Arc，
  避免借用检查器报 `lifetime may not live long enough`
- **Token hash**: `Sha256(token) → hex` 1:1 Node `createHash("sha256").update(token).digest("hex")`
- **Token format**: `base64::URL_SAFE_NO_PAD.encode(32 random bytes)` 对齐 Node `randomBytes(32).toString("base64url")`
- **Min date**: 手写 `min_date` 避免引入 `iter::min` 返回 `Option` 节点对不上
- **Setup session expiry**: `read_future_date`（避免 `Option<DateTime>` 的歧义）

## 测试结果

### `cargo test -p pc-environment --test custom_image_terminal_sessions_tests`

```
test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 测试覆盖矩阵

| 类别 | 测试数 | 覆盖点 |
|---|---:|---|
| SSH 命令解析 | 5 | 默认端口 / `-p` 前置 / `-p` 后置 / 各种 invalid / trim |
| Payload 验证 | 7 | OK / 非 ssh / 非 object / 缺 command / 不支持 command / 无效 expiry / expired / future OK |
| Setup session utils | 8 | company id 4 场景 / date parse 5 场景 / future date past / future / require future ok/err |
| SessionStore | 8 | create+get / 错 token / connect expired / setup expired 拒绝 create / get_by_id session expired / verify_or_pin_host_key / delete_by_setup_session_id / 常量 |
| ConnectionRegistry | 5 | unregister / close by id / close all / 未知 setup id / clear |

### 关键 bug & 学习

1. **Lifetime 不通过** —— 初版 `pub fn add(&self, ...) -> impl FnOnce()` 把 `&self` 捕获进闭包，导致闭包没法 `'static`。修复：拆 `RegistryInner` + 用 `Arc<Self>` 让 add 接收 self-owned。
2. **测试期望错** —— `drop(unregister)` 不会触发 `FnOnce` body（必须显式 `unregister()`）。修正测试 + 加注释。这条 R673 的"测试期望 vs Node 真实行为"教训的延续。
3. **`verify_or_pin_host_key` 返回 `bool` 不是 `Option<bool>`** —— 初版误以为 Option，写了 `.unwrap_or(false)`。修正测试。

## 回归

- `cargo test -p pc-environment --lib`：**7 passed**（R671 runtime_parity 仍 OK）
- `cargo test -p pc-environment --test config_tests`：**44 passed**（R675 仍 OK）
- `cargo test -p pc-environment --test custom_image_runtime_tests`：**41 passed**（R677 仍 OK）
- `cargo test -p pc-environment --test custom_image_terminal_sessions_tests`：**35 passed**（R678 新增）
- `cargo test -p pc-http --lib`：**495 passed / 0 failed**
- `cargo test -p pc-plugin-database`：**47 passed / 0 failed**
- `cargo build -p pc-server`：成功（无新 warning）

## 综合覆盖度（更新至 R678）

| 维度 | R677 终态 | R678 终态 |
|---|---|---|
| pc-environment lib tests | 7 | **7** |
| pc-environment config_tests | 44 | **44** |
| pc-environment custom_image_runtime_tests | 41 | **41** |
| pc-environment custom_image_terminal_sessions_tests | — | **35 passed** |
| pc-http lib tests | 495 | **495** |
| pc-plugin-database tests | 47 | **47** |
| pc-server build | ✅ | ✅ |

## 用户硬约束遵守

| 约束 | 状态 |
|---|---|
| 不 commit | ✅ |
| 不修 Adapter | ✅ |
| 真实验证 | ✅（35 unit test PASS + 全套 regression 无 regression） |
| 中文 evidence | ✅ |
| 不修预存在 unrelated bug | ✅ |
| 不调 `update_goal` 完成 | ✅ |
| 继续推进 | ✅ |

## 后续计划

| 轮次 | 内容 |
|---|---|
| **R679** | pc-server prod-mode 真实启动 + 真实 OAUTH 模拟（authenticated 路径） |
| **R680** | 探索下一个完全未复刻 Node service parity 缺口（候选：`<plugin-environment-driver.ts (570) / `plugin-job-scheduler.ts (752)`） |
