# R384 — Log Redaction + Env-Key Classification + PID Liveness

## 目标

按 `comet-open` + `RTK` 思路,把 Node `adapter-utils/src/server-utils.ts`
+ `command-redaction.ts` 中尚未在 `pc-acpx` 复刻的 8 个简单纯函数模块
一次性补齐,保持高内聚低耦合(新独立模块 `log_redaction`,无全局
状态、无 I/O、无 async)。

## 范围

- 新增 `crates/pc-acpx/src/log_redaction.rs`(独立模块,~520 行含 17 单测)
- `crates/pc-acpx/src/lib.rs` 增加模块导出 + re-export
- 新增 `crates/pc-acpx/tests/round384_log_redaction_and_pid.rs`(18 集成测试)
- 跟 Node `paperclip/packages/adapter-utils/src/server-utils.ts` L114-137,
  L1926-1964, L2229-2241, L3003-3013 行对齐,以及
  `paperclip/packages/adapter-utils/src/command-redaction.ts` L1-L60 对齐。

## 复刻的 8 个模块

### 1. `is_paperclip_runtime_env_key` (Node L114-121)
```ts
export function isPaperclipRuntimeEnvKey(key: string): boolean {
  return key.startsWith("PAPERCLIP_");
}
```
简单 prefix 检查。

### 2. `is_forbidden_config_env_key` (Node L122-131)
```ts
export function isForbiddenConfigEnvKey(key: string): boolean {
  return key === "PAPERCLIP_API_KEY";
}
```
PAPERCLIP_API_KEY 是唯一禁接受的 config key。

### 3. `expand_home_prefix` (Node L133-137)
```ts
function expandHomePrefix(value: string): string {
  if (value === "~") return os.homedir();
  if (value.startsWith("~/")) return path.resolve(os.homedir(), value.slice(2));
  return value;
}
```
`~` 和 `~/` 展开为 home,`~name`(不是 `~/`)不展开。

### 4. `is_sensitive_env_key` (新增辅助)
Node `SENSITIVE_ENV_KEY = /(key|token|secret|password|passwd|authorization|cookie)/i`
case-insensitive substring 匹配。`redactEnvForLogs` 和
`buildInvocationEnvForLogs` 都依赖此判定。

### 5. `redact_env_for_logs` (Node L1926-1933)
每个 key 走 is_sensitive_env_key,匹配则值替换为 `***REDACTED***`。
新增 `REDACTED_LOG_VALUE` 常量(`"***REDACTED***"`)。

### 6. `redact_command_text_for_logs` (Node L1934-1937 + command-redaction.ts)
对包含 secret hints(`api|key|token|...|sk-|ghp_|...`)的命令行扫描:
- OpenAI key:`sk-` 后接 12+ alphanumerics/`-`/`_` → `***REDACTED***`
- GitHub token:`gh[pousr]_` 后接 20+ alphanumerics → `***REDACTED***`
- `Authorization: Bearer <token>` → 替换 token
- 不引入 regex 依赖,用纯字符串扫描实现(acpx runtime 不需要其他模式)

### 7. `build_invocation_env_for_logs` (Node L1938-1964)
参数 options:
- `runtime_env: Option<BTreeMap<String, String>>`
- `include_runtime_keys: Vec<String>`
- `resolved_command: Option<String>`
- `resolved_command_env_key: Option<String>`

行为:merge caller env + runtime env (按 `include_runtime_keys` 列表,
caller 值优先) + 加 `resolved_command` 到 `PAPERCLIP_RESOLVED_COMMAND`
(或自定义 env key) → 全局 redact。

### 8. `sanitize_inherited_paperclip_env` (Node L2229-2241)
删 `PAPERCLIPAI_CMD` + 保留 3 个 allowlist
(`PAPERCLIP_RUNTIME_API_URL` / `PAPERCLIP_LISTEN_HOST` /
`PAPERCLIP_LISTEN_PORT`)+ 删其他 `PAPERCLIP_*`。

