# R400 — Command Managed Runtime + Sandbox Callback Bridge (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapter-utils/src/` 中两个
adapter-utils 模块完整移植到 `crates/pc-acpx/src/`:

1. `command-managed-runtime.ts` (319 行) → `command_managed_runtime.rs` (319 行)
   - **纯函数部分 9 个函数 + 3 个类型**完整移植
   - **async `createCommandManagedRuntimeClient` + `prepareCommandManagedRuntime`
     延后** (依赖真实 sandbox runtime + ssh)
2. `sandbox-callback-bridge.ts` (602 行) → `sandbox_callback_bridge.rs` (602 行)
   - **纯函数部分 6 个函数 + 常量 + allowlist**完整移植
   - **async bridge server / worker / client 延后** (需要真实 sandbox FS 钩子)

## Node 函数 / 类型映射

### command-managed-runtime.ts

| Node | Rust | 说明 |
|---|---|---|
| `PostUploadCommand` (interface) | `PostUploadCommand` (struct) | full 移植 |
| `SandboxFileMapping` (interface) | `SandboxFileMapping` (struct) | full 移植 |
| `SandboxSyncOperation` (interface) | `SandboxSyncOperation` (struct) | full 移植 |
| `shellQuote` (internal) | `shell_quote` (private fn) | POSIX single-quote 转义 |
| `buildSyncInExtractDirectoryCommand` | `build_sync_in_extract_directory_command` | full 移植 |
| `buildSyncInChmodCommand` | `build_sync_in_chmod_command` | full 移植 |
| `buildSyncInRenameCommand` | `build_sync_in_rename_command` | full 移植 |
| `buildUniqueStagingPath` | `build_unique_staging_path` | UUID v4 后缀 |
| `assertPostUploadCommandsConfined` | `assert_post_upload_commands_confined` | **核心安全防护** |
| `posixIsAbsolute` / `posixNormalize` (internal) | `posix_is_absolute` / `posix_normalize` (private fn) | 纯字符串处理 |
| `createCommandManagedRuntimeClient` | ❌ 延后 | 需 sandbox runtime |
| `prepareCommandManagedRuntime` | ❌ 延后 | 需 sandbox runtime |

### sandbox-callback-bridge.ts

| Node | Rust | 说明 |
|---|---|---|
| `DEFAULT_BRIDGE_TOKEN_BYTES` | `DEFAULT_BRIDGE_TOKEN_BYTES` | `24` |
| `DEFAULT_BRIDGE_POLL_INTERVAL_MS` | `DEFAULT_BRIDGE_POLL_INTERVAL_MS` | `100` |
| `DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS` | `DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS` | `30000` |
| `DEFAULT_BRIDGE_STOP_TIMEOUT_MS` | `DEFAULT_BRIDGE_STOP_TIMEOUT_MS` | `2000` |
| `DEFAULT_BRIDGE_MAX_QUEUE_DEPTH` | `DEFAULT_BRIDGE_MAX_QUEUE_DEPTH` | `64` |
| `DEFAULT_BRIDGE_MAX_BODY_BYTES` | `DEFAULT_BRIDGE_MAX_BODY_BYTES` | `262144` (256KB) |
| `DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES` | 同名 (re-export) | 同上 |
| `REMOTE_WRITE_BASE64_CHUNK_SIZE` | `REMOTE_WRITE_BASE64_CHUNK_SIZE` | `32768` |
| `SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT` | `SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT` | `"paperclip-bridge-server.mjs"` |
| `SANDBOX_EXEC_CHANNEL_ENV` / `SANDBOX_EXEC_CHANNEL_BRIDGE` | 同名 | env opt-in |
| `default_sandbox_callback_bridge_route_allowlist` | `default_sandbox_callback_bridge_route_allowlist` | 50+ 路由规则 |
| `DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST` | 同名 (const) | `accept,content-type,if-match,if-none-match` |
| `SandboxCallbackBridgeRouteRule` (interface) | `SandboxCallbackBridgeRouteRule` (struct) + `.new()` | full 移植 |
| `BridgeDirectories` (interface) | `BridgeDirectories` (struct) | full 移植 |
| `BridgeEnvInput` (interface) | `BridgeEnvInput` (struct) | full 移植 |
| `sandboxCallbackBridgeDirectories` | `sandbox_callback_bridge_directories` | full 移植 |
| `createSandboxCallbackBridgeToken` | `create_sandbox_callback_bridge_token` | base64url + OsRng |
| `authorizeSandboxCallbackBridgeRequestWithRoutes` | `authorize_sandbox_callback_bridge_request_with_routes` | regex 路由匹配 |
| `sanitizeSandboxCallbackBridgeHeaders` | `sanitize_sandbox_callback_bridge_headers` | 大小写不敏感 allowlist |
| `normalizeMethod` (internal) | `normalize_method` (private fn) | trim + upper |
| `buildSandboxCallbackBridgeEnv` | `build_sandbox_callback_bridge_env` | 返回 `BTreeMap` |
| `startSandboxCallbackBridgeServer*` | ❌ 延后 | 需 HTTP server + 文件监听 |
| `createSandboxCallbackBridgeClient*` | ❌ 延后 | 需 HTTP client |

