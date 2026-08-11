# R534 — paperclip-rs vs paperclip 全面差距审计（2026-08-11）

> 评估基线：commit `aab4a7b` + R514-R533 未提交工作 + 91 commits ahead of origin/main
> 评估口径：crates/ 77 + apps/ 2 + ui 复用 = 80 工作单元
> 测试基线：6619+ passing（`cargo test --workspace --lib`）

## 一、整体完成度快照（R534 末真实盘点）

| 维度 | paperclip (Node) | paperclip-rs (Rust) | 完成度 |
|---|---|---|---|
| **源文件数** | 1849 server + 1053 packages = **2902 TS** | **1265+ .rs** (crates) + 2 apps | 文件数 ~44% |
| **代码行数** | server 619,638 + packages 264,941 ≈ **884K LOC** | **477,616 LOC** (含 tests/fixtures) | 体积 ~54% |
| **HTTP 路由文件** | 56 个 | 56 个（pc-http 70+ 文件覆盖） | **100%** |
| **仓储子模块** | 212 services | 80+ pc-repos 子模块 | **~90%** |
| **数据库表** | 109 表 | 172 表（含衍生） | schema 兼容 |
| **内置适配器** | 11 个 | 11 个 crate（11/11 描述符就位） | **100% 就位** |
| **adapter execute.ts 覆盖率** | 100% | claude-local ~85% / codex-local ~95% / 其他 30-50% | **加权 ~70%** |
| **UI** | 1168 React 文件 | 复用 `paperclip/ui/` | **100% 复用** |
| **CLI 子命令** | 19+ | 19 子命令（apps/pc-cli/src/main.rs 2212 行） | **100% 入口** / **~70% 真做** |
| **Auth/AuthZ** | better-auth 完整 | refresh/CSRF/API key ✅ / OAuth 0% (上游也没) | **~85%** |
| **Heartbeat** | 状态机 + recovery | 1431 LOC lib + 12 决策模块 + stale sweep 已知 4 失败 | **~85%** |
| **Realtime WS** | live-events | pc-realtime 1,334 LOC | **~95%** |
| **OpenAPI** | routes/openapi.ts 自动反射 | pc-openapi 480 LOC + dto_schemas + 41 DTO + 69 path hints | **~100% 路由覆盖** |
| **Plugin SDK** | 完整 npm SDK | pc-plugin-host 4,986 LOC + 4 plugin crates | **~65%** |
| **Workflow + Cron** | routines + pipelines | pc-workflow + pc-cron | **~75%** |
| **Storage** | local-disk + s3 | pc-storage 1,212 LOC | **~95%** |
| **Secrets** | local + aws-sm | pc-secrets 2,535 LOC | **~95%** |
| **Backup** | pg_dump/pg_restore | pc-backup 1,445 LOC | **~95%** |
| **Migrate** | 109 表 SQL | pc-migrate up/down/status/create/verify/baseline/seed | **~100%** |
| **真实启动验证** | N/A | scripts/e2e-baseline.sh + dev-ui-rust.sh + e2e-full-stack.sh | **3 套脚本就位** |

### 综合完成度

| 口径 | 百分比 |
|---|---|
| **质量层**（架构清晰 + 测试覆盖 + 关键路径真实） | **~93%** |
| **功能层**（V1-V15 硬目标加权） | **~78-82%** |
| **行数等效比**（不含测试/fixture） | **~50%** |
| **可投产比例**（真实长跑 + 60 client UI + 端到端） | **~70%** |

## 二、模块级差距（按业务域）

### 🔴 P0 — 硬阻塞（用户目标）

| # | 模块 | 缺口 | Node 位置 | 估计工作量 |
|---|---|---|---|---|
| **G1** | UI 60 client 全 happy path | `scripts/ui-happy-path.sh` 验证 5/60 | ui/src/api/ 60 client | 1-2 轮 |
| **G2** | Playwright 真实 UI 剧本 | 登录→公司→issue→heartbeat→live-event | tests/e2e/ 仅 5 API spec | 1-2 轮 |
| **G3** | 真实长跑 + 性能基线 | 5 分钟 heartbeat + WS + wrk 对比 Node | scripts/long-run-5min.sh | 1 轮 |
| **G4** | pc-heartbeat stale lock sweep 回归 | round300 4 失败待修 | pc-heartbeat/recovery/ | 0.5 轮 |

### 🟡 P1 — 深度补全

