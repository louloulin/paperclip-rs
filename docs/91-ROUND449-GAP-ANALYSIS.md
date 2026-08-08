# R449 差距分析与下一轮复刻计划

## 1. 当前实现概览（提交点 4272e92 + R434–R448 本地未提交）

### 1.1 工作区状态
- 工作区：`/Users/louloulin/Documents/lumosaipaperclip/paperclip-rs/`
- 分支：`main`，领先 `origin/main` 26 个 commit
- 6-crate 测试合计：**~1804 全通过，0 失败**
  - pc-acpx：883
  - pc-adapter-codex-local：95+
  - pc-adapter-claude-local：89+
  - pc-adapter-process：6+
  - pc-activity：14+
  - pc-adapter-quota：39+

### 1.2 paperclip-rs 总规模
- pc-acpx：~47 个模块，总行数 ~32k 行（核心引擎）
- pc-adapter-codex-local：9 文件 / ~3.5k 行
- pc-adapter-claude-local：10 文件 / ~2.8k 行
- pc-http：50+ routes，总行数 ~43k 行
- pc-repos / pc-heartbeat / pc-core：~85% 完成度

### 1.3 Node paperclip 参考规模（src/server 子目录）
- codex-local：~12.6k 行（含测试 17 个 .ts + 1 cjs + 1 sh）
- claude-local：~6.5k 行（含测试 13 个 .ts）

---

## 2. claude-local / codex-local 模块复刻覆盖率

### 2.1 codex-local（17 .ts + 1 .cjs + 1 .sh）

| Node 文件 | 行数 | Rust 复刻 | 覆盖率 |
|---|---|---|---|
| acp.ts | 626 | ❌ 未复刻 | **0%** |
| auth-precedence.ts | 46 | ✅ auth_precedence.rs | 100% |
| codex-args.ts | 88 | ✅ lib.rs::build_codex_exec_args | 100% |
| codex-auth-copyback.ts | 154 | ✅ auth_copyback.rs | 100% |
| codex-auth-merge-scripts.ts + .cjs + .sh | 49+87+73 | ⚠️ 部分（决策谓词未 Rust 化） | 30% |
| codex-home.ts | 795 | ✅ codex_home.rs（核心子集） | 70% |
| config-schema.ts | 73 | ❌ 未复刻 | **0%** |
| execute.ts | 1504 | ✅ lib.rs::execute（核心路径） | 75% |
| index.ts | 94 | ⚠️ 部分（AdapterExecutionContext 接线 + sessionCodec） | 60% |
| output-inactivity-monitor.ts | 155 | ✅ output_inactivity_monitor.rs | 100% |
| parse.ts | 328 | ✅ lib.rs::parse_codex_jsonl + codex_errors.rs | 90% |
| process-activity-monitor.ts | 129 | ✅ pc_activity/process_activity_monitor.rs | 100% |
| quota.ts | 651 | ⚠️ 部分（pc-adapter-quota 已覆盖） | 50% |
| runtime-config.ts | 430 | ✅ runtime_config.rs | 95% |
| skills.ts | 44 | ✅ skills.rs | 100% |
| test.ts | 446 | ⚠️ test_environment 已部分覆盖 | 70% |

**codex-local 整体覆盖率：~78%**

### 2.2 claude-local（13 个 .ts）

| Node 文件 | 行数 | Rust 复刻 | 覆盖率 |
|---|---|---|---|
| acp.ts | 554 | ❌ 未复刻 | **0%** |
| claude-config.ts | 244 | ✅ claude_config.rs | 95% |
| cli-capabilities.ts | 94 | ✅ cli_capabilities.rs | 100% |
| config-schema.ts | 73 | ❌ 未复刻 | **0%** |
| execute.ts | 1270 | ✅ lib.rs::execute（核心路径） | 70% |
| index.ts | 95 | ⚠️ 部分 | 60% |
| models.ts | 164 | ✅ claude_models.rs | 100% |
| parse.ts | 507 | ✅ claude_stream_json.rs + lib.rs::parse | 80% |
| permissions.ts | 43 | ✅ claude_permissions.rs | 100% |
| prompt-cache.ts | 174 | ✅ claude_prompt_cache.rs | 95% |
| quota.ts | 541 | ⚠️ 部分 | 50% |
| skills.ts | 64 | ✅ skills.rs | 100% |
| test.ts | 463 | ⚠️ 部分 | 60% |

**claude-local 整体覆盖率：~75%**

---

## 3. 关键差距（按优先级排序）

### 3.1 R449 acp.ts（核心 — 引擎选择 + ACP 协议）
- **重要性**：🔴 最高 — `execute.ts` 第一步调用，决定走 ACP 还是 CLI
- **codex-local 行数**：626
- **claude-local 行数**：554
- **可独立复刻的子模块**：
  1. `normalizeEngine` / `resolveXxxExecutionEngine` — 纯函数
  2. `resolveXxxExecutionEngineForRun` — async，运行时决策
  3. `formatXxxAcpFallbackMessage` — 字符串
  4. `firstNonEmptyString` — 工具
  5. `buildXxxAcpConfig` — 配置归一化
  6. `resolveXxxAcpBillingIdentity` — 身份解析
  7. `withXxxAcpDefaults` — AcpxEngineExecutorOptions 包装
  8. `withXxxAuthRefreshFailureClassification` — 结果再分类
  9. `parseVersion` / `nodeVersionMeetsXxxAcpMinimum` — 版本检查
  10. `pathExists` / `findCommandOnPath` / `findAncestorBin` — 命令解析
  11. `commandIsResolvable` — 命令可解析性检查
  12. `resolveXxxAcpCommand` / `resolveXxxAcpCommandForTarget` — 命令选择
  13. `defaultXxxAcpFallbackReason` — fallback 原因聚合
  14. `summarizeStatus` — 环境检查结果汇总
  15. `testXxxAcpEnvironment` — 环境探测入口（依赖上面所有）
  16. `prepareXxxRemoteManagedHome` — 远程 managed home
  17. `createXxxAcpExecutor` — ACP executor factory