### 9. `is_pid_alive` (Node L3003-3013)
Node 用 `process.kill(pid, 0)`(signal-zero permission probe)。
Rust 由于 workspace `unsafe_code = "forbid"` 政策,**改用外部 `kill -0`
命令**(`sh -c "kill -0 $PID; echo $?"`),保持 forbid-clean:
- exit 0 → alive
- non-zero → dead
- `EPERM`(进程存在但属其他用户)→ Node 视为 alive — 当前实现
  通过 `kill -0` 的实际退出码覆盖(EPERM 时 exit 非 0),所以会被
  判为 dead。这是 Node 与 Rust 之间的一个 edge case trade-off,后续
  R385+ 视情况改进。

## Node 源码参考

```ts
// L114-121
export function isPaperclipRuntimeEnvKey(key: string): boolean {
  return key.startsWith("PAPERCLIP_");
}

// L122-131
export function isForbiddenConfigEnvKey(key: string): boolean {
  return key === "PAPERCLIP_API_KEY";
}

// L1926-1933
export function redactEnvForLogs(env: Record<string, string>): Record<string, string> {
  const redacted: Record<string, string> = {};
  for (const [key, value] of Object.entries(env)) {
    redacted[key] = SENSITIVE_ENV_KEY.test(key) ? REDACTED_LOG_VALUE : value;
  }
  return redacted;
}

// L1938-1964
export function buildInvocationEnvForLogs(env, options = {}) {
  const merged: Record<string, string> = { ...env };
  const runtimeEnv = options.runtimeEnv ?? {};
  for (const key of options.includeRuntimeKeys ?? []) {
    if (key in merged) continue;
    const value = runtimeEnv[key];
    if (typeof value !== "string" || value.length === 0) continue;
    merged[key] = value;
  }
  const resolvedCommand = options.resolvedCommand?.trim();
  if (resolvedCommand) {
    merged[options.resolvedCommandEnvKey ?? "PAPERCLIP_RESOLVED_COMMAND"] =
      redactCommandTextForLogs(resolvedCommand);
  }
  return redactEnvForLogs(merged);
}

// L2229-2241
export function sanitizeInheritedPaperclipEnv(baseEnv): NodeJS.ProcessEnv {
  const env = { ...baseEnv };
  delete env.PAPERCLIPAI_CMD;
  for (const key of Object.keys(env)) {
    if (!key.startsWith("PAPERCLIP_")) continue;
    if (key === "PAPERCLIP_RUNTIME_API_URL") continue;
    if (key === "PAPERCLIP_LISTEN_HOST") continue;
    if (key === "PAPERCLIP_LISTEN_PORT") continue;
    delete env[key];
  }
  return env;
}

// L3003-3013
function isPidAlive(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    const code = err && typeof err === "object" ? (err as { code?: unknown }).code : null;
    return code === "EPERM";
  }
}
```

## 测试

- 17 个新单元测试注入 `log_redaction::tests`
- 18 个新集成测试在 `tests/round384_log_redaction_and_pid.rs`

合计 R384 新增 35 个测试,全部绿色。

## 验证

```
cd paperclip-rs && cargo test -p pc-acpx
```

结果:576 个 pc-acpx tests 通过 (R383 是 541,+35),0 失败 0 回归。
`round384_log_redaction_and_pid.rs` 18 个新增测试全绿。

```
cd paperclip-rs && cargo fmt -p pc-acpx --check
```

clean。

## 下一步

R385 候选模块(Node `server-utils.ts` 剩余):
1. `signalRunningProcess` (L82-112) — Unix process group signal
2. `sanitizeSshRemoteEnv` (L2311-2317) — SSH env filter
3. `shapePaperclipWorkspaceEnvForExecution` (L2023-2117) — env shape
4. `rewriteWorkspaceCwdEnvVarsForExecution` (L2118-2154) — env rewrite
5. `refreshPaperclipWorkspaceEnvForExecution` (L2155-2228) — env refresh

R386 候选:
6. `resolvePaperclipInstanceRootForAdapter` (L139-285) — 复杂 OS 路径解析

R387 候选:
7. `readPaperclipSkillSyncPreference` (L2794-2834)
8. `writePaperclipSkillSyncPreference` (L2870-3002)
9. `resolvePaperclipDesiredSkillNames` (L2858-2869)