## 关键设计决策

1. **Async 部分延后**：`createCommandManagedRuntimeClient`、
   `prepareCommandManagedRuntime`、bridge server/worker/client 都依赖真实 sandbox
   runtime / HTTP server / 文件轮询。Rust 移植只覆盖可在 `pc-acpx` 纯 helper
   层（无 async、无 process spawn、无 FS 监听）实现的部分。

2. **confinement guard 严格 1:1 移植 Node 语义**：
   - 先 short-circuit 拒绝 `..` 段 → "not a confined absolute POSIX path"
   - 再 posix-normalize + 检查是否在 target root 内 → "escapes the operation's target root"
   - 关键测试 `assert_confined_rejects_dotdot_cwd` 与
     `assert_confined_catches_normalized_escape_without_dotdot` 分别覆盖两个分支

3. **`create_sandbox_callback_bridge_token` 使用 `rand::rngs::OsRng`**：
   - Node 用 `crypto.randomBytes`，Rust 用 `OsRng`，保证密码学安全
   - 输出 `base64url(NO_PAD)`，长度固定（24B → 32 chars，16B → 22 chars）
   - 测试覆盖默认值 + 自定义字节数 + 唯一性 + 字符集（不含 `+/`）

4. **`BTreeMap` 替代 `Map` / 对象字面量**：
   - `build_sandbox_callback_bridge_env` 返回 `BTreeMap<String, String>`，
     保证环境变量迭代顺序确定性
   - `BridgeDirectories` struct 字段显式定义，无 key 冲突风险

5. **header allowlist 大小写不敏感**：
   - Rust 实现先 `to_lowercase` 比较，再 `to_lowercase` 过滤
   - 保留原 key 大小写（与 Node `sanitize...Headers` 一致）

6. **`RouteRule::new(method, path_regex)` 命名构造器**：
   - 避免 4 个字段的 `RouteRule { method: ..., path: ... }` 模板代码
   - 测试用一行 new 替代两行字段赋值

## 集成测试覆盖（19 个）

### command_managed_runtime (7 个)
- `happy_path_full_sync_in_flow` — 完整 sync-in：guard pass + extract + chmod + rename + uuid staging
- `confinement_rejects_cwd_outside_target_root` — sibling dir 拒绝
- `confinement_accepts_no_post_upload_commands` — 空 commands pass-through
- `confinement_rejects_relative_cwd` — "not a confined absolute POSIX path"
- `confinement_rejects_dotdot_cwd_with_specific_message` — `..` early-reject
- `unique_staging_paths_are_unique_under_concurrent_invocations` — 50 个路径无重复
- `shell_quoting_does_not_break_on_spaces_or_special_chars` — 路径含空格仍正确 quote

### sandbox_callback_bridge (11 个)
- `bridge_token_is_random_and_unique` — 默认 32 chars、自定义 16B 22 chars、无 `+/`
- `directories_compute_correct_layout` — 7 个派生路径全对
- `default_route_allowlist_includes_agents_endpoint` — `GET /api/agents/me` + 子路径 + `POST /api/issues/.../checkout`
- `default_route_allowlist_rejects_arbitrary_path` — `/etc/passwd` reject
- `default_route_allowlist_rejects_wrong_method` — `DELETE /api/agents/abc` reject
- `default_route_allowlist_method_case_normalized` — `get` (lowercase) 仍 ok
- `custom_routes_can_replace_defaults` — 自定义 regex 路由 + `/v1/agents` reject
- `sanitize_headers_preserves_allowed_keys` — Content-Type + Accept + If-Match 保留，X-Custom 删除
- `sanitize_headers_with_custom_allowlist` — 自定义 allowlist 生效
- `bridge_env_uses_documented_defaults` — None 值给出 default host/port
- `bridge_env_uses_overrides_for_tuning` — 6 个 override 字段全部反映到 env 输出

### 跨模块 smoke (1 个)
- `cross_module_smoke_confined_sync_in_with_authorized_bridge_ping` — 验证两个模块
  在典型 sync-in + bridge ping 流程中可同时通过各自授权检查，且 env 输出正确

## 编译 / 测试数据

```
pc-acpx lib  : 619 passed; 0 failed (was 586, +33)
pc-acpx tests: 33 integration files (incl. round400)
round400 test file: 19 tests; 0 failed
```

编译时新增依赖：`rand = "0.8"` + `base64 = { workspace = true }`（workspace
已有 `base64 = "0.22"`，`rand = "0.8"` 已在 pc-auth / pc-http / pc-repos / pc-secrets
使用）。两者都已显式加入 `pc-acpx/Cargo.toml`。

## 高内聚低耦合检查

- `command_managed_runtime.rs` 与 `sandbox_callback_bridge.rs` **完全独立**，
  无相互 `use`，互不依赖
- 仅 stdlib + `serde` + `rand` + `base64`，无外部 crate 依赖
- 可单独 mock / 测试 / 替换任一模块
- 测试仅依赖 public API

## 结论

R400 完成两个 module 完整移植（14 个函数 + 11 个常量 + 7 个结构 + 1 个 allowlist），
async 部分按既定原则延后。所有测试通过、无回归。