| # | 模块 | 缺口 | Node 位置 | 估计工作量 |
|---|---|---|---|---|
| **G5** | claude-local execute.ts 剩余 15% | remote bridge + restoreRemoteWorkspace + materializeRemoteClaudeConfig | server/src/adapters/claude-local/execute.ts L570-690 | 2 轮 |
| **G6** | codex-local execute.ts 剩余 2% | stagedCodexHomeDir teardown + remoteCodexConfigDir | server/src/adapters/codex-local/execute.ts | 0.5 轮 |
| **G7** | claude-local localProcessSandbox 选项 | bwrap + network + fs scope | execute.ts L530-555 | 1 轮 |
| **G8** | quota.ts 完整复刻 | pc-adapter-quota 39 测试仅占 Node 50% | packages/adapters/claude-local/quota.ts 541 LOC | 1-2 轮 |
| **G9** | pc-plugin-host 互操作 | 与原 SDK worker JSON-RPC 真实握手 | packages/plugins/sdk/ | 1-2 轮 |
| **G10** | pc-workflow + pc-cron 端到端 | DAG 真实触发 + cron 真实 tick | server/src/services/routines/ | 1-2 轮 |
| **G11** | 路由字节级 14% 缺口 | companies 子路由 DELETE + /api/admin/* | routes/companies/ + routes/admin/ | 1 轮 |
| **G12** | pc-openapi ↔ UI 类型契约 | UI 60 client 用生成的 types 替换手写 | ui/src/api/types.ts | 1-2 轮 |

### 🟢 P2 — 长尾深化

| # | 模块 | 缺口 | 估计工作量 |
|---|---|---|---|
| **G13** | 其他 adapter 远程路径 | hermes / cursor-cloud / openclaw | 用户明确延后 |
| **G14** | 真实迁移 109→172 patch | 衍生表说明 + schema diff warning | 0.5 轮 |
| **G15** | 中文文档（OPERATIONS + PLUGIN_AUTHORING + MIGRATION） | 仅有 ARCHITECTURE/MODULE-MAPPING/PROJECT-PLAN | 1 轮 |
| **G16** | UI 依赖修复（pre-existing） | `@assistant-ui/tap` 缺 `./react` 子路径 | 独立 change |
| **G17** | pc-repos / pc-heartbeat 深化 | 边缘 case | 持续 |

## 三、按 crate 的剩余行数估算

| 类别 | 剩余缺口 | 说明 |
|---|---|---|
| pc-adapter-claude-local | ~600 行 | remote bridge + sandbox options |
| pc-adapter-codex-local | ~200 行 | staged teardown + remote auth json |
| pc-adapter-quota | ~1500 行 | 完整 quota.ts 复刻 |
| pc-adapter-hermes / cursor / openclaw / gemini / grok / pi / opencode | 各 ~2000-3000 行 | 用户明确延后 |
| pc-plugin-host | ~1200 行 | worker→host 回调 + 互操作 |
| pc-workflow + pc-cron | ~500 行 | 端到端 trigger chain |
| pc-http (companies 子路由 + admin) | ~400 行 | 14 个端点 |
| ui/src/api/types.ts + 60 client | ~1500 行 | 替换手写为生成 |
| tests/e2e/ Playwright spec | ~800 行 | 真实 UI 剧本 |
| 中文文档 | ~2500 行 | OPERATIONS + PLUGIN_AUTHORING + MIGRATION |

**合计 P0+P1 缺口**：~8000-10000 行 Rust（不含 P2 延后 adapter）

## 四、R534 真实"剩余硬骨头"优先级

按"用户目标硬阻塞 + 价值/复杂度比"排序：

1. **G4 stale lock sweep 修复**（0.5 轮，4 个失败 case）
2. **G5/G6 claude/codex 远程路径收尾**（2.5 轮）
3. **G1 UI 60 client happy path 验证**（1 轮）→ 解锁 G2
4. **G2 Playwright 真实 UI 剧本**（1.5 轮）→ 解锁"真实启动前后端验证"用户硬目标
5. **G3 真实长跑 + 性能基线**（1 轮）→ 性能声明依据
6. **G11 路由字节级收尾**（1 轮）
7. **G12 OpenAPI ↔ UI 类型契约**（1.5 轮）→ 解锁前后端类型自动同步
8. **G8 quota.ts 完整复刻**（1.5 轮）
9. **G9 plugin 互操作**（1.5 轮）
10. **G15 中文文档**（1 轮）→ 移交

**约 12 轮（6 周）后可达 ~92-95% 综合完成度 + 用户硬目标全清**

## 五、建议的下个 change 命名

`paperclip-rs-r534-r545-hardening` — 围绕 G1-G4 用户硬阻塞 + G5/G6 adapter 收尾 + G12 类型契约展开。