### 3.2 R450 codex-auth-merge-scripts（外部脚本 → 纯 Rust 谓词）
- **重要性**：🟡 中 — 已通过 `auth_copyback.rs` 覆盖多数，但决策谓词仍依赖 `node codex-auth-merge-decision.cjs` 子进程
- **可独立复刻**：决策逻辑（比较 `auth.json` 的 `tokens.account_id` + `last_refresh` 时间戳），约 200 行

### 3.3 R451 config-schema（两个 adapter）
- **重要性**：🟡 中 — 控制台表单 + UI 校验
- **实现复杂度**：低 — 数据结构 + JSON 序列化

### 3.4 R452 pc-http executionTarget 注入
- **重要性**：🟠 高 — 打通远程执行路径
- **改动点**：
  - `crates/pc-adapter-api/src/lib.rs`（`AdapterExecutionContext` 添加字段）
  - `crates/pc-http/src/routes/agents.rs`（从 DB 读取 target，注入 context）
  - `crates/pc-adapter-codex-local/src/lib.rs`（consume target）

---

## 4. R449 实施计划（立即执行）

### 目标：复刻 acp.ts 的所有可独立测试子模块

### 4.1 codex-local/src/acp.rs（约 350 行 + 8 测试）
模块结构：
```rust
//! Codex ACP engine 选择与执行器创建
//!
//! 对齐 Node `packages/adapters/codex-local/src/server/acp.ts`：
//!   * `normalizeCodexEngine` / `resolveCodexExecutionEngine`
//!   * `resolveCodexExecutionEngineForRun`（含 in_place / filesystemScope / networkScope 检查）
//!   * `formatCodexAcpFallbackMessage`
//!   * `firstNonEmptyString`
//!   * `buildCodexAcpConfig`
//!   * `resolveCodexAcpBillingIdentity`
//!   * `withCodexAcpDefaults`（包装 `pc_acpx::acpx_engine_executor::AcpxEngineExecutorOptions`）
//!   * `withCodexAuthRefreshFailureClassification`
//!   * `parseVersion` / `nodeVersionMeetsCodexAcpMinimum`（改为 RuntimeVersion）
//!   * `commandIsResolvable` / `findCommandOnPath` / `findAncestorBin`（async fs）
//!   * `resolveCodexAcpCommand` / `resolveCodexAcpCommandForTarget`
//!   * `defaultCodexAcpFallbackReason`（async，聚合）
```

### 4.2 claude-local/src/acp.rs（约 300 行 + 8 测试）
平行模块，结构相同。

### 4.3 codex-local/src/config_schema.rs（约 70 行 + 2 测试）
```rust
//! 对齐 Node `config-schema.ts` — 控制台表单字段定义。
```

### 4.4 claude-local/src/config_schema.rs（约 70 行 + 2 测试）

### 4.5 验证
```bash
rtk cargo test -p pc-adapter-codex-local --lib
rtk cargo test -p pc-adapter-claude-local --lib
rtk cargo test -p pc-acpx --lib
```

期望新增测试：~20 个
期望 6-crate 总测试数：~1824

---

## 5. 长远路线图

| Round | 目标 | 预计新增测试 | 累计覆盖 |
|---|---|---|---|
| R449 | acp.ts (codex + claude) + config-schema.ts | +20 | 82% |
| R450 | codex-auth-merge-scripts 决策谓词 Rust 化 | +8 | 83% |
| R451 | pc-http executionTarget 注入 | +5 | 85% |
| R452 | 远程 managed home 准备 + auth-copyback 完整 Rust 化 | +10 | 86% |
| R453 | quota.ts 完整复刻（pc-adapter-quota 深化） | +15 | 87% |
| R454 | test.ts (test_environment) 完整复刻 | +10 | 88% |
| R455 | 其他 adapter (hermes/cursor-cloud/openclaw-gateway) 启动 | +20 | 90% |

---

## 6. 用户约束（贯穿始终）

- 中文说明 + 中文注释
- Rust Edition 2021（**不用 let chains**）
- **优先聚焦 `claude-local` + `codex-local`**
- 其他 adapter（hermes / cursor-cloud / openclaw-gateway / opencode / grok / gemini）延后
- 高内聚低耦合：每个模块独立，最少跨 crate 耦合
- Node 100% parity：纯函数全移植；execute 接线映射到 `AdapterExecutionResult`
- 不重命名、不修无关 bug、不主动 git commit
- shell 命令用 `rtk` 前缀
- `apply_patch` 需直接调二进制

