# paperclip-rs 文档路线图

> 目的：以"顶级开源项目"为参照，全面盘点 paperclip-rs 的文档现状与缺口，给出
> 按优先级排序的可执行清单。每条都标注**当前证据**（在哪里 / 是否存在）、
> **目标状态**（要写成什么样）与**来源**（参考何处）。

- 评估基线日期：2026-08-05
- 对照对象：[paperclip](https://github.com/paperclipai/paperclip) 上游、`paperclip/doc/`
  （44 个文件）、`docs.rs` 顶级 crate、`OpenSSF Scorecard`、`crates.io` 顶级项目
  （tokio / axum / sqlx / ripgrep / serde / clap / tracing 等）。
- 工作区根：`/Users/louloulin/Documents/lumosaipaperclip/paperclip-rs`

---

## 0. 当前文档盘点（证据）

### 0.1 顶层文档（workspace 根）

| 文件 | 存在 | 说明 |
| --- | --- | --- |
| `README.md` | ✅ | 已新建（10 KB），含对比表、协议一致性、构建、运行 |
| `LICENSE` | ❌ | 缺 |
| `NOTICE` | ❌ | 缺（上游商标归属） |
| `CONTRIBUTING.md` | ❌ | 缺 |
| `CODE_OF_CONDUCT.md` | ❌ | 缺 |
| `SECURITY.md` | ❌ | 缺 |
| `SUPPORT.md` | ❌ | 缺 |
| `ROADMAP.md` | ❌ | 缺 |
| `CHANGELOG.md` | ❌ | 缺 |
| `AGENTS.md` | ❌ | 缺（**paperclip 上游有**） |
| `DESIGN.md` | ❌ | 缺（**paperclip 上游有**，项目愿景/设计理念） |

### 0.2 `docs/` 现有文档（8 个）

| 文件 | 性质 | 应保留还是迁移 |
| --- | --- | --- |
| `docs/README.md` | 索引 | 保留，重写 |
| `docs/01-VITE-ERROR-ROOT-CAUSE.md` | 一次性根因分析 | 迁 `docs/internal/historical/` |
| `docs/02-PAPERCLIP-ARCHITECTURE.md` | Node 端基线 | 重命名为 `architecture/node-baseline.md` |
| `docs/03-KAMEO-ACTOR-ANALYSIS.md` | 内部技术分析 | 提炼成 `architecture/actors.md` |
| `docs/04-EXECUTION-PLAN.md` | 阶段执行计划 | 迁 `docs/internal/planning/` |
| `docs/05-PROGRESS-AUDIT.md` | 进度审计（445 KB） | 迁 `docs/internal/audit/` |
| `docs/06-LEXICAL-REAL-ROOT-CAUSE.md` | 一次性根因 | 迁 `docs/internal/historical/` |
| `docs/06-NODE-RUST-GAP-MATRIX.md` | gap 矩阵（46 KB） | 迁 `docs/internal/migration/` |
| `docs/07-COMPREHENSIVE-GAP-ANALYSIS.md` | 综合 gap（164 KB） | 迁 `docs/internal/migration/` |
| `docs/08-RUST-MODULAR-ARCHITECTURE.md` | 模块化架构 | 重命名为 `architecture/rust-modular.md` |

### 0.3 其他已有文档（按目录）

- `ARCHITECTURE-DIAGRAMS.md`（根目录，10 张图） — ✅ 保留为 `architecture/diagrams.md`
- `MODULE-MAPPING.md`（根目录） — ✅ 重命名为 `architecture/module-mapping.md`
- `PROJECT-PLAN.md`（根目录） — ✅ 迁 `internal/planning/project-plan.md`
- `openspec/changes/paperclip-rs-rewrite/{proposal,design,tasks,specs/}.md` — ✅ 迁
  `internal/openspec/`，但 tasks.md 的关键决策要进 ADR
- `packages/adapter-utils/{README,CHANGELOG}.md` — ✅ 保留（TS 包）
- `packages/shared/CHANGELOG.md` — ✅ 保留
- `packages/teams-catalog/MIGRATION.md` — ✅ 保留
- `ui/README.md` — ✅ 保留

### 0.4 完全缺失的结构

- `.github/`（仓库级无，UI 内的 `node_modules/` 不计） — ❌ CI、Issue 模板、PR 模板、Dependabot
- `examples/` — ❌
- `docker/`（纸笔） — ❌（上游有 `paperclip/docker/`）
- `scripts/` — ❌（上游有 `paperclip/scripts/`）
- `tests/`（集成测试目录） — ❌（上游有 `paperclip/tests/`）
- `book/src/`（mdBook） — ❌
- `crates/*/README.md`（crate 级 README） — ❌（38 个 crate 全部缺）
- `crates/*/examples/` — ❌

---

## 1. 顶级项目文档标准（参照系）

顶级 Rust + Node 全栈项目（如 axum、tokio、serde、pnpm、Deno）的标准配置：

| 类别 | 标准文件 | paperclip-rs |
| --- | --- | --- |
| 合规 | LICENSE / NOTICE / CONTRIBUTING / CoC / SECURITY / SUPPORT | 0/6 |
| 变更 | CHANGELOG / RELEASE-NOTES / ROADMAP | 0/3 |
| 协作 | AGENTS / DESIGN / .github/ISSUE_TEMPLATE / .github/PULL_REQUEST_TEMPLATE | 0/4 |
| 用户 | README / INSTALL / CONFIGURATION / CLI / API / DEPLOY / UPGRADE / TROUBLESHOOTING / FAQ | 1/9 |
| 开发 | ARCHITECTURE / MODULES / CONTRIBUTING-RUST / TESTING / STYLE / PROTOCOL / DATABASE | 0/7 |
| 扩展 | PLUGIN-AUTHOR / ADAPTER-AUTHOR / 自定义 CRATE README | 0/39+ |
| 运维 | PERFORMANCE / OBSERVABILITY / SECURITY-MODEL / BACKUP-RESTORE / CAPACITY | 0/5 |
| 决策 | ADR/0001-…/、RFC/0001-…/ | 0 |
| 产物 | docs.rs / mdBook / OpenAPI 托管页 / examples/ / 模板仓库 | 0 |
| CI | GitHub Actions（fmt/clippy/test/audit/doc/migrate-smoke/coverage）/ Dependabot / CodeQL | 0 |

---

## 2. 优先级矩阵（推荐执行顺序）

每项标 [P0/P1/P2/P3] = [必做/应做/可做/可后置]，[Effort] = 估算工作量。

### 第 1 批：合规与法律 [P0，~0.5–1 天]

| # | 文档 | Effort | 来源 | 内容要点 |
| --- | --- | --- | --- | --- |
| 1 | `LICENSE` | 5 min | `paperclip/LICENSE`（MIT） | MIT 全文 |
| 2 | `NOTICE` | 10 min | 上游 paperclip | 商标归属、致谢 |
| 3 | `CONTRIBUTING.md` | 30 min | 上游有 | 流程、commit、PR、契约影响、CLA/DCO 留口 |
| 4 | `CODE_OF_CONDUCT.md` | 5 min | Contributor Covenant v2.1 | 模板 |
| 5 | `SECURITY.md` | 30 min | 上游有，扩展 | 披露流程、支持窗口、Rust 适配（`cargo audit`） |
| 6 | `SUPPORT.md` | 10 min | 新增 | Discord / GitHub Discussions / 邮件 |
| 7 | `AGENTS.md` | 60 min | 上游有 | 给 AI agent 的工程约束（fmt/clippy/test/build/test-list） |
| 8 | `CHANGELOG.md` | 20 min | Keep a Changelog 1.1 | 0.1.0 首条 |
| 9 | `ROADMAP.md` | 30 min | 上游有 | 与 `PROJECT-PLAN.md` 协同 |
| 10 | `.github/ISSUE_TEMPLATE/{bug,feature,question}.md` | 30 min | 模板 | 含 reproduction、契约影响表 |
| 11 | `.github/PULL_REQUEST_TEMPLATE.md` | 30 min | 模板 | 含契约影响、测试矩阵、CHANGELOG 是否需要更新 |

### 第 2 批：用户文档核心 5 件 [P0，~3 天]

| # | 文档 | Effort | 内容要点 |
| --- | --- | --- | --- |
| 12 | `docs/INSTALL.md` | 4 h | 系统依赖、Rust ≥ 1.80、Postgres ≥ 14、UI 依赖、平台矩阵、Troubleshooting |
| 13 | `docs/CONFIGURATION.md` | 6 h | 全部 `PAPERCLIP_*` 环境变量表、`paperclip.json` schema、`.env` 示例 |
| 14 | `docs/CLI.md` | 6 h | `paperclipai` 全子命令（来自 `crates/pc-cli/src/main.rs`）、全局 flag、退出码 |
| 15 | `docs/MIGRATION-FROM-NODE.md` | 4 h | 从 `paperclip/` 切换步骤：DB 零迁移、UI 切 base、CLI 兼容表、已知差异 |
| 16 | `docs/TROUBLESHOOTING.md` | 4 h | 启动失败、迁移失败、心跳卡住、WS 断连、连接池耗尽、actor CapacityExceeded |

### 第 3 批：协议与生态文档 [P0，~2 天]

| # | 文档 | Effort | 内容要点 |
| --- | --- | --- | --- |
| 17 | `docs/PROTOCOL.md` | 8 h | HTTP 错误码表、x-paperclip-* 头部、live-events WS 消息 schema、JSON-RPC 插件 envelope |
| 18 | `docs/PLUGIN-AUTHOR.md` | 6 h | manifest、capability 声明、worker 进程模型、示例 JSON-RPC handler |
| 19 | `docs/ADAPTER-AUTHOR.md` | 6 h | `pc-adapter-api` trait、实现骨架、注册流程、测试 |

### 第 4 批：开发者文档 [P1，~5 天]

| # | 文档 | Effort | 内容要点 |
| --- | --- | --- | --- |
| 20 | `docs/ARCHITECTURE.md` | 6 h | 顶层架构导读（消费 `ARCHITECTURE-DIAGRAMS.md`） |
| 21 | `docs/MODULES.md` | 6 h | 38 个 crate 职责矩阵（消费 `MODULE-MAPPING.md`） |
| 22 | `docs/CONTRIBUTING-RUST.md` | 4 h | clippy pedantic、错误处理、async/kameo 模式、thiserror 用法 |
| 23 | `docs/TESTING.md` | 4 h | 单元/集成/E2E、proptest、mockall、覆盖率门槛 |
| 24 | `docs/STYLE.md` | 3 h | 命名、错误传播、日志、tracing span、配置注入 |
| 25 | `docs/DATABASE.md` | 6 h | 109 张表概览、迁移约定、soft-delete、tenant isolation、查询优化 |
| 26 | `docs/observability.md` | 4 h | tracing/OTLP、log redaction、metrics、health |
| 27 | `docs/PERFORMANCE.md` | 4 h | criterion 基准、pprof、连接池调优、actor 并发模型 |

### 第 5 批：运维与发布 [P1，~3 天]

| # | 文档 | Effort | 内容要点 |
| --- | --- | --- | --- |
| 28 | `docs/DEPLOYMENT.md` | 6 h | 单二进制、systemd、Docker、Kubernetes、反向代理、TLS、嵌入式 Postgres 选项 |
| 29 | `docs/BACKUP-RESTORE.md` | 4 h | `pc-backup` 使用、灾难恢复、S3 存储、密钥解密 |
| 30 | `docs/SECURITY-MODEL.md` | 6 h | session/JWT、密钥管理、依赖审计、SBOM、签名 |
| 31 | `docs/RELEASE.md` | 4 h | SemVer、tag、`cargo publish`、容器镜像、签名、公告 |

### 第 6 批：API 参考与产物 [P1/P2，~3 天]

| # | 项目 | Effort | 内容要点 |
| --- | --- | --- | --- |
| 32 | rustdoc 全 workspace 公开 API | 2 d | CI 任务 `cargo doc --workspace --no-deps` + 部署 |
| 33 | `crates/*/README.md`（38 份） | 1 d | 每 crate 一页：职责、依赖、关键 API、示例 |
| 34 | `docs/API.md` | 6 h | HTTP/WS 速查、auth、错误码、pagination、附件上传 |
| 35 | `examples/` 目录（4–6 个） | 1 d | hello-server、custom-adapter、plugin-worker、auth-client |
| 36 | mdBook 站点 `book/src/` | 1 d | 把 docs/ 用 SUMMARY.md 编成可托管站点 |

### 第 7 批：决策与社区 [P2，~2 天]

| # | 项目 | Effort | 内容要点 |
| --- | --- | --- | --- |
| 37 | `docs/adr/0001-actor-model-kameo.md` | 2 h | kameo vs tokio vs actix 的选型理由 |
| 38 | `docs/adr/0002-sqlx-vs-sea-orm.md` | 2 h | 编译期 SQL 校验 vs ORM |
| 39 | `docs/adr/0003-plugin-protocol-jsonrpc.md` | 2 h | JSON-RPC over stdio 而非 TCP |
| 40 | `docs/adr/0004-monorepo-cargo-pnpm.md` | 2 h | Rust + pnpm 双包管理边界 |
| 41 | `docs/adr/0005-109-tables-keep-schema.md` | 2 h | 数据库 schema 100% 兼容上游 |
| 42 | `docs/DESIGN.md` | 4 h | 设计哲学：协议冻结 vs 内部自由、双主体（user+agent）、UI 复用 |
| 43 | `docs/FAQ.md` | 3 h | 常见 20–30 问 |
| 44 | `docs/UPGRADING.md` | 2 h | 0.x → 0.y 升级、迁移路径 |

### 第 8 批：基础设施 [P1，~2 天]

| # | 项目 | Effort | 内容要点 |
| --- | --- | --- | --- |
| 45 | `.github/workflows/ci.yml` | 4 h | fmt + clippy + test + audit + doc + migrate-smoke |
| 46 | `.github/dependabot.yml` | 1 h | Cargo + npm + GitHub Actions |
| 47 | `.github/CODEOWNERS` | 1 h | 各 crate 负责人 |
| 48 | `docker/Dockerfile` | 4 h | 多阶段、musl、~80–120 MB |
| 49 | `docker-compose.yml` | 2 h | Postgres + server + UI |
| 50 | `scripts/{fmt,lint,test,audit,doc,release}.sh` | 4 h | 本地与 CI 复用 |

---

## 3. 客观评估：与顶级项目差距

| 维度 | 顶级项目典型状态 | paperclip-rs 现状 | 差距 |
| --- | --- | --- | --- |
| README 完整度 | Quickstart + 徽章 + 截图 + 引用 + 架构图 | ✅ 已建，缺徽章、截图、引用 | 补 6–8 项 |
| 合规文件 | 6/6 | 0/6 | 必补 |
| CI | 5+ 工作流（fmt/clippy/test/audit/coverage） | 0 | 必建 |
| API 文档 | docs.rs + 完整 rustdoc | 仅有源码注释 | 发版时补 |
| Examples | 4–10 个 | 0 | 应补 |
| ADRs | 决策日志 5+ | 0 | 应补 |
| Releases | SemVer + GitHub Release + Notes | 0 | 发版时建 |
| Issue 模板 | 3+ | 0 | 必补 |
| Discussion | 已开 | 0 | 必开 |
| mdBook | 已托管 | 0 | 应建 |
| CONTRIBUTING | 含 DCO/CLA、PR 流程 | 0 | 必补 |

---

## 4. 推荐执行路径（首轮 1–2 周）

按依赖关系最小化，能让项目立刻看起来"像样"：

**Day 1**：第 1 批全部（11 项，~0.5–1 天）
**Day 2–3**：第 2 批核心 5 件（5 项，~3 天）
**Day 4**：第 3 批 PROTOCOL.md（1 项，~1 天）
**Day 5**：第 8 批基础设施（6 项，~2 天，CI 与 docker 优先）
**Day 6**：第 7 批前 5 个 ADR（5 项，~0.5 天）
**Day 7**：第 4 批 ARCHITECTURE.md + MODULES.md（2 项，~1 天）
**Day 8–10**：第 4 批其余 + 第 5 批（按需）

完成后项目即可具备顶级项目的最低视觉与功能基线。

---

## 5. 验证清单（完成判据）

完成度可用下列证据验证：

- [ ] GitHub 仓库顶部 6 项徽章（License、CI、docs.rs、crates.io、Discord、Star History）
- [ ] `LICENSE` 文件 25+ 行 MIT 全文
- [ ] `cargo test --workspace` 在 CI 跑通
- [ ] `cargo doc --workspace --no-deps` 通过无 warning
- [ ] `cargo clippy --workspace -- -D warnings` 通过
- [ ] `cargo audit` 无 RUSTSEC 警告
- [ ] `crates.io` 至少发布 1 个 crate
- [ ] `docs.rs/paperclip-server` 有渲染页面
- [ ] mdBook 站点可访问，`docs/` 全部页面可读
- [ ] OpenAPI `/openapi.json` 可获取且结构兼容
- [ ] 至少 3 个 `examples/` 可编译运行
- [ ] 至少 5 个 ADR 已沉淀
- [ ] GitHub Releases 至少 1 个 tag

---

## 6. 与现有文档的协同关系

| 现有 | 处置 |
| --- | --- |
| `docs/02-PAPERCLIP-ARCHITECTURE.md` | 内容重写为 `docs/architecture/node-baseline.md`，保留为对照基线 |
| `docs/03-KAMEO-ACTOR-ANALYSIS.md` | 提炼为 `docs/architecture/actors.md`，原文归档 `docs/internal/historical/` |
| `docs/04-EXECUTION-PLAN.md` | 迁移到 `docs/internal/planning/execution-plan.md` |
| `docs/05-PROGRESS-AUDIT.md` | 迁移到 `docs/internal/audit/`（占 445 KB，未来按月切片） |
| `docs/06-LEXICAL-REAL-ROOT-CAUSE.md` | 迁移到 `docs/internal/historical/` |
| `docs/06-NODE-RUST-GAP-MATRIX.md` | 迁移到 `docs/internal/migration/` |
| `docs/07-COMPREHENSIVE-GAP-ANALYSIS.md` | 迁移到 `docs/internal/migration/` |
| `docs/08-RUST-MODULAR-ARCHITECTURE.md` | 重命名为 `docs/architecture/rust-modular.md` |
| `ARCHITECTURE-DIAGRAMS.md`（根） | 迁移到 `docs/architecture/diagrams.md`，README 引用新位置 |
| `MODULE-MAPPING.md`（根） | 迁移到 `docs/architecture/module-mapping.md` |
| `PROJECT-PLAN.md`（根） | 迁移到 `docs/internal/planning/project-plan.md`，README 引用 |
| `openspec/changes/paperclip-rs-rewrite/*` | 迁移到 `docs/internal/openspec/`，关键决策抽取进 ADR |

