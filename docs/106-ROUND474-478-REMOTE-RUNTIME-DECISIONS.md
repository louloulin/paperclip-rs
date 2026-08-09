# R474-R478 完成 — 远程受管运行时决策 + codex session 序列化/resume

## 1. 目标

在 R469-R473（远程 workspace 决策 + bridge env + quota）基础上继续深化，
覆盖 Node `adapter-utils/remote-managed-runtime.ts` 与
`codex-local/src/server/execute.ts` 的远程执行编排决策。

## 2. 新增模块

### R474 — `pc-acpx::remote_managed_runtime` 布局决策（+9 测试，模块共 26）

对齐 Node `prepareRemoteManagedRuntime` 的纯决策部分：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `RemoteManagedRuntimeAsset` | `RemoteManagedRuntimeAsset` | key/localDir/followSymlinks/exclude/restore |
| `AdditionalSource` | `SandboxAdditionalSource` | localPath/projectId |
| `prepare_remote_managed_runtime_layout` | `prepareRemoteManagedRuntime` | workspaceRemoteDir + runtimeRootDir + assetDirs + additionalSourceDirs |
| `resolve_prepared_workspace_remote_dir` | syncWorkspace 分支 | runs/<runId>/workspace vs base |
| `resolve_asset_dirs` | `assetDirs[key] = join(runtimeRootDir, key)` | 资产目录映射 |
| `resolve_additional_source_dirs` | additionalSourceDirs | 校验通过的项目 |
| `additional_source_local_path_is_absolute` | `path.posix.isAbsolute` | POSIX 绝对路径 |
| `additional_source_project_id_is_valid` | 内联校验 | 非空 / 无 `/` `\` `..` |
| `resolve_additional_source_remote_dir` | `join(runtimeRootDir, \`project-${id}\`)` | 项目隔离目录 |

### R475 — `codex_session_params.rs`（codex-local，8 测试）

对齐 Node codex `execute.ts` L1342-1353 `resolvedSessionParams`：

- `build_resolved_session_params` — sessionId/cwd/remoteExecution?/workspaceId?/repoUrl?/repoRef?
- 远程时填 `adapterExecutionTargetSessionIdentity(target)`
- 空字段不写入（spread 模式语义）
- 便捷读取器：session_id / cwd / remote_execution / workspace_id / repo_url / repo_ref

### R476 — `codex_session_resume.rs`（codex-local，12 测试）

对齐 Node codex `execute.ts` L981-1004 `canResumeSession`：

- `decide_codex_session_resume` — cwd 匹配（path.resolve 语义）+ identity 匹配 +
  forceFreshSession + 完整日志分支
- `session_cwd_matches_execution_target` — 空 cwd 恒 true，否则 resolve 比较
- `remote_execution_matches_target` — 复用 pc-acpx `adapter_execution_target_session_matches`
  （SSH 5 元组含 remoteCwd；沙箱 5 元组）
- 日志精确复刻 Node 两条 onLog 文案

## 3. 集成测试

### R477 — `tests/round477_remote_runtime.rs`（codex-local，6 测试）

把 R469-R476 串成 Node 远程执行全链路：

1. 布局决策（workspaceRemoteDir / runtimeRootDir / home asset）
2. bridge env 注入（PAPERCLIP_API_URL / BRIDGE_MODE）
3. sessionParams 序列化（含 remoteExecution identity）
4. resume 决策（身份匹配允许 / 不匹配拒绝 + 日志）
5. env cwd 重写（managedRemoteWorkspace）

## 4. 测试快照

| Crate | R473 | R478 | Δ |
|---|---|---|---|
| pc-acpx | 883 | 892 | +9 |
| pc-adapter-codex-local | 429 | 455 | +26 |
| pc-adapter-claude-local | 475 | 475 | 0 |
| pc-activity | 14 | 14 | 0 |
| pc-adapter-process | 6 | 6 | 0 |
| pc-adapter-quota | 39 | 39 | 0 |
| **合计** | **1846** | **1881** | **+35** |

## 5. 后续计划

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | claude-local `claude_session_params` 补全 | 检查与 codex 版对齐（含 workspaceId 序列化） |
| P1 | pc-acpx `sandbox_callback_bridge` 决策 | worker/server 启动决策 + runLogTail |
| P2 | codex-local `execute` 编排 | 把 session_params + resume + bridge 接入主执行流程 |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restore 真实 I/O 封装 |
| P3 | 其他 adapter | 延后（用户约束） |

## 6. R479 增量 — 主执行流程接入远程 identity

### codex-local `lib.rs`

- 私有 `build_resolved_session_params` 升级：解析 `context.execution_target`，
  远程 target 时装配 `remoteExecution` identity（复用
  `codex_session_params::build_resolved_session_params` + pc-acpx
  `parse_adapter_execution_target` / `adapter_execution_target_session_identity`）
- 新增 2 测试：远程装配 identity（transport/host/username/port）、本地省略
- cwd 语义修正：Node 始终写 `cwd: effectiveExecutionCwd`（空也写）

### claude-local `lib.rs`

- 移除硬编码 `execution_target_is_remote = false` / identity `None`：
  从 `context.execution_target` 解析远程状态与 identity，传入
  `decide_claude_session_resume` 与 `claude_resume_loop`
- claude_result_builder 已支持 `execution_target_session_identity`，
  现在主流程真正装配

### 测试快照（R479 后）

| Crate | 测试数 |
|---|---|
| pc-acpx | 892 |
| pc-adapter-codex-local | 457（+2） |
| pc-adapter-claude-local | 475 |
| **合计（3 crate）** | **1824** |

## 7. R480 增量 — bridge worker/server 转发决策纯函数（+8 测试）

继续对齐 Node `adapter-utils/execution-target.ts` L1232-1260 的 bridge
转发决策，全部为 pc-acpx 纯决策函数（高内聚低耦合：决策与 I/O 分离）：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `normalize_timeout_ms` | `normalizeTimeoutMs` | 有限且 > 0 取整，否则 fallback；u64 范围内有限性恒真，故仅 >0 判定生效 |
| `build_bridge_forward_url` | `buildBridgeForwardUrl` | baseUrl + path + query 拼接；path 补 `/`、query 去前导 `?` 并 trim |
| `build_bridge_response_headers` | `buildBridgeResponseHeaders` | 仅透传 content-type/etag/last-modified；键名大小写不敏感（对齐 `Response.headers.get()`）、空值剔除并 trim |
| `bridge_response_body_limit_error` | `bridgeResponseBodyLimitError` | 超限错误文案逐字对齐 |
| `bridge_response_body_within_limit` | `readBridgeForwardResponseBody` 预检 | content-length > max 时 Err（等于 max 不超限） |

### 关键决策

- **大小写不敏感 header**：初版实现按精确小写键匹配，测试发现与 Node
  `Response.headers.get()`（Fetch 规范大小写不敏感）不一致，已改为
  归一化后匹配，保证转发透传行为与 Node 相同。
- **`Some(u64::MAX)` 语义**：Node `Number.isFinite` 在 u64 全范围恒真，
  因此任意 > 0 值直接生效、不回退（与 `Some(0)`/`None` 回退形成对照）。

### 测试快照（R480 后全量）

| Crate | 测试数 |
|---|---|
| pc-acpx | 900（+8） |
| pc-adapter-codex-local | 457 |
| pc-adapter-claude-local | 475 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **合计** | **1891** |

## 8. 后续计划（R481+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | pc-acpx `sandbox_callback_bridge` 决策 | worker 决策：authorizeRequest 默认路由、writeBridgeResponse 决策、请求 JSON 解析失败 400 响应 |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | codex/claude 主执行流程更深接入 | bridge env 合并进执行 env、restoreWorkspace 回调、进程 session 代理 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 9. R481 增量 — bridge worker 决策纯函数（+12 测试）

对齐 Node `startSandboxCallbackBridgeWorker` 的 `processRequestFile` /
`writeBridgeResponse` / `failPendingRequests` 纯决策部分
（`sandbox-callback-bridge.ts` L594-737），全部为 pc-acpx 纯函数：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `SandboxCallbackBridgeRequest` / `SandboxCallbackBridgeResponse` | 同名接口 | camelCase JSON 序列化对齐协议 |
| `parse_bridge_request_file` | `JSON.parse(raw)` | 解析失败 → 400 |
| `bridge_request_id_from_file_name` | `fileName.replace(/\.json$/i, "") \|\| randomUUID()` | 大小写不敏感去后缀，空 → None |
| `invalid_bridge_request_payload_response` | catch 分支 | 400 + `{"error":"Invalid bridge request payload."}` |
| `denied_bridge_request_response` | denial 分支 | 403 + 拒绝原因 |
| `handler_failure_bridge_response` | catch 分支 | 502 + 错误消息 |
| `pending_request_failure_bridge_response` | `failPendingRequests` | 503 + 停止消息 |
| `bridge_response_json_line` | `` `${JSON.stringify(response)}\n` `` | 单行 JSON + 换行 |
| `bridge_response_body_utf8_len_within_limit` | `Buffer.byteLength(body, "utf8") > maxBodyBytes` | UTF-8 字节数检查 |
| `decide_bridge_handler_response` | body 检查 + 写响应 | body 超限 → Err（调用方转 502） |
| `decide_bridge_response_write` / `BridgeResponseWritePlan` | `writeBridgeResponse` | 直写（带 requestPath）vs temp+rename 兜底 |

### 关键决策

- **`requireRequestPath === false`**：`failPendingRequests` 补写响应时
  不携带 requestPath，直写计划将 request_path 置 None。
- **id 取自文件名**：解析失败时也无请求体可用，request id 只能从
  文件名提取；空文件名 → None（执行器生成 UUID，对齐 Node
  `randomUUID()` 兜底）。
- **body 超限转换**：handler 返回体超限时 Node 直接 throw，被外层
  catch 捕获转 502；Rust 用 `decide_bridge_handler_response` 返回
  Err，执行器统一用 `handler_failure_bridge_response` 转 502，
  避免决策层重复两套分支。

### 测试快照（R481 后全量）

| Crate | 测试数 |
|---|---|
| pc-acpx | 912（+12） |
| pc-adapter-codex-local | 457 |
| pc-adapter-claude-local | 475 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **合计** | **1903** |

## 10. 后续计划（R482+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | pc-acpx `sandbox_callback_bridge` 收尾 | worker 循环决策（poll 间隔、队列深度、停止 deadline）与 server 决策（鉴权 401 / 队列满 503 / 415） |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | codex/claude 主执行流程更深接入 | bridge env 合并进执行 env、restoreWorkspace 回调、进程 session 代理 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 11. R482 增量 — bridge worker 循环 + server 决策（+11 测试）

对齐 Node `startSandboxCallbackBridgeWorker` 的 loop/stop（L760-806）与
`createServer` 的鉴权/队列/内容类型/响应决策（L1155-1230），全部为
pc-acpx 纯函数：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `decide_bridge_worker_loop_action` / `BridgeWorkerLoopAction` | loop 头部 | 空队列+stopping → Stop；空队列 → Sleep；否则 Process |
| `decide_bridge_worker_should_stop_processing` | `stopping && Date.now() >= stopDeadline` | 内层/外层 break 共用 |
| `decide_bridge_worker_stop_deadline` | `Date.now() + normalizeTimeoutMs(drainTimeoutMs, 2000)` | saturating_add 防溢出 |
| `bridge_server_bearer_token` | `auth.startsWith("Bearer ") ? auth.slice(7) : ""` | Bearer 提取 |
| `bridge_server_token_matches` | `tokensMatch` | 等长预检 + XOR 常数时间比较（对齐 timingSafeEqual） |
| `bridge_server_queue_full` | `queueDepth() >= maxQueueDepth` | 满 → 503 |
| `bridge_server_accepts_content_type` | `!== "GET" && !== "HEAD" && !/json/i.test` | 415 判定 |
| `bridge_server_error_response` / `BridgeServerError` | 401/503/415 分支 | status + `{"error": message}` |
| `bridge_wait_deadline_ms` / `bridge_wait_for_response_should_retry` | `waitForResponse` | `Date.now() < deadline` 轮询 |
| `bridge_server_response_status` | `typeof status === "number" ? status : 200` | 默认 200 |
| `filter_bridge_server_response_headers` | 响应循环 | content-length 大小写不敏感剔除 |
| `bridge_server_response_body` | `typeof body === "string" ? body : ""` | 默认空串 |
| `bridge_request_json_line` | `` `${JSON.stringify(payload)}\n` `` | 请求队列文件单行 JSON |

### 关键决策

- **常数时间比较**：token 校验对齐 Node `timingSafeEqual`——长度不等
  直接 false，等长时 XOR 累加后统一判定，避免逐字节提前返回的时序侧信道。
- **溢出饱和**：时间戳加法（stop deadline、wait deadline）用
  `saturating_add`，u64 极端值不 panic（Node 浮点无此问题）。
- **415 判定顺序**：GET/HEAD 恒放行；其余方法要求 content-type 含
  `json` 子串（大小写不敏感，对齐 `/json/i` 正则）。

### 测试快照（R482 后全量）

| Crate | 测试数 |
|---|---|
| pc-acpx | 923（+11） |
| pc-adapter-codex-local | 457 |
| pc-adapter-claude-local | 475 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **合计** | **1914** |

## 12. 后续计划（R483+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | pc-acpx `sandbox_callback_bridge` 收尾 | `startSandboxCallbackBridgeServer` 启动编排决策（mkdir/pid/ready.json 轮询）与 `stopSandboxCallbackBridgeServer` |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | codex/claude 主执行流程更深接入 | bridge env 合并进执行 env、restoreWorkspace 回调、进程 session 代理 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 13. R483 增量 — bridge server 启动/就绪/停止编排决策（+11 测试）

对齐 Node `startSandboxCallbackBridgeServer`（L961-1100）与
`sandbox-shell.ts`，全部为 pc-acpx 纯函数（脚本字符串构建 + 就绪解析 +
失败消息，I/O 由执行器负责）：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `preferred_shell_for_sandbox` | `preferredShellForSandbox` | 仅 `"bash"` 用 bash，其余 `sh` |
| `shell_command_args` | `shellCommandArgs` | `["-c", script]` |
| `shell_quote` | `shellQuote` | 单引号包裹 + `'"'"'` 转义 |
| `build_bridge_exec_env` | `{ channel, ...env }` | 注入 SANDBOX_EXEC_CHANNEL，env 同名键覆盖 |
| `build_bridge_server_start_script` | start execute 脚本 | mkdir 队列目录 / 清 ready+pid / nohup / 写 pid / pid JSON |
| `build_bridge_ready_poll_script` | ready 轮询脚本 | 200×0.05s；ready 非空成功；进程死读日志失败；超时报错 |
| `build_bridge_server_stop_script` | stop 脚本 | kill + 40×0.05s 轮询 + 清理 pid/ready |
| `parse_bridge_ready_data` / `BridgeReadyData` | ready JSON 解析 | host/port/baseUrl/pid 逐项回退；port=0 报错 |
| `bridge_runner_failure_message` | `buildRunnerFailureMessage` | timedOut / exit code / stderr 优先于 stdout |

### 关键决策

- **脚本即数据**：三个 shell 脚本均为纯字符串构建，可单测逐行断言，
  与 Node 模板逐字对齐（含 `sleep 0.05`、`printf '{\"pid\":%s}\\n'` 等细节）。
- **env 覆盖顺序**：对齐 Node 对象展开顺序——channel 先设、env 后展开，
  执行器自定义 env 可覆盖 channel。
- **ready 解析回退链**：host 空白 → `127.0.0.1`；baseUrl 空白 →
  `http://{host}:{port}`；port 缺失/0 → 明确报错；pid 非 number → 0。

### 测试快照（R483 后全量）

| Crate | 测试数 |
|---|---|
| pc-acpx | 934（+11） |
| pc-adapter-codex-local | 457 |
| pc-adapter-claude-local | 475 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **合计** | **1925** |

## 14. 后续计划（R484+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | pc-acpx `sandbox_callback_bridge` 收尾 | 远程文本同步决策（`syncRemoteTextFileWithHashSkip` 的 sha256 门控/pid 锁脚本）与 worker 队列客户端决策（makeDirs/readTextFile/writeTextFile/rename/remove 计划） |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | codex/claude 主执行流程更深接入 | bridge env 合并进执行 env、restoreWorkspace 回调、进程 session 代理 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 15. R484 增量 — 远程文本同步 + 队列客户端决策（+14 测试）

对齐 Node `syncRemoteTextFileWithHashSkip`（L825-913）、
`buildRemotePidLock*Script`（L240-277）与
`createCommandManagedSandboxCallbackBridgeQueueClient`（L460-590），
全部为 pc-acpx 纯决策（脚本字符串 + 解析 + 工具函数，I/O 由执行器做）：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `sha256_hex_utf8` / `base64_encode_utf8` | `createHash(...).digest("hex")` / `toString("base64")` | 模块内联工具（复用 workspace sha2/base64/hex） |
| `split_base64_chunks` | `base64Chunks` | 32KB chunk 切分（对齐 REMOTE_WRITE_BASE64_CHUNK_SIZE） |
| `posix_dirname` | `path.posix.dirname` | 纯 POSIX 语义（含空串/根/尾斜杠） |
| `remote_partial_path` / `remote_upload_path` | `.partial` / `.paperclip-upload.b64` | 同步辅助路径 |
| `build_remote_pid_lock_acquire_script` | `buildRemotePidLockAcquireScript` | mkdir 原子锁 + 600×0.05s + 死锁回收 + `$$` 写 pid |
| `build_remote_pid_lock_cleanup_script` | `buildRemotePidLockCleanupScript` | cleanup 函数 + EXIT/INT/TERM trap |
| `build_sync_text_file_with_hash_skip_script` | `syncRemoteTextFileWithHashSkip` 脚本 | hash_file 双工具探测 / 内容哈希跳过 / base64 上传 / 完整性校验 / 原子 rename |
| `parse_sync_text_file_result` | `JSON.parse(stdout.trim())?.uploaded === true` | `null`/缺字段 → false；非法 JSON → 明确报错 |
| `build_make_dir(s)_script` | makeDir/makeDirs | 空列表 → None 不执行 |
| `build_list_json_files_script` / `parse_list_json_files_output` | listJsonFiles | 目录存在性 + `*.json` + basename + trim/sort |
| `build_read_text_file_script` | readTextFile | `base64 < <path>` |
| `build_write_text_file_steps` / `ClientScriptStep` | writeTextFile | prepare / append chunks / finalize 三段式（action+script 分离） |
| `build_write_response_file_script` | writeResponseFile | pid 锁 + requestPath 存在性 + 幂等 + temp+mv 原子写 |
| `parse_write_response_file_result` | `?.wrote === true` | 结果解析 |
| `build_rename_script` / `build_remove_script` | rename/remove | mkdir dirname + mv / rm -rf |

### 关键决策

- **脚本即数据 + action 分离**：`ClientScriptStep` 把 action（失败消息用）
  与 script 解耦，执行器按序执行并可用 `bridge_runner_failure_message`
  统一报错，与 Node `runChecked(action, script)` 一一对应。
- **sha256 门控**：宿主计算 sha256，远端 `sha256sum`/`shasum` 双工具
  探测；已有同内容时跳过上传（`{"uploaded":false}`），上传后校验
  mismatch 即失败，无工具时降级警告（对齐 Node 的 fail-loud 语义）。
- **锁协议**：`mkdir` 原子获取 + 持有者 pid 探活（死锁自动回收）+
  600 次超时 + `trap` 清理，与 Node 完全一致。

### 测试快照（R484 后全量）

| Crate | 测试数 |
|---|---|
| pc-acpx | 948（+14） |
| pc-adapter-codex-local | 457 |
| pc-adapter-claude-local | 475 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **合计** | **1939** |

## 16. 后续计划（R485+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | pc-acpx `sandbox_callback_bridge` 收尾 | 集成测试（worker+server 决策串成全链路）；`syncSandboxCallbackBridgeEntrypoint` / `startSandboxCallbackBridgeServer` 组合决策 |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | codex/claude 主执行流程更深接入 | bridge env 合并进执行 env、restoreWorkspace 回调、进程 session 代理 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 17. R485 增量 — bridge 组合决策 + 全链路集成测试（+3 单元 +7 集成）

对齐 Node `syncSandboxCallbackBridgeEntrypoint`（L911-940）与
`startSandboxCallbackBridgeServer`（L961-1100）的编排决策，把
R480-R484 的碎片函数串成完整计划：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `sync_sandbox_callback_bridge_entrypoint_plan` | `syncSandboxCallbackBridgeEntrypoint` | remoteEntrypoint join / 宿主 sha256 / lockDir / 同步脚本 / action+label |
| `SyncBridgeEntrypointPlan::expected_sha` | — | 便捷读取器（门控值） |
| `start_sandbox_callback_bridge_server_plan` | `startSandboxCallbackBridgeServer` 决策部分 | timeout/shell/directories/entrypoint（可选 sync）/env/nodeCommand/start+ready+stop 三脚本 |
| `StartBridgeServerPlan` / `StartBridgeServerPlanInput` | StartedServer + 入参 | 执行器按计划执行并解析 ready |

### 集成测试 `tests/round485_bridge_worker_server.rs`（7 测试）

1. **worker 全链路**：请求解析 → 白名单放行 → 200 响应 → Direct 写计划；
   非白名单 403 / 非法 JSON 400（id 从文件名提取）；body 超限 → 502
2. **server 全链路**：Bearer 401 → 队列满 503 → 415 → payload 构造
   （query 保留 `?`）→ 响应轮询截止 → 状态/headers/body 归一化
3. **启动编排**：entrypoint 同步计划（sha 门控）→ 启动/就绪/停止脚本 →
   ready.json 解析
4. **文本同步**：sha256 门控 + base64 chunk 往返 + 结果解析 +
   writeResponseFile 幂等分支

### 测试快照（R485 后）

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 951（+3） |
| pc-acpx 集成 | 581（+7） |
| pc-acpx 合计 | 1532 |
| pc-adapter-codex-local | 457 |
| pc-adapter-claude-local | 475 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **全量合计** | **2523** |

## 18. 后续计划（R486+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | pc-acpx `sandbox_callback_bridge` 完成 | 与 Node `execute.remote` 全链路对照集成（round469/477 扩展）；bridge 决策模块声明完成 |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | codex/claude 主执行流程更深接入 | bridge env 合并进执行 env、restoreWorkspace 回调、进程 session 代理 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 19. R486 增量 — bridge handle 计划 + 主执行流程接入（+15 测试）

对齐 Node `execution-target.ts` L1719-1896
（`startAdapterExecutionTargetPaperclipBridge`）与 codex/claude
`execute.ts` 的 `if (remote && usesBridge) { Object.assign(env, bridge.env) }`
分支，全部为纯决策：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `preferred_sandbox_shell` / `adapter_execution_target_shell_command` | `preferredSandboxShell` / `adapterExecutionTargetShellCommand` | ssh → `sh`；sandbox → shellCommand 决策 |
| `resolve_bridge_timeout_ms` | `bridgeTimeoutMs` | timeoutSec>0 → sec×1000；否则 sandbox timeoutMs |
| `resolve_bridge_max_body_bytes` | maxBodyBytes 归一化 | >0 取整，否则默认 256KB |
| `bridge_handle_paths` | bridgeRuntimeDir/queueDir/assetRemoteDir | join(runtimeRootDir, "paperclip-bridge", ...) |
| `build_bridge_proxy_request_plan` | worker `handleRequest` | method trim+大写默认 GET、headers 过滤空值 + Bearer + x-paperclip-run-id、forward URL、GET/HEAD 无 body、30s 超时 |
| `bridge_host_api_token_or_error` | hostApiToken 校验 | trim 空 → throw 文案逐字对齐 |
| `start_adapter_execution_target_paperclip_bridge_plan` | start 函数决策部分 | 路径/token/maxBody/url/timeout/env/has_run_log_tail 一次组装 |
| `decide_execution_bridge_plan` | execute.ts 分支 | 非远程 → None；远程+无 token → Err；否则 plan |
| `merge_bridge_handle_env` | `Object.assign(env, bridge.env)` | 覆盖合并 |
| `decide_codex_execution_bridge_plan` | codex execute.ts L891-907 | adapterKey 固定 "codex" 薄封装 |
| `decide_claude_execution_bridge_plan` | claude execute.ts L679-692 | adapterKey 固定 "claude" 薄封装 |

### 关键决策

- **run log tail 门控**：仅 sandbox transport 且 `streamRunLogs != false`
  时启用（对齐 Node `target.transport === "sandbox" && target.streamRunLogs !== false`）。
- **token 校验前置**：bridge 启动前校验 host token，缺失即报错，
  与 Node `startAdapterExecutionTargetPaperclipBridge` 内 throw 一致；
  本地执行不启动 bridge 也不校验（Node usesBridge gate 在前）。
- **代理请求头**：请求方 header 空值剔除后注入
  `authorization: Bearer <hostApiToken>` 与 `x-paperclip-run-id`，
  GET/HEAD 不携带 body，超时固定 30s。

### 测试快照（R486 后，lib 口径）

| Crate | 测试数 |
|---|---|
| pc-acpx | 960（+9） |
| pc-adapter-codex-local | 460（+3） |
| pc-adapter-claude-local | 478（+3） |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **合计** | **1957** |

## 20. 后续计划（R487+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | codex/claude 主流程 execute 编排 | 把 `decide_*_execution_bridge_plan` 接入 route 层执行环境构建（execute 全链路） |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | 进程 session 代理决策 | `writeProcessSessionProxyScript` / `syncProcessSessionRemoteScript`（execution-target.ts L1265-1420） |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 21. R487 增量 — 进程 session bridge 决策（+9 测试）

对齐 Node `execution-target.ts` L1266-1735
（`writeProcessSessionProxyScript` / `syncProcessSessionRemoteScript` /
`startAdapterExecutionTargetProcessSessionBridge` 纯决策部分）：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `PROCESS_SESSION_PROXY_SCRIPT` / `REMOTE_SCRIPT` / `AUTH_TIMEOUT_MS` | 同名常量 | 脚本名 + 鉴权超时 5s |
| `json_line` / `split_json_lines` | `jsonLine` / `splitJsonLines` | 事件流编解码（末段 rest） |
| `get_process_session_proxy_source` | `getProcessSessionProxySource` | 代理 .mjs 完整模板（port/token 插值、hello/stdin/stdinEnd/data/error/exit 协议） |
| `get_process_session_remote_source` | `getProcessSessionRemoteSource` | 远端 .mjs 完整模板（spawn、事件文件 writeChain、close 排水后 exit、stdin 轮询 50ms） |
| `sync_process_session_remote_script_plan` | `syncProcessSessionRemoteScript` | label/action/lockDir 专用 + sha256 门控 |
| `build_process_session_command_payload` | commandPayload | base64(JSON{command,args,cwd,env:sanitized}) |
| `build_process_session_bridge_start_script` | start execute 脚本 | mkdir stdin/events + env + nohup node + printf pid |
| `remote_event_socket_action` | `writeRemoteEventToSocket` | exit→End、error→Destroy、其他→Write |
| `start_adapter_execution_target_process_session_bridge_plan` | start 函数决策部分 | sandbox gate、timeout 解析、session 目录树、proxy token 18B |
| `base64_decode_utf8` | `Buffer.from(b64).toString("utf8")` | 解码（payload/事件反向） |

### 关键决策

- **脚本即数据**：proxy/remote 两个 .mjs 模板逐字对齐 Node，含
  `close`（非 `exit`）事件排水保证、`padStart(12, "0")` 事件文件序号、
  `writeChain` 串行写保证 exit 落在所有 data 之后。
- **sandbox gate**：仅 sandbox 远程 target 产生计划（对齐 Node
  `transport !== "sandbox"` → null）；timeoutSec 优先于
  `target.timeoutMs`。
- **command payload 环境消毒**：复用 `remote_execution_env` 的
  `sanitize_remote_execution_env`（身份键与 inherited 相同则剔除），
  cwd 缺省回退 `target.remoteCwd`。

### 测试快照（R487 后，lib 口径）

| Crate | 测试数 |
|---|---|
| pc-acpx | 969（+9） |
| pc-adapter-codex-local | 460 |
| pc-adapter-claude-local | 478 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **合计** | **1966** |

## 22. 后续计划（R488+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | 进程 session 收尾 | proxy 启动/监听决策（`waitForLocalServerListen`、鉴权握手 5s 超时）与 runLogTail 工厂决策 |
| P1 | codex/claude 主流程 execute 编排 | 把 bridge/process-session 计划接入 route 层执行环境构建 |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 23. R488 增量 — proxy 连接/事件决策（+8 测试）

对齐 Node `execution-target.ts` L1479-1565 的 connection handler /
`deliverRemoteEvent` / `poll` / `stop` 纯决策部分
（runLogTail 已在 R404 `sandbox_run_log_stream.rs` 复刻，本轮确认无缺口）：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `PROXY_POLL_INTERVAL_MS` | `setTimeout(..., 100)` | 事件轮询间隔 |
| `proxy_stdin_file_name` | `String(seq).padStart(12, "0") + ".json"` | 序号文件名 |
| `decide_proxy_connection_message` / `ProxyConnectionDecision` | connection handler | token 不等 → Reject；已有活跃 socket 抢占 → Reject；首次 → Authenticate；已鉴权 → Proceed |
| `proxy_connection_auth_timed_out` | authTimer | 未鉴权 5s 后 destroy |
| `parse_proxy_message_line` | `JSON.parse(line)` | 失败 → destroy |
| `build_proxy_stdin_write` | stdin/stdinEnd 分支 | `{seq:012}.json` + 事件行；其他/缺 data → None |
| `decide_proxy_poll_should_stop` | poll `return` | exit/error 停止轮询 |
| `build_proxy_stop_stdin_end_write` | stop | `{seq+1:012}.json` + stdinEnd |
| `process_session_listen_port_or_error` | `waitForLocalServerListen` | 无 TCP 端口报错文案逐字对齐 |
| `proxy_error_message_line` | catch 分支 | `{"type":"error","message":...}` 写 socket |
| `decide_remote_event_delivery` | `deliverRemoteEvent` | 有 socket → 直写（exit End / error Destroy）；无 → 缓冲 + 停止标记 |

### 关键决策

- **连接独占**：未鉴权连接在已有活跃 socket 时被拒绝（"Connections own
  the session"），避免多连接竞态抢占事件流。
- **缓冲排水**：socket 建立前的事件入 pending 队列，首次鉴权成功后
  flush（`Authenticate` 语义由执行器承接）。
- **stop 排水**：停止时补写 `stdinEnd`（序号 +1），保证远端 stdin
  轮询退出，与 R487 的 writeChain/close 排水闭环。
- **JSON 键顺序**：serde_json 默认按键排序，与 Node `JSON.stringify`
  插入序不同但协议等价；测试按解析后断言。

### 测试快照（R488 后，lib 口径）

| Crate | 测试数 |
|---|---|
| pc-acpx | 977（+8） |
| pc-adapter-codex-local | 460 |
| pc-adapter-claude-local | 478 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **合计** | **1974** |

## 24. 后续计划（R489+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | 进程 session 完成 | 集成测试串全链路（plan → proxy 源码 → 连接决策 → stdin 写入 → 事件投递 → stop）；`AdapterExecutionTargetProcessSessionBridgeHandle` 组装 |
| P1 | codex/claude 主流程 execute 编排 | 把 bridge/process-session 计划接入 route 层执行环境构建 |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 25. R489 增量 — 进程 session 全链路集成 + handle 组装（+4 集成）

对齐 Node `startAdapterExecutionTargetProcessSessionBridge` 的完整
决策主流程，`tests/round489_process_session_bridge.rs` 串起 R487-R488：

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `build_process_session_bridge_handle` | `{ agentCommand, stop }` | agentCommand + has_stop 能力位组装 |

### 集成测试（4 测试）

1. **启动计划全链路**：sandbox gate → 目录树（join 链）→ timeout →
   proxy token 24 chars → command payload 往返（cwd 回退 remoteCwd、
   env 消毒）→ start 脚本（mkdir/env/nohup/printf pid）→ 远端脚本
   同步计划（sha 门控 + 专用 lock）
2. **proxy 源码 + 写盘**：模板协议片段逐项断言
   （hello/stdin/stdinEnd/data/error/exit），写盘路径对齐
   `join(dir, PROCESS_SESSION_PROXY_SCRIPT)`（执行器 0o700）
3. **连接握手全链路**：hello 鉴权接管 → 第二个连接抢占 Reject →
   已鉴权 Proceed → stdin 事件流（序号文件名 + base64 data）→
   事件投递（data 直写 / 缓冲 exit 停止轮询）
4. **stop + handle**：stdinEnd 序号 +1 补写、端口校验、错误消息、
   handle 组装（agentCommand 路径）

### 测试快照（R489 后）

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 977 |
| pc-acpx 集成 | 585（+4） |
| pc-acpx 合计 | 1562 |
| pc-adapter-codex-local | 460 |
| pc-adapter-claude-local | 478 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **全量合计** | **2559** |

## 26. 后续计划（R490+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | codex/claude 主流程 execute 编排 | 把 bridge/process-session 计划接入 route 层执行环境构建（execute 全链路编排决策） |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | `git_workspace_sync` 对照 | importGitWorkspaceToSsh / exportGitWorkspaceFromSsh 与 Node 逐函数对照补缺 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 27. R490 增量 — 执行 env 与 bridge env 合并接入主流程（+19 测试）

对齐 Node codex `execute.ts` L891-907 / claude `execute.ts` L679-692：
远程 + usesBridge 时启动 paperclip bridge 并把 bridge env
（`PAPERCLIP_API_URL` / `PAPERCLIP_API_KEY` / `PAPERCLIP_API_BRIDGE_MODE` /
`PAPERCLIP_BRIDGE_QUEUE_DIR`）合并进子进程 env，缺失 host token 报错
（Node throw），本地原样返回。真实 bridge server/worker 执行器仍留待
P2（本轮只做 env 合并决策 + 启动日志行）。

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `merge_execution_bridge_env`（pc-acpx，两 adapter 共用） | `startAdapterExecutionTargetPaperclipBridge` 调用分支 | 纯决策：base env + bridge env 合并（Object.assign 语义）+ 启动日志行 |
| `build_codex_execution_env`（codex-local） | codex execute.ts L891-907 | adapterKey 固定 `"codex"`，接入 `execute` 首轮 + resume 重试 |
| `build_claude_execution_env`（claude-local） | claude execute.ts L679-692 | adapterKey 固定 `"claude"`，接入 `execute_with_resume_retry` |

### 决策语义（对齐 Node）

1. **gate**：`adapterExecutionTargetUsesPaperclipBridge`（仅远程
   SSH/Sandbox）为 false → 返回 base env 原样
2. **host token**：从 `base_env.PAPERCLIP_API_KEY` 读取；缺失 →
   `Sandbox bridge mode requires a host-side Paperclip API token.`
   （映射 `AdapterError::InvalidConfiguration`）
3. **host_api_url 解析**：显式覆盖 > `PAPERCLIP_RUNTIME_API_URL` >
   `PAPERCLIP_API_URL` > `http://localhost:3100`（对齐 Node
   `process.env` 解析）
4. **合并**：bridge 4 键覆盖同名键（`Object.assign(env, bridge.env)`）
5. **日志**：`[paperclip] Starting sandbox callback bridge for <key> in
   <bridgeRuntimeDir>.` 在进程启动前经 stdout 事件发出

### 测试分布（R490 增量）

- pc-acpx `execution_target` 单测 +5：本地原样 / None target / 远程
  4 键合并 + 日志行 / runtime URL 优先 / 缺 token 报错
- pc-acpx 集成 `tests/round490_execution_env_bridge.rs` +4：codex 与
  claude 全链路断言（合并键、日志行、timeout 透传、URL 解析）
- codex-local `codex_execution_env.rs` +5：本地 / 缺 target / 远程合并
  （len=6）/ 缺 token / 日志行 + timeout
- claude-local `claude_execution_env.rs` +5：同上（adapterKey=claude）

### 测试快照（R490 后）

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 982（+5） |
| pc-acpx 集成 | 589（+4） |
| pc-acpx 合计 | 1571 |
| pc-adapter-codex-local | 465（+5） |
| pc-adapter-claude-local | 483（+5） |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **全量合计** | **2578** |

## 28. 后续计划（R491+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | bridge 真实执行器 | 把 `startSandboxCallbackBridgeServer` / `startSandboxCallbackBridgeWorker`（pc-acpx `sandbox_callback_bridge`）接入 codex/claude execute：真实启动编排 + teardown（stop 命令 / 队列清理） |
| P2 | pc-acpx `ssh.rs` 执行器 | syncDirectoryToSsh / restoreWorkspaceFromSshExecution 真实 I/O 封装 |
| P2 | `git_workspace_sync` 对照 | importGitWorkspaceToSsh / exportGitWorkspaceFromSsh 与 Node 逐函数对照补缺 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 29. R491 增量 — bridge 真实执行器（+5 测试，真实 node 端到端验证）

把 R480-R485 的 bridge 决策串成 Node `sandbox-callback-bridge.ts` /
`execution-target.ts` 的真实 I/O 编排，新增 `pc-acpx::bridge_executor`：

| Rust 组件 | Node 对应 | 说明 |
|---|---|---|
| `BridgeCommandRunner` + `LocalProcessBridgeRunner` | `CommandManagedRuntimeRunner` | 远程命令执行抽象 + 本地进程实现（spawn / stdin / 超时 kill） |
| `RunnerBridgeQueueClient` | `createCommandManagedSandboxCallbackBridgeQueueClient` | 队列操作全部复用 R484 决策脚本（mkdri / list / base64 读 / 三段式写 / 响应锁 / rename / remove） |
| `BridgeAsset::create` + `get_sandbox_callback_bridge_server_source` | `createSandboxCallbackBridgeAsset` + `getSandboxCallbackBridgeServerSource` | 远端 server 源码从 Node 模板逐字节转录（`assets/paperclip-bridge-server.mjs`，占位符运行期替换，已用 Node 求值验证字节一致） |
| `start_bridge_server` | `startSandboxCallbackBridgeServer` L947-1094 | entrypoint sha 门控同步 → start 脚本（nohup node）→ ready 轮询（200×0.05s）→ ready.json 解析 → stop（kill pid + 清理） |
| `start_bridge_worker` + `BridgeWorkerHandle::stop` | `startSandboxCallbackBridgeWorker` | 队列目录创建 → 轮询循环（复用 R481-R482 决策）→ 400/403/502/503 响应 → stop（drain 2s + failPending 补写 503） |
| `BridgeForwardHandler` | execute.ts L1802-1834 fetch 转发 | 30s 超时、Bearer + x-paperclip-run-id、GET/HEAD 无 body、headers 透传（content-type/etag/last-modified）、body 限额 |
| `start_adapter_execution_target_paperclip_bridge` | execution-target.ts L1719-1930 | gate → token 校验 → asset → worker → server → env 4 键 + 启动日志 → teardown（server stop + worker stop + asset 清理） |

### 真实验证（`tests/round491_bridge_executor.rs`，真实 node 进程）

1. **全链路 round-trip**：本地 `sh` runner 模拟远端 → 真实 `node` 启动
   bridge server → 代理 POST/GET 到本地 echo 服务器 → 200 + body 回显 +
   etag 透传；错误 token 401；非 JSON 415；teardown 后 pid/ready 文件清理、
   队列无残留
2. **worker 语义**：坏 JSON → 400；路由拒绝 → 403；允许路由 → 200；
   stop 时未决 + 在途请求 → failPending 补写 503（幂等检查阻止 handler
   迟到的 200 覆盖，对齐 Node）
3. **转发 handler 单测**：URL 组装（path + query）、allowlist header
   过滤、响应透传

### 测试快照（R491 后）

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 984（+2） |
| pc-acpx 集成 | 592（+3） |
| pc-acpx 合计 | 1576 |
| pc-adapter-codex-local | 465 |
| pc-adapter-claude-local | 483 |
| pc-activity | 14 |
| pc-adapter-process | 6 |
| pc-adapter-quota | 39 |
| **全量合计** | **2583** |

## 30. 后续计划（R492+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | SSH runner 真实实现 | 为 `BridgeCommandRunner` 提供 SSH 实现（`ssh` 子进程封装，对齐 Node `createCommandManagedSandboxCallbackBridgeQueueClient` 的 SSH runner），并把 `start_adapter_execution_target_paperclip_bridge` 接入 codex/claude execute（替换 R490 的 env-only 合并） |
| P2 | sandbox run log tail | `streamRunLogs` 的 run log tail 工厂（`sandbox_run_log_stream`）接入 bridge 编排 |
| P2 | `git_workspace_sync` 对照 | importGitWorkspaceToSsh / exportGitWorkspaceFromSsh 与 Node 逐函数对照补缺 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 31. R492 增量 — SSH runner 真实实现 + bridge 接入 codex/claude execute（+23 测试）

把 R491 留下的 P1 落地：为 `BridgeCommandRunner` 提供真实 SSH 实现，并把
`start_adapter_execution_target_paperclip_bridge` 接入 codex / claude 的
execute（替换 R490 的 env-only 合并），全部用真实 sshd 端到端验证。

### 新增组件（`pc-acpx::ssh`，对齐 Node `ssh.ts`）

| Rust 组件 | Node 对应 | 说明 |
|---|---|---|
| `SshAuthArgs::create` + `write_temp_secure_file` | `createSshAuthArgs` + `withTempFile` | BatchMode=yes / ConnectTimeout=10 / StrictHostKeyChecking；known_hosts 与 private key 写入 0600 临时文件（`UserKnownHostsFile` / `-i`），Drop 自动清理（Node 的 cleanup 语义） |
| `build_ssh_login_script` | runSshCommand 的 remoteScript | `/etc/profile` → `~/.profile` → `.bash_profile`/`.bashrc` → `.zprofile` 登录 profile sourcing，再 `exec env K=V sh -c '<cmd>'`（用户注入 env 覆盖 profile 重导出值） |
| `run_ssh_command` + `spawn_ssh_capture` | `runSshCommand` + `spawnText`/`execFileText` | stdin 管道、超时（SIGTERM → 5s 宽限 → SIGKILL 升级）、maxBuffer 上限 kill、失败携带 stdout/stderr/exitCode/signal/timedOut（对齐 Node error 属性） |
| `SshCommandManagedRuntimeRunner` | `createSshCommandManagedRuntimeRunner` | 实现 `BridgeCommandRunner`：`sh`/`bash` + `-c`/`-lc` → export 前缀；其他命令 → `env` 前缀 + `exec`；`cd <cwd>` 前缀；失败 exit_code/signal/timed_out 精确传播 |
| `build_ssh_spawn_target` + `SshSpawnTarget` | `buildSshSpawnTarget` | 进程 session bridge 启动目标（`ssh` 命令 + auth args + `-p <port> user@host sh -c '<script>'`），本轮预留，接入后续轮次 |

### bridge 接入（对齐 Node execute.ts L891-907 / claude L679-692）

- `pc-acpx::bridge_executor::start_adapter_execution_bridge_for_target`：
  按 target 选 runner —— SSH target → 真实 `SshCommandManagedRuntimeRunner`
  启动完整 bridge；Sandbox target → `Ok(None)`（provider runner 未实现，
  保持 R490 env-only 合并）；本地 → `Ok(None)`
- `pc-adapter-codex-local::codex_bridge_env::start_codex_execution_bridge` /
  `pc-adapter-claude-local::claude_remote_workspace::start_claude_execution_bridge`：
  从 `base_env` 提取 `PAPERCLIP_API_KEY`（缺失报错，Node throw 语义）与
  host API URL，启动真实 bridge，返回 `StartedAdapterBridge` 供 teardown
- codex `execute()` / claude `execute_with_resume_retry()`：
  bridge 启动后用真实 bridge env 覆盖 4 键（`Object.assign(env,
  paperclipBridge.env)` 语义），emit `[paperclip] Sandbox ACP API callback
  bridge enabled for this run.`；执行结束后统一 `bridge.stop()`（对齐 Node
  `cleanupRemoteBridges`：server stop → worker drain → asset 清理），错误路径
  同样不泄漏 bridge

### 真实验证

1. **`tests/round492_ssh_runner.rs`（+7，真实 sshd fixture）**：本机
   `/usr/sbin/sshd` 随机 loopback 端口 + 生成 ed25519 密钥 + known_hosts
   （对齐 Node `startSshEnvLabFixture`；就绪轮询 10s）
   - `run_ssh_command`：echo + env 注入、stdin 管道往返、超时 kill
     （sleep 30 / 1.5s → timed_out）、maxBuffer 溢出
   - `SshCommandManagedRuntimeRunner`：`sh -c` + export 前缀、非 shell 命令
     （pwd → remoteCwd）、exit 7 传播、stdin 管道
   - **bridge 全链路**：SSH runner + 真实 sshd + 真实 node bridge server →
     代理 POST 200 + 回显 + etag、401、GET 转发、teardown 后
     server.pid / ready.json 清理 + 队列经 SSH `find` 验证零残留
2. **adapter 集成（codex +4 / claude +2，真实 sshd + node）**：
   `start_*_execution_bridge` SSH target → 真实 bridge（adapter key 日志
   `codex`/`claude`、env 4 键、转发可达、teardown 队列清理）；sandbox /
   local / 缺 target → `Ok(None)`；缺 `PAPERCLIP_API_KEY` → 不触发（sandbox
   env-only 路径不校验，真实启动路径由 pc-acpx 校验）

### 测试快照（R492 后）

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 993（ssh 模块 33，+14） |
| pc-acpx 集成（48 suites） | 1592（+round492 7 个） |
| pc-adapter-codex-local | 469（+4） |
| pc-adapter-claude-local | 485（+2） |
| **本仓合计（workspace 除 postgres 依赖套件）** | **2546** |

注：`pc-heartbeat::round300_stale_issue_lock_sweep` 并行跑共享本地
postgres 时有测试隔离问题（`--test-threads=1` 下 5/5 通过），与 R492
改动无关（该 crate 不依赖 pc-acpx）。

## 32. 后续计划（R493+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | process session bridge 接入 | 用 `build_ssh_spawn_target` 把 `start_adapter_execution_target_process_session_bridge`（R487 决策）接入 codex/claude execute，与 paperclip bridge 并发启动（对齐 Node `settleRemoteBridgeStarts`），teardown 双 bridge 全停 |
| P2 | sandbox run log tail | `streamRunLogs` 的 run log tail 工厂（`sandbox_run_log_stream`）接入 bridge 编排（`has_run_log_tail` 已就位） |
| P2 | `git_workspace_sync` 对照 | importGitWorkspaceToSsh / exportGitWorkspaceFromSsh / `syncDirectoryToSsh` 与 Node 逐函数对照补缺（tar 流 + 进度上报） |
| P2 | SSH env-lab fixture 上移 | `SshLabFixture` 从各测试文件收敛为共享测试支持模块，供 git sync / log tail 轮次复用 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 33. R493 增量 — 进程 session bridge 真实执行器 + 接入 codex/claude execute（+20 测试）

### 新增组件（`pc-acpx::process_session_bridge`，对齐 Node execution-target.ts L1360-1578）

- `start_adapter_execution_target_process_session_bridge`：sandbox gate →
  远端脚本 sha 门控同步（`sync_process_session_remote_script_plan`，内容 hash
  相同跳过 base64 上传）→ 启动日志 → 启动执行（`mkdir -p stdin/events` +
  `PAPERCLIP_PROCESS_SESSION_DIR` / `_COMMAND_B64` 环境注入 + `nohup node
  <remote script>`）→ 本地 TCP server（127.0.0.1 随机端口）→ proxy 脚本
  （mkdtemp + 0700）→ 队列客户端（stdin 写 / events 读都走 runner）
- `ProcessSessionBridgeState`：`shutdown_notify: tokio::sync::Notify` 唤醒
  全部连接任务退出（对齐 Node stop 的 liveSockets destroy）
- `handle_connection`：`Authenticate` 时把 write_half 移入 state；坏 JSON /
  错误 token 立即 drop 写半部断开连接；退出时活跃连接清空 write_half；
  缓冲的 exit/error 行在接管后 flush 并 shutdown socket（对齐
  `writeRemoteEventToSocket`）
- `poll_events`：list events → 读 + remove → deliver；exit/error 停止轮询
- `stop`：stopping → abort poll/server 任务 → notify_waiters → shutdown 活跃
  socket → 补写 stdinEnd（`{stdinSeq+1}.json`）→ 移除远端 session 目录 →
  清理本地 proxy 目录

### adapter 接入（对齐 Node execute.ts L1944-2010 `settleRemoteBridgeStarts`）

- `codex_bridge_env::use_codex_remote_process_session` /
  `claude_remote_workspace::use_claude_remote_process_session`：Node gate
  全语义（remote + sandbox + runner + agentCommandShell）；Rust sandbox
  target 尚无 provider runner，execute 调用时 runner 位恒 false → 默认
  不触发，代码路径保留
- `start_codex_process_session_bridge` / `start_claude_process_session_bridge`：
  sandbox + runner → 真实启动（launch 形状对齐 Node：`command: "sh"`、
  `args: ["-lc", "exec <agentCommandShell>"]`、`cwd: sessionCwd`）；无 runner /
  非 sandbox → `Ok(None)`
- codex `execute()` / claude `execute_with_resume_retry()`：paperclip bridge
  env 合并后按 gate 启动 process session bridge（启动 env 等价于 Node env
  thunk 求值结果）；teardown 对齐 `cleanupRemoteBridges` 双停顺序
  （`allSettled([processSessionBridge?.stop(), paperclipBridge?.stop()])`）

### 真实验证

1. **`tests/round493_process_session_bridge.rs`（pc-acpx +6）**：真实 node
   远端脚本 + 真实 TCP server + 真实 proxy 脚本跑通双向桥接（stdin →
   stdout/stderr 回显）、输出缓冲（child 提前退出后 proxy 仍收到 flush）、
   exit 7 传播、错误 token 连接被立即断开、stop 清理（session 目录仅统计
   子目录 / proxy 目录 / 连接全清）、SSH gate → `Ok(None)`
2. **adapter 集成（codex +3 / claude +3，真实 node + 本地 sandbox fixture）**：
   `start_*_process_session_bridge` sandbox target + `LocalProcessBridgeRunner`
   → 真实 bridge：proxy 双向桥接 + stop 清理；无 runner / SSH target /
   空 agentCommandShell → `Ok(None)`；launch env 注入 `HOME` 指向沙箱目录，
   隔离宿主 `~/.profile` 登录 shell 噪音

### 测试快照（R493 后）

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 993 |
| pc-acpx 集成 | 1598（+round493 6 个） |
| pc-adapter-codex-local | 476（+7：gate/回退单测 4 + 集成 3） |
| pc-adapter-claude-local | 492（+7：gate/回退单测 4 + 集成 3） |
| **本仓合计（workspace 除 postgres 依赖套件）** | **3559（+20）** |
## 34. R494 增量 — run log tail 接入 bridge 编排 + `runAdapterExecutionTargetProcess` 三分支（+5 测试）

### 新增组件（`pc-acpx::execution_target_process`，对齐 Node execution-target.ts L570-630）

- `RunProcessResult` / `RunAdapterExecutionTargetProcessOptions`：
  对齐 Node 同名结构（exit code / signal / timed-out / stdout / stderr /
  started_at + cwd / env / stdin / timeoutSec / graceSec / onLog /
  runLogTail / runner / killFlag）
- `run_adapter_execution_target_process` 三分支 dispatch：
  - **sandbox**：`options.runner.execute` + run log tail
    `create → wrap_command → start → execute → finish → abort`
    集成（对齐 Node L575-617）
  - **ssh**：`build_ssh_spawn_target` → 本地 `ssh` 进程流式 spawn（对齐
    `runChildProcess` + `resolveSpawnTarget` 的 `remoteExecution` 分支）
  - **local**（或 absent）：直接本地 spawn
- `spawn_stream_capture`（local + ssh 共用）：`tokio::process` + process
  group（detach + pgid = pid）+ stdin pipe + 双 reader 流式 `on_log` +
  50ms tick 监控 timeout/grace + kill_flag + 进程组 SIGTERM → grace →
  SIGKILL + 双流捕获（`append_with_byte_cap`）
- `make_tail_sink`：把 `(stream, chunk)` on_log 适配成
  `SandboxRunLogTailHandle.start` 的 `SandboxRunLogSink`（`BoxFuture`）

### bridge 编排接入（Node L1848-1866）

- `pc_acpx::sandbox_run_log_stream::adapt_bridge_runner`：
  `BridgeCommandRunner → SandboxRunLogRunner` 包装（绕开 Arc trait-object
  unsize 限制——`Arc<dyn A>` 不会自动转为 `Arc<dyn B>` 即使有 blanket impl）
- `StartedAdapterBridge.run_log_tail: Option<Arc<SandboxRunLogTailFactory>>`：
  paperclip bridge start 在 sandbox target + `streamRunLogs !== false`
  时用 `adapt_bridge_runner` 适配 runner + 从 `<queue_dir>/logs` 组装
  `logs_dir` + `preferred_shell_for_sandbox` shell + 触发
  `[paperclip] Sandbox run log streaming enabled for this run.` 日志

### 真实验证（`tests/round494_execution_target_process.rs`，pc-acpx +5）

- **local echo**：捕获 stdout + exit 0 + on_log 流式 chunk 到达
- **local timeout**：0.3s timeout + 0.2s grace → SIGTERM 升级 SIGKILL；
  `timed_out` 标记 + signal label（SIGTERM / SIGKILL）
- **local kill_flag**：150ms 后翻 flag → SIGTERM + `timed_out=true`
- **sandbox run-log-tail**：LocalProcessBridgeRunner + 真实 node child
  (`fs.openSync + writeSync`) 在 sandbox `<logs_dir>` 写 5 行日志，
  真实 tail poll loop 通过 on_log 流式推送（对齐 Node `runLogTail.start`
  → `writeRemoteEventToSocket`）
- **ssh remote command**：真实 sshd fixture + `build_ssh_spawn_target` →
  本地 spawn `ssh -p <port> user@host sh -c 'cd <cwd> && exec <cmd>'`，
  流式 on_log + 远端 echo 通过 stdout 回传

### 测试快照（R494 后）

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 993 |
| pc-acpx 集成 | 1603（+round494 5 个） |
| pc-adapter-codex-local | 476 |
| pc-adapter-claude-local | 492 |
| **本仓合计（workspace 除 postgres 依赖套件）** | **3564（+5）** |

## 35. 后续计划（R495+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | 远程 CLI 执行接入 | codex/claude execute 对 SSH target 改走 `run_adapter_execution_target_process` 的 ssh 分支（替代当前 `execute_process_capture` 本地执行），output inactivity monitor 复用现有 kill_flag 路径；sandbox target 因 provider runner 缺失暂保持本地执行 |
| P2 | `git_workspace_sync` 对照 | importGitWorkspaceToSsh / exportGitWorkspaceFromSsh / `syncDirectoryToSsh` 与 Node 逐函数对照补缺（tar 流 + 进度上报） |
| P2 | SSH env-lab fixture 上移 | `SshLabFixture` 从各测试文件收敛为共享测试支持模块，供 git sync 轮次复用 |
| P2 | `round493_sandbox_test` 拓扑 | pc-acpx round493 + adapter round493 的本地 sandbox fixture + 真实 node/sshd 测试拓扑收敛为共享 helper，减少测试样板 |
| P3 | provider runner | sandbox provider runner（E2B/Daytona 等）接入后，bridge / process session bridge / run log tail 真正并发生效，codex/claude execute 按 gate 切换到 sandbox + ssh 分支 |
| P3 | 其他 adapter | hermes/cursor-cloud/openclaw/gateway 延后（用户约束） |

## 36. R495 完成：codex / claude CLI 改走 execute_command_for_target

### 动机

R494 引入 `execute_command_for_target(spec_program, spec_args, ...)` 作为
remote-runtime 三分支 dispatch 入口（ssh 分支、sandbox fallback、本地）。
R495 让 codex / claude 的实际 CLI 执行都改走这个新 helper，使远程
ssh/sandbox 路径生效，同时把 output inactivity monitor 的 kill_flag 信号
合入新 helper 的超时/中止语义。

### 关键改动

1. `crates/pc-acpx/src/execution_target_process.rs`：
   - 新增 `RunProcessResult { exit_code, signal, timed_out, killed_by_flag,
     spawned_pid, stdout, stderr, started_at }`（对齐 Node `RunProcessResult`
     shape）
   - `spawn_stream_capture` 50ms tick 同时监控 timeout/grace 与 kill_flag；
     timeout 触发时优先检查 kill_flag（race window：上一次 tick → 当前
     tick 之间翻转）→ `killed_by_flag=true, timed_out=false`
   - 三分支 dispatch（`fallback = !wants_remote || !has_runner`）：
     sandbox-without-runner 走本地分支并 emit 一行 one-time note

2. `crates/pc-adapter-codex-local/src/lib.rs`：
   - `execute_codex_with_monitor` 替换为：
     - 不启用 monitor：`execute_command_for_target` 直跑
     - 启用 monitor：spawn `RunningMonitor`（250ms tick）+ on_log 闭包
       同时 emit `AdapterEvent::Output(stdout/stderr)` 与
       `monitor.note_output_chunk`
   - `execute_codex_with_monitor` 提升为 `pub`（供 round495 集成测试）
   - `RunProcessResult → StreamingProcessExecution` 转换
     (`run_process_result_to_streaming`)，保留 exit_code / signal /
     timed_out 字段
   - `killed_by_flag` 分支：构造 `error_message` 走
     `format_output_inactivity_monitor_error_message`（对齐 Node
     `formatOutputInactivityMonitorErrorMessage`），不再用硬编码字符串

3. `crates/pc-adapter-claude-local/src/claude_resume_loop.rs`：
   - 新增 helper `execute_claude_attempt_for_target(command, args, stdin,
     context, events)` 包装 `execute_command_for_target`，on_log 同样 emit
     `AdapterEvent::Output(stdout/stderr)`
   - 替换 `run_resume_retry_loop` 中两处 `execute_process_capture`（initial
     + retry）；Claude 无 output inactivity monitor，kill_flag=None

4. 测试：
   - `monitor_fires_and_kills_silent_process` 修复：原断言硬编码
     `"killed by output inactivity monitor"`（早期字符串），新 helper 输出
     对齐 Node 的 `"monitor: no codex activity (output or process) for
     {m}m {s}s"`；改为前缀匹配避免 timer jitter 敏感
   - `crates/pc-adapter-codex-local/tests/round495_codex_remote_execute.rs`
     新增：真实 sshd fixture + SSH target + `/bin/sh -c 'printf <marker>'`
     占位命令，验证 `execute_codex_with_monitor` 经 SSH 分支把远端 stdout
     拉回 `StreamingProcessExecution.stdout` 与 `AdapterEventSink`

### 真实验证

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 993 |
| pc-acpx 集成 | 1603 |
| pc-adapter-codex-local lib | 390（+1 fix） |
| pc-adapter-codex-local 集成 | 477（+1 round495） |
| pc-adapter-claude-local | 421 |
| **本仓合计** | **3884** |

### 后续计划（R496+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | `execute_process_capture` 退役 | `crates/pc-adapter-claude-local/src/lib.rs:741` 主 `execute()` 仍用旧 `pc_adapter_process::execute_process_capture`；下一轮迁到 `execute_command_for_target` 让 Claude 主路径也享受三分支 dispatch |
| P1 | provider runner | sandbox target 的 `provider_runner` 字段接入；接入后 sandbox 不再 fallback 到本地分支 |
| P2 | `git_workspace_sync` 对照 | 与 Node 逐函数对照补 `importGitWorkspaceToSsh` / `exportGitWorkspaceFromSsh` / `syncDirectoryToSsh`（tar 流 + 进度上报） |
| P2 | SSH fixture 上移 | `SshLabFixture` 从 round492 / round495 / round494 三处收敛为共享 helper，供 git sync / bridge 轮次复用 |
| P2 | monitor chunk 抖动 | 当 child 退出前最后一刻 stdout 到达但 tick 已超时，存在 < 50ms race：monitor fire 但 on_log note_output_chunk 已被 child exit 跳过 → outcome 已被记入，OK；如发现 monitor fire 后 stdout 仍回流到 result.stdout 可引入 explicit drain 阶段 |
| P3 | 其他 adapter | hermes / cursor-cloud / openclaw / gateway 延后（用户约束：adapter 优先只做 claude-local / codex-local） |

## 37. R496 完成：Claude 主 execute() 迁到 execute_command_for_target

### 动机

R495 把 `claude_resume_loop::run_resume_retry_loop`（resume retry 路径）迁到了
`execute_command_for_target`，但 `ClaudeLocalAdapter::execute()`（主执行路径，
被 route 层通过 `AdapterRegistry::execute` 调用）仍走旧的
`pc_adapter_process::execute_process_capture`。这意味着生产路径对 SSH 远端 target
完全无感 — 没有三分支 dispatch，ssh/sandbox 配置无法生效。

### 关键改动

1. `crates/pc-adapter-claude-local/src/claude_resume_loop.rs`：
   - 把 `execute_claude_attempt_for_target` 从 `async fn` 提升为
     `pub(crate) async fn`，供 `lib.rs::execute()` 共享。

2. `crates/pc-adapter-claude-local/src/lib.rs::execute()`：
   - 删除 `ProcessSpec::new(...) + execute_process_capture(...)` 两步
   - 改为 `execute_claude_attempt_for_target(command, args, stdin, context, events)`
   - `stdin` 按 prompt 是否空区分 `Some(prompt.as_str()) / None`
   - 删除 `use pc_adapter_process::{execute_process_capture, ProcessSpec};`

3. 测试：
   - 新增 `crates/pc-adapter-claude-local/tests/round496_claude_remote_execute.rs`：
     真实 sshd fixture + SSH target + `/bin/echo` 占位命令，验证
     `ClaudeLocalAdapter::execute()` 经 SSH 分支把远端 exit_code=0、
     provider="claude_local"、billing_type 填充、result_json 已构造。
   - 测试覆盖 `Adapter` trait 的 `execute` 入口（实际生产调用路径）。

### 真实验证

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 993 |
| pc-acpx 集成 | 1607 |
| pc-adapter-codex-local | 477 |
| pc-adapter-claude-local lib | 421 |
| pc-adapter-claude-local 集成 | 493（+1 round496） |
| **本仓合计** | **3991** |

### 后续计划（R497+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | `execute_process_capture` 退役 | codex-local / 其他 adapter 检查是否还有 `execute_process_capture` 直接调用，统一改走 `execute_command_for_target` |
| P1 | provider runner 接入 | sandbox target 的 `provider_runner` 字段接入；接入后 sandbox 不再 fallback 到本地分支 |
| P2 | `git_workspace_sync` 复刻 | `importGitWorkspaceToSsh` / `exportGitWorkspaceFromSsh` / `syncDirectoryToSsh`（tar 流 + 进度上报） |
| P2 | `SshLabFixture` 上移 | round492 / round494 / round495 / round496 四处各有一份；上移到 `pc-acpx::test_support` 或 `tests/common` 共享 |
| P2 | Claude `execute_with_resume_retry` 接入生产路径 | 当前只有 `execute()` 被 route 调用；`execute_with_resume_retry`（含 bridge 启动 + session resume 重试）只被测试覆盖 — 接入生产路径后才能在 Claude 走完 bridge + resume + retry 完整流程 |
| P3 | 其他 adapter | hermes / cursor-cloud / openclaw / gateway 延后（用户约束：adapter 优先只做 claude-local / codex-local） |

## 38. R497 完成：git-workspace-sync.ts 基础模块端口

### 动机

Node `packages/adapter-utils/src/git-workspace-sync.ts`（433 行）的核心被
`importGitWorkspaceToSsh` / `exportGitWorkspaceFromSsh` 依赖：snapshot reader
+ local git 执行 + ref 删除。Rust 端 `pc-acpx/src/git_workspace_sync.rs`
只有 R399 的纯 helper（ref name 生成 + script builder），缺 async 部分。
不先端口基础，import/export 无法真实验证。

### 关键改动

`crates/pc-acpx/src/git_workspace_sync.rs` 新增：

1. `GitCommandResult` struct — stdout/stderr 包装
2. `RunLocalGitError` enum — `Timeout { timeout_ms }` / `NonZeroExit` /
   `OutputOverflow { max_buffer_bytes }` / `Spawn(io::Error)`
3. `run_local_git(local_dir, args, timeout_ms, max_buffer_bytes)` — `tokio::process::Command`
   + 并发读 stdout/stderr（Vec 上限强制），timeout 经 `tokio::time::timeout`，
   溢出时返回 `OutputOverflow`，保留 stderr 给上层做 prerequisite 检测
4. `GitWorkspaceSnapshot` struct — head_commit / branch_name / overlay_paths /
   deleted_paths / ignored_paths（对齐 Node `GitWorkspaceSnapshot`）
5. `read_git_workspace_snapshot(local_dir)` — `rev-parse --is-inside-work-tree`
   非零退出（"fatal: not a git repository"）→ `Ok(None)`，与 Node 调用方约定
   一致；然后并发跑 `rev-parse HEAD` / `--abbrev-ref HEAD` / diff-ACMRTUXB /
   `ls-files --others --exclude-standard` / diff-D / `status --ignored --porcelain`
   6 个 git 命令，组合成 snapshot
6. `delete_local_git_ref(local_dir, ref)` — best-effort，错误吞掉（对齐
   Node `.catch(() => undefined)`）
7. `read_git_workspace_snapshot_path(&Path)` — 便捷 Path 重载

### 真实验证

- 10 既有 unit 测试全过
- 6 新增 async 集成测试用真实 git CLI（`git init` / `commit` / `config`）：
  - `run_local_git_returns_stdout_for_rev_parse` — SHA-1 长度 40
  - `run_local_git_returns_non_zero_exit_error_on_bad_command` — `NonZeroExit` + stderr 非空
  - `read_git_workspace_snapshot_returns_none_for_non_git_dir` — `Ok(None)` 语义
  - `read_git_workspace_snapshot_reads_head_for_clean_repo` — head_commit 长度 40
  - `read_git_workspace_snapshot_picks_up_overlay_paths` — overlay 同时包含 untracked + modified
  - `delete_local_git_ref_swallows_errors` — 不存在的 ref 不报错

### 测试快照

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 999（+6 R497） |
| pc-acpx 集成 | 1607 |
| pc-adapter-codex-local | 477 |
| pc-adapter-claude-local | 914（lib 421 + 集成 493） |
| **本仓合计** | **3997** |

### 后续计划（R498+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | R498：port `importGitWorkspaceToSsh` / `exportGitWorkspaceFromSsh` | 在 R497 基础上加 `streamLocalFileToSsh` / `streamSshToLocalFile` + bundle transfer；进度上报留到 R499 |
| P2 | R499：`createTransferProgress` 进度上报 | 真实百分比 / 字节速率 / 阶段标签，对齐 Node `runtime-progress.ts` |
| P2 | `SshLabFixture` 上移 | round492 / round494 / round495 / round496 四处各有一份；上移为共享 helper |
| P2 | `integrateImportedGitHead` | Node L916-1000（concurrent ref retry + merge-tree），R498 之后 |
| P2 | `restoreWorkspaceFromSshExecution` | Node L1559-1650（pick import/export/reset 策略），R500+ |
| P3 | 其他 adapter | cursor / gemini / grok / pi / hermes / openclaw 延后（用户约束） |

## 39. R498 完成：importGitWorkspaceToSsh / exportGitWorkspaceFromSsh 端口

### 动机

Node `packages/adapter-utils/src/git-workspace-sync.ts` 的 import/export 函数
（~165 行）调用 `streamLocalFileToSsh` / `streamSshToLocalFile`（在 ssh.ts 中，
~110 行）。R497 已端口基础 snapshot reader + run_local_git；本轮补
stream helpers + import/export 本身，让 SSH 远端 workspace 同步在 Rust 端
**真正能跑通**——之前两函数都被 `claude_remote_workspace` /
`codex_remote_workspace` 的注释声明 TODO，但实际调用落空。

### 关键改动

1. **`stream_local_file_to_ssh`**（port of Node `streamLocalFileToSsh`）：
   - tokio::process::Command spawn `ssh ... sh -c <script>`，stdin 管道
   - tokio::fs::File → tokio::io::copy → child.stdin
   - drop stdin（EOF）→ child.wait → 检查 status
   - 文件不存在 / 权限 / spawn / 非零退出各自错误（`SshStreamError` enum）

2. **`stream_ssh_to_local_file`**（port of Node `streamSshToLocalFile`）：
   - 类似结构但 stdout → 本地文件
   - 文件以 0o600 创建（对齐 Node `createWriteStream({ mode: 0o600 })`）

3. **`import_git_workspace_to_ssh`**（port of Node `importGitWorkspaceToSsh`）：
   - 创建 `tmp_bundle=$(mktemp ...)` 在远端 `<remoteDir>/.paperclip-runtime/`
   - 流式推送本地 git bundle 到远端
   - 远端脚本：`git init` → `fetch --force $tmp_bundle <tempRef>:<tempRef>` →
     `checkout -B <branch>` 或 `--detach` → `reset --hard` → `clean -fdx -e .paperclip-runtime`
   - 本地 cleanup：delete temp ref + rm bundle dir

4. **`export_git_workspace_from_ssh`**（port of Node `exportGitWorkspaceFromSsh`）：
   - 远端脚本：`update-ref refs/paperclip/ssh-sync/export HEAD` →
     `bundle create $tmp_bundle <ref>` → `cat $tmp_bundle`
   - 本地 fetch bundle → `reset --hard <importedRef>` (可选) → `rev-parse <importedRef>`
   - 返回 imported HEAD SHA

5. **Rust 2021 兼容**：shell 脚本中的 `$tmp_bundle` 是 Rust 2021 reserved
   prefixed identifier 语法。规避策略：用 `Vec<String>` + `String::from`
   + 字面 `"$"` + `"tmp_bundle"` 拼接，把 `$identifier` 散布到多个
   string literal 中而非出现在单个 format! 调用里。

### 真实验证（`tests/round498_git_workspace_sync_ssh.rs`，pc-acpx +2）

- `import_git_workspace_to_ssh_runs_remote_git_init_and_checkout`（10.18s）：
  - 本地 git repo（init + commit "hello from local"）
  - SSH fixture：真实 sshd + ssh-keygen + ed25519
  - 调用 `import_git_workspace_to_ssh` 后，远端目录出现 `.git` + `hello.txt`
  - 远端 `hello.txt` 内容含 `"hello from local"`
- `export_git_workspace_from_ssh_runs_remote_bundle_create_and_local_reset`：
  - 远端 git repo（init + commit "world from remote"）
  - 本地空 git repo
  - 调用 `export_git_workspace_from_ssh(..., reset_local_workspace=true)` 后
  - 返回 imported HEAD SHA = 远端 `rev-parse HEAD`
  - 本地 working tree 出现 `world.txt` 含 `"world from remote"`

### 测试快照

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 999 |
| pc-acpx integration | 1609（+2 R498） |
| pc-adapter-codex-local lib | 390 |
| pc-adapter-claude-local lib | 421 |
| **本仓合计** | **3419** |

### 后续计划（R499+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | R499：`createTransferProgress` 进度上报 | 真实百分比 / 字节速率 / 阶段标签，对齐 Node `runtime-progress.ts`；R498 的 import/export 接受 `Option<ProgressSink>` 参数 |
| P1 | R500：`integrateImportedGitHead` | Node L916-1000（concurrent ref retry + merge-tree），用于 `export` 后本地同步 |
| P2 | `restoreWorkspaceFromSshExecution` | Node L1559-1650（pick import/export/reset 策略），R501 |
| P2 | `syncDirectoryToSsh` | Node L1270-1530（tar 流 + 进度 + delta 排除），R502 |
| P2 | `SshLabFixture` 上移 | round492 / round494 / round495 / round496 / round498 五处各有一份；上移为共享 helper |
| P3 | 其他 adapter | cursor / gemini / grok / pi / hermes / openclaw 延后（用户约束） |

## 40. R502 完成：syncDirectoryToSsh 端口（tar 流 SSH 同步）

### 动机

Node `packages/adapter-utils/src/ssh.ts` L1270-1330 的 `syncDirectoryToSsh` 是
**非 git-backed workspace 的主同步路径**：用 `tar -cf -` 在本地打包，通过
`ssh` 管道到 `tar -xf -` 在远端解压。Rust 端之前完全缺失，导致非 git 仓库
（例如 git-history 初始化前的临时工作区、纯配置工作区）无法走 SSH 远程路径。

### 关键改动

`crates/pc-acpx/src/git_workspace_sync.rs` 新增：

1. **`sync_directory_to_ssh`**：
   - 本地 `tar -h? -C <local_dir> [--exclude ...] -cf - .` spawn，
     stdout 管道 + stderr 捕获
   - 远端 `ssh ... sh -c 'mkdir -p <remote> && tar -xf - -C <remote>'` spawn，
     stdin 管道 + stderr 捕获
   - `tokio::io::copy(tar.stdout → ssh.stdin)` 异步 pump
   - 两个 child 各自的 stderr 后台读取任务
   - 双 child `wait()` 都收 0 才算成功；任一非零返回其 stderr
   - `tar_exclude_args` 自动前置 `._*`（Mac resource fork 防护）
   - `tar_spawn_env_defaults` 自动设 `COPYFILE_DISABLE=1`
   - `follow_symlinks=true` 透传 `-h`

2. **未引入**（下一轮 R503）：
   - `RuntimeProgressReporter` + `createTransferProgress`（170 行）
   - `syncDirectoryFromSsh`（镜像反向）
   - `estimateLocalDirSize` / `probeRemoteDirSize`
   - `prepareWorkspaceForSshExecution` / `restoreWorkspaceFromSshExecution`

### 真实验证（`tests/round502_sync_directory_to_ssh.rs`，pc-acpx +2）

- `sync_directory_to_ssh_pipes_tar_through_ssh_to_remote_extract`（10.15s）：
  本地建 `file1.txt` + `file2.txt` + `subdir/nested.txt` + 远端预置 stale
  `file1.txt`，调用后远端三个文件内容与本地一致，stale 文件被覆盖。
- `sync_directory_to_ssh_respects_exclude`（同 suite）：
  本地 `keep.txt` + `node_modules/x.js`，`exclude=["node_modules"]`，调用后
  远端只有 `keep.txt`，`node_modules` 不存在。

### 测试快照

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 999 |
| pc-acpx integration | 1617（+2 R502） |
| pc-adapter-codex-local lib | 390 |
| pc-adapter-claude-local lib | 421 |
| **本仓合计** | **3427**（含 codex/claude 集成 = 4397） |

### 后续计划（R503+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | R503：`RuntimeProgressReporter` + `createTransferProgress` + progress 接入 `sync_directory_to_ssh` / `import` / `export` | 阶段标签 + 字节速率 + 节流百分比；175 行 |
| P1 | R504：`syncDirectoryFromSsh` 镜像反向 | ~130 行；staging temp dir + remote tar → 本地 extract + clear/copy |
| P2 | R505：`prepareWorkspaceForSshExecution` + `restoreWorkspaceFromSshExecution` | pick import/export/reset 策略；接入 codex/claude execute 路径 |
| P2 | R506：`estimateLocalDirSize` / `probeRemoteDirSize` | progress 估计所需 |
| P2 | `SshLabFixture` 上移 | round492 / round494 / round495 / round496 / round498 / round502 六处各有一份；上移为共享 helper |
| P3 | 其他 adapter | cursor / gemini / grok / pi / hermes / openclaw 延后（用户约束） |

## 41. R504 完成：syncDirectoryFromSsh 端口（tar 流 SSH 反向同步）

### 动机

Node `packages/adapter-utils/src/ssh.ts` L1336-1530 的 `syncDirectoryFromSsh`
是 `syncDirectoryToSsh` 的镜像反向：**远端 `tar -cf -` 经 SSH 管道 → 本地
`tar -xf -` 到 staging dir → 原子替换目标目录**。用于 remote execution 后
恢复本地 workspace（`restoreWorkspaceFromSshExecution` 内的导入路径）。

R502 已经实现了 ToSsh；本轮把 FromSsh 镜像补齐，让两侧双向同步完整。

### 关键改动

`crates/pc-acpx/src/git_workspace_sync.rs` 新增：

1. **`sync_directory_from_ssh(spec, remote_dir, local_dir, exclude, preserve_local_entries)`**：
   - 远端 `ssh ... sh -c 'cd <remote> && tar <exclude> -cf - .'` spawn，
     stdout 管道 + stderr 捕获
   - 本地 `tar -xf - -C <staging_dir>` spawn，stdin 管道 + stderr 捕获
   - `tokio::io::copy(ssh.stdout → tar.stdin)` 异步 pump
   - 双 child `wait()` 都收 0 才算成功；任一非零返回其 stderr
   - `tar_exclude_args` / `tar_spawn_env_defaults` 同 R502（`._*` 前置 +
     `COPYFILE_DISABLE=1`）
   - 成功路径：**先** `clear_local_directory(local_dir, preserve)` 删掉旧条目
     （保留 `preserve_local_entries` 中的文件名），**再** `copy_directory_contents(staging, local_dir)`
     递归复制 staging → 目标；最后 `remove_dir_all(staging)`
   - 失败 / 完成路径都清理 staging dir

2. **辅助 helpers**：
   - `clear_local_directory(local_dir, preserve: Option<&[String]>)` —
     `mkdir recursive` + `read_dir` + 过滤 preserve 集合 + `remove_dir_all`
     / `remove_file`；镜像 Node `clearLocalDirectory`
   - `copy_directory_contents(source, target)` — `mkdir recursive` +
     `read_dir` + 一层递归 `copy_dir_entry`
   - `copy_dir_entry(from, to)` — 递归复制 helper（`metadata` 判断
     dir/file/symlink，symlink 跳过对齐 Node tar 提取行为）

3. **未引入**（下一轮 R505 / R506）：
   - `RuntimeProgressReporter`（progress 暂未接入 to/from 路径）
   - `prepareWorkspaceForSshExecution` / `restoreWorkspaceFromSshExecution`
   - `estimateLocalDirSize` / `probeRemoteDirSize`

### 关键设计要点

- **sh 脚本分段拼接规避 Rust 2021 `$identifier` 保留语法**：远端命令用
  `format!("cd {} && tar {}", shell_quote(remote_dir), tar_cmd_parts.join(" "))`
  而不是 `"cd $remote_dir && tar $flags"`——任何字符串字面量含 `$identifier`
  都会触发 Rust 2021 lexer 的保留前缀错误。
- **`tar_cmd_parts` 用 `Vec<String>` 拼**：每个 exclude 单独 `shell_quote`
  后 push，避免在单个 `format!` 块里交叉 `$` 与 `{}` 占位符。
- **`SshAuthArgs::create(&spec.as_connection_config())`**：`auth.args()`
  返回 `Vec<String>`，`chain([...])` 追加 `-p <port>` / `<user>@<host>`
  / `sh -c <script>`；与 R502 一致。
- **本地 tar env_clear + defaults**：避免宿主 `COPYFILE` / `TAR_OPTIONS`
  污染；`envs(tar_spawn_env_defaults())` 自动设 `COPYFILE_DISABLE=1`，
  `env("PATH", ...)` 保留基本路径。

### 真实验证（`tests/round504_sync_directory_from_ssh.rs`，pc-acpx +3）

- `sync_directory_from_ssh_pipes_tar_through_ssh_to_local_extract`（10.18s）：
  远端建 `file1.txt` + `file2.txt` + `subdir/nested.txt` + 本地预置 stale
  `file1.txt`，调用后本地三个文件内容与远端一致，stale 文件被覆盖。
- `sync_directory_from_ssh_preserves_local_entries`（同 suite）：
  本地预置 `user.env`（preserved）+ `stale.txt`（clear）+ 远端 `keep.txt`，
  调用后 `user.env` 内容保持不变，`stale.txt` 被清除，`keep.txt` 存在且
  内容来自远端。
- `sync_directory_from_ssh_respects_exclude`（同 suite）：
  远端 `keep.txt` + `node_modules/x.js`，`exclude=["node_modules"]`，
  调用后本地只有 `keep.txt`，`node_modules` 不存在。

### 测试快照

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 999 |
| pc-acpx integration | **1620**（+3 R504） |
| pc-adapter-codex-local lib | 390 |
| pc-adapter-claude-local lib | 421 |
| **本仓合计** | **3430**（含 codex/claude 集成 = 4400） |

### 后续计划（R505+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | R503：`RuntimeProgressReporter` + `createTransferProgress` + progress 接入 `sync_directory_to_ssh` / `sync_directory_from_ssh` / `import` / `export` | 阶段标签 + 字节速率 + 节流百分比；175 行 |
| P1 | R505：`prepareWorkspaceForSshExecution` + `restoreWorkspaceFromSshExecution` | pick import/export/reset 策略；接入 codex/claude execute 路径 |
| P1 | R506：`estimateLocalDirSize` / `probeRemoteDirSize` | progress 估计所需；可与 R503 合并 |
| P2 | `SshLabFixture` 上移 | round492 / round494 / round495 / round496 / round498 / round502 / round504 七处各有一份；上移为共享 helper |
| P2 | Claude `execute_with_resume_retry` 接入生产路径（`Adapter::execute()` 当前未调用它） | 完整 bridge + resume + retry 流程 |
| P3 | 其他 adapter | cursor / gemini / grok / pi / hermes / openclaw 迁到 `execute_command_for_target` 延后（用户约束） |

## 42. R505 完成：SSH workspace prepare/restore 编排入口

### 动机

Node `packages/adapter-utils/src/ssh.ts` L1490-1660 提供三个高层入口：
- `prepareWorkspaceForSshExecution`（L1510-1558）
- `restoreWorkspaceFromSshExecution`（L1559-1647）
- `ensureSshWorkspaceReady`（L1649-1658，本轮未做，留给 R507）
- 配套私有 helpers：`clearRemoteDirectory`（L995-1011）+ `removeDeletedPathsOnSsh`（L1014-1027）

它们是 **Claude/Codex adapter 的 `Adapter::execute()` 调用 SSH 路径的总入口**：
- prepare：把本地 workspace（含 `.git` history + working tree）推到远端，
  让远端成为一个可执行命令的"现场"
- restore：把远端执行后的 workspace（含修改/新增/删除）拉回本地，恢复用户视角
- 内部按本地是否 git-backed 分两条路径

之前 R495-R504 已经实现了所有底层原语，但缺这层 orchestrator。Claude/Codex
走 SSH 远程路径的"最后一公里"——这一轮补齐。

### 关键改动

`crates/pc-acpx/src/git_workspace_sync.rs` 新增 4 个 pub async 函数：

1. **`clear_remote_directory(spec, remote_dir, preserve_entries)`** —
   ssh 跑 `mkdir -p <remote> && find <remote> -mindepth 1 -maxdepth 1
   [! -name <p1>] [! -name <p2>] -exec rm -rf -- {} +`；与 Node `clearRemoteDirectory` 一致

2. **`remove_deleted_paths_on_ssh(spec, remote_dir, deleted_paths)`** —
   `deleted_paths` 为空时短路返回；否则 ssh 跑 `cd <remote> && rm -rf -- <p1> <p2>...`；
   `shell_quote` 防注入

3. **`prepare_workspace_for_ssh_execution(spec, local_dir, remote_dir) -> bool`**
   - 读本地 `read_git_workspace_snapshot`
   - **git 路径**：`import_git_workspace_to_ssh`（R497）+ `sync_directory_to_ssh`
     （R502，exclude `.git` + `.paperclip-runtime`）+ `remove_deleted_paths_on_ssh`（snapshot.deleted_paths）
   - **非 git 路径**：`clear_remote_directory`（保留 `.paperclip-runtime`）+
     `sync_directory_to_ssh`（R502，exclude `.paperclip-runtime`）
   - 返回 `bool`：`true` = git-backed，`false` = 非 git

4. **`restore_workspace_from_ssh_execution(spec, local_dir, remote_dir)`** —
   - **git 路径**：`export_git_workspace_from_ssh`（R498）+ `sync_directory_from_ssh`
     （R504，exclude `.git` + `.paperclip-runtime`，preserve `.git`）
   - **非 git 路径**：`sync_directory_from_ssh`（R504，exclude `.paperclip-runtime`）
   - 暂不做 `baselineSnapshot` 分支（Node L1559-1647 早期分支），留给 R506+ 接入 baseline 快照

### 关键设计要点

- **全程基于 R497-R504 的 sync 原始**：不重新发明 SSH/tar/git 逻辑，每一步都
  复用已实现的 pub async 函数，确保单一职责与高内聚
- **`shell_quote` 防注入**：用户输入的 `remote_dir` / `deleted_paths` / `preserve_entries`
  都过 `git_workspace_sync.rs` 内的局部 `shell_quote`（POSIX 单引号转义）
- **超时/缓冲常量对齐 Node**：ssh 跑 `30_000 ms` timeout + `256 * 1024` max buffer
- **失败短路**：所有步骤任一失败立刻返回 `Err`，不清场（node 端由调用方决定重试策略）

### 真实验证（`tests/round505_prepare_restore_workspace.rs`，pc-acpx +2）

- `prepare_restore_roundtrip_git_backed_workspace`（10.20s）：本地 git repo
  + `README.md` + `src.txt`，`git init` + 提交；`prepare` 后远端同时出现
  `.git/` + `README.md` + `src.txt`；模拟远端编辑 `src.txt`；`restore` 后
  本地 `src.txt` 反映远端编辑 + `.git/` 仍保留
- `prepare_restore_roundtrip_non_git_workspace`（同 suite）：本地无 `.git`，
  `config.yaml = "key: val\n"`；远端预置 stale；`prepare` 返回 `false` +
  远端 config.yaml 内容覆盖；远端编辑后 `restore` 本地文件反映远端修改

### 测试快照

| Crate | 测试数 |
|---|---|
| pc-acpx lib | 999 |
| pc-acpx integration | **1622**（+2 R505） |
| pc-adapter-codex-local lib | 390 |
| pc-adapter-claude-local lib | 421 |
| **本仓合计** | **3432**（含 codex/claude 集成 = 4402） |

### 后续计划（R506+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | R506：`baselineSnapshot` 接入 + `integrateImportedGitHead` | 让 `restore` 支持 Node L1559-1647 的早期合并路径（dirty remote wins + git head integrate） |
| P1 | R503：`RuntimeProgressReporter` + `createTransferProgress` | progress 接入 R502/R504/R505 sync 路径；175 行 |
| P1 | `ensureSshWorkspaceReady` 端口 | Node L1649-1658（mkdir + pwd 校验 + 返回 cwd），10 行 |
| P1 | 接入 Claude/Codex `Adapter::execute()` 走 R505 编排 | 把 R495/R496 已有的 `execute_command_for_target` 串起来：prepare → exec → restore |
| P2 | `SshLabFixture` 上移为共享 helper | round492/494/495/496/498/502/504/505 八处重复 ~150 行 |
| P2 | Claude `execute_with_resume_retry` 接生产路径 | `Adapter::execute()` 当前未调用它 |
| P3 | 其他 adapter（cursor/gemini/grok/pi/hermes/openclaw）延后 | 用户约束 |

## 43. R503 完成：RuntimeProgressReporter + TransferProgress 端口

### 动机

Node `packages/adapter-utils/src/runtime-progress.ts`（170 行）+ ssh.ts L484-541
的 `createTransferProgress`（约 60 行）。这是 **所有 sync 操作的用户感知进度**：
- 大文件传输时显示百分比 / 字节速率 / 阶段标签
- 节流：默认每 10% 步进 + 至少 2s 间隔，避免 log flood
- 失败时显式 "failed at X%" 行而不是悬空百分比

R502 / R504 / R505 已有 `Option<...>` 占位接口但未真正接 sink；本轮实现
`RuntimeProgressReporter` + `ProgressReader` + `create_transfer_progress`，并
把 progress 接入 sync_directory_to_ssh / from_ssh / import / export / prepare / restore。

### 关键改动

新增 `crates/pc-acpx/src/runtime_progress.rs`（约 700 行，含测试）：

1. **核心类型**：
   - `RuntimeProgressPhase`：`Syncing` / `Restoring` / `ImportingGitHistory` / `ExportingGitHistory`
   - `RuntimeProgressDirection`：`To` / `From`
   - `RuntimeProgressTarget`：`Ssh` / `Sandbox`
   - `RuntimeProgressSink = Arc<dyn Fn(String) + Send + Sync>`（接完整行）
   - `RuntimeProgressReporterOptions`（含 `now_ms: Option<Arc<dyn Fn() -> u64>>` 测试时钟）

2. **`RuntimeProgressReporter`**：节流实现，对齐 Node
   - `report(done, total)`：仅在步进跨越 / 时间窗到时 emit
   - `complete(done?, total?)`：终态 100% 行，幂等
   - `fail(done?, total?)`：终态失败行，幂等且与 complete 互斥
   - 内部 `state: TerminalState` Open / Completed / Failed 状态机

3. **`ProgressReader`**：实现 `tokio::io::AsyncRead` 的字节计数包装
   - 构造时包一个 `Box<dyn AsyncRead + Send + Unpin>` + `Arc<tokio::sync::Mutex<RuntimeProgressReporter>>` + 可选 `cap`
   - `poll_read`：先调 inner 读，再同步 `try_lock` 累加字节（cap 限定 99% 防止估算误差显示过早 100%）；然后 `tokio::spawn` 一个 task 异步调用 `reporter.report()`
   - `set_total(total)`：中段更新 total（async size 估算 resolve 时调用）
   - `transferred()`：当前累计字节

4. **`TransferProgress`** + `create_transfer_progress(inner, options)`：
   - 返回 `{ counter: ProgressReader, finish: Arc<dyn Fn()->Pin<Box<Future>>>, fail: Arc<...> }`
   - 自动用 cap = total*99/100（如果 estimated=true）；终态 100% 由 `finish()` 发

5. **辅助**：
   - `format_progress_line(...)`：脱机格式化（同步），给测试 + 单点调用
   - `BYTES_PER_MB = 1024*1024` + `format_mb` + `clamp_percent`：与 Node 100% 一致

6. **接入 git_workspace_sync**：所有 sync 函数的末尾参数加 `progress: Option<&RuntimeProgressSink>`
   - `sync_directory_to_ssh`：phase=`Syncing`, direction=`To`, target=`Ssh`，估算模式
   - `sync_directory_from_ssh`：phase=`Restoring`, direction=`From`, target=`Ssh`，估算模式
   - `import_git_workspace_to_ssh`：phase=`ImportingGitHistory`, direction=`To`，精确模式
   - `export_git_workspace_from_ssh`：phase=`ExportingGitHistory`, direction=`From`，精确模式
   - `prepare/restore_workspace_for_ssh_execution`：传递 progress 给内部 sync 调用
   - `stream_local_file_to_ssh`：参数已就位（v1 内部 no-op 等待后续接 ProgressReader 包装 local file）

7. **测试适配**：
   - `tests/round397_progress_and_compaction.rs` 旧 API `now: Some(now)` → `now_ms: Some(now)`；sink 从 `Fn(&str)` → `Fn(String)`；`is_completed()` 改 assert 行内容；`#[test]` → `#[tokio::test]` + `.await`

### 关键设计要点

- **分离 transport 字节计数 vs orchestrator 阶段标签**：`create_runtime_progress_reporter` 只关心节流，标签来自 `options`；transport 只管调 `reporter.report(done, total)`。
- **`tokio::sync::Mutex` + `try_lock` + spawn**：poll_read 是 sync 不能 await，所以 sync 路径用 `try_lock` 更新 byte counter；async report 走 `tokio::spawn`。
- **`estimated=true` 模式 cap=99%**：防止 size 估算误差显示过早 100%，与 Node 一致。
- **零 panic + 全部幂等**：`complete()` / `fail()` 多次调用安全；`report()` 在 terminal 后直接 return。
- **测试时钟注入**：`now_ms: Option<Arc<dyn Fn() -> u64 + Send + Sync>>` 让 unit test 跑不依赖 `tokio::time::sleep` 真实等待，0.00s 完成。

### 真实验证

- **`crates/pc-acpx/src/runtime_progress.rs` lib 测试（7/7 通过）**：
  - `emits_initial_byte_only_line_when_total_unknown` — 未知 total 走 MB-only 模式
  - `throttles_when_percent_step_not_crossed` — 5% 步进不 emit（除非时间到）
  - `emits_on_each_10_percent_step` — 10/20/.../100 全部 emit
  - `terminal_at_100_percent_marks_complete` — 100% 后 report/complete 全部幂等
  - `fail_emits_failure_line_and_blocks_complete` — fail 后 complete no-op
  - `format_progress_line_bytes_only_when_no_total` + `..._with_total_and_label` — 格式化正确
- **`round397_progress_and_compaction` 集成测试**：`progress_reporter_full_sync_lifecycle` + `progress_reporter_fail_marks_completed` 两个 case 通过新 API 适配后通过

### 跨 round 回归（9/9 sshd fixture 全过，零破坏）

| 套件 | 结果 |
|---|---|
| round498_git_workspace_sync_ssh | 2 passed |
| round502_sync_directory_to_ssh | 2 passed |
| round504_sync_directory_from_ssh | 3 passed |
| round505_prepare_restore_workspace | 2 passed |

### 测试快照

| Crate | 测试数 |
|---|---|
| pc-acpx lib | **1006**（999 + 7 R503） |
| pc-acpx integration | 1622（保持） |
| pc-adapter-codex-local lib | 390 |
| pc-adapter-claude-local lib | 421 |
| **本仓合计** | **3439**（含 codex/claude 集成 = 4409） |

### 后续计划（R504+）

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | R504：把 progress 真正接入 `stream_local_file_to_ssh`（v1 no-op） | 包装 local file 为 ProgressReader；接 bundle size 作为精确 total |
| P1 | R505b：`baselineSnapshot` 接入 + `integrateImportedGitHead` | restore 早期合并路径 |
| P1 | R506：`ensureSshWorkspaceReady` 端口（10 行）+ 接入 Claude/Codex `Adapter::execute()` 走 R505 编排 | 最后一公里 |
| P2 | `SshLabFixture` 上移 | round492/494/495/496/498/502/504/505 八处重复 ~150 行/处 |
| P2 | Claude `execute_with_resume_retry` 接生产路径 | 完整 bridge + resume + retry |
| P3 | 其他 adapter（cursor/gemini/grok/pi/hermes/openclaw）延后 | 用户约束 |
