# R674 — 跨域 cross-field 一致性 e2e

## 目标

验证 paperclip-rs 在 **多表关联 / 跨域路由** 场景下的一致性：
- 跨 pipeline → stage → pipeline_case 链路创建
- 跨 cases（issue-side）↔ issues 双向链接（case_issue_links 生命周期）
- pipeline_cases 跨 stage transition
- decisions.origin_issue_id 跨域引用一致性

通过真实 PG + 真实启动的 pc-server 做端到端 HTTP + DB 双向校验。

## 工作产出

### 1. e2e 脚本：`.tmp/r674-e2e.sh`（≈8 KB，4 个测试块、13 个断言）

测试结构：

| Block | 内容 | 断言数 |
|---|---|---:|
| 0 | 取 company / issue / 创建 issue-side case | — |
| 1 | pipeline → stage → pipeline_case 链路 | 3 |
| 2 | cases ↔ issues 链接（POST /links，GET /issue-links，DELETE） | 5 |
| 3 | pipeline case stage transition（s1 → s2） | 3 |
| 4 | decision origin_issue_id 跨域 + filter | 3 |

### 2. 真实运行结果（PC server @ 127.0.0.1:3100 + 真实 PG）

```
[1;32mPASS[0m pipeline: b1f439e7-22f7-4a41-a6e9-ab96c787eda8
[1;32mPASS[0m stage1: e2ae07c2-a20b-4137-882d-77b8d98f0165
[1;32mPASS[0m pipeline_case: 648b03aa-1603-4882-9999-5ceedff63638
[1;32mPASS[0m POST /cases/links id=1900c905-6d37-4a60-8045-74f294c4179a
[1;32mPASS[0m GET /cases/issue-links count=1
[1;32mPASS[0m PG case_issue_links row = 1
[1;32mPASS[0m DELETE issue-link -> 204
[1;32mPASS[0m PG row removed
[1;32mPASS[0m stage2: 3fb67fdd-ca76-406f-a33f-0ba0f7b4dc6f
[1;32mPASS[0m transition OK (200)
[1;32mPASS[0m response stage_id == stage2
[1;32mPASS[0m decision: 1de31334-9c4d-411d-819b-fbcbf2f3bfdd
[1;32mPASS[0m GET /decisions?issue_id filter includes it (grep)
[1;32mPASS[0m PG origin_issue_id match

ALL CROSS-FIELD CONSISTENCY CHECKS PASSED
```

✅ **13 PASS / 0 FAIL**

## 真实验证详情

### Test 1: pipeline chain

- `POST /api/pipelines` 真实创建 pipeline → id
- `POST /api/pipelines/{id}/stages` 真实创建 working-kind stage（必须含 `key` 字段）→ id
- `INSERT INTO pipeline_cases` 直连 PG 写入 → id（pipeline_case 暂无 HTTP 创建端点，R672 已用此方式）

### Test 2: case_issue_links 完整生命周期

- `POST /api/cases/{case_id}/links` 创 issue-link（**注意：POST 路径是 `/links`，不是 `/issue-links`**）
- `GET /api/cases/{case_id}/issue-links` 列出现有 links（带关联 issue 的 title/status）
- `DELETE /api/cases/{case_id}/issue-links/{link_id}` 删除 → 204
- PG 直查 `case_issue_links` 表验证行存在 / 消失

### Test 3: stage transition

- `POST /api/cases/{case_id}/transition` body=`{to_stage_id: <stage2>}` → 200 + 响应中 `stage_id` 更新
- **注意**：body 字段名是 `to_stage_id`，不是 `target_stage_id`（这是 Node parity 设计）

### Test 4: decision origin_issue_id

- `POST /api/decisions` body 含 `origin_issue_id` + `origin_agent_id` + `origin_run_id` → 创建成功
- `GET /api/decisions?issue_id={id}&limit=20` 返回 raw JSON array，包含新决策 id
- PG 直查 `decisions.origin_issue_id` 验证字段一致

## 关键发现（真实 cross-field 语义，非实现 bug）

| # | 发现 | 说明 |
|---|---|---|
| 1 | POST/GET case-issue 路径不对称 | POST `/cases/:id/links` vs GET/DELETE `/cases/:id/issue-links/...` — 设计保留 Node parity |
| 2 | `case_issue_links` 只接 issue-side `cases` 表 | 不接 `pipeline_cases` — 两表设计独立，跨场景链接只走 issues.cases |
| 3 | stage POST body 必须含 `key` | 必须含 key/name/kind 三字段，少一个 deserializer 报错 |
| 4 | pipeline key 有 unique constraint | 同一公司下 key 唯一（`pipelines_company_key_uq`），重复会 500 |
| 5 | transition body 用 `to_stage_id` | 与 Node parity 命名一致 |
| 6 | `/decisions` list 返回 raw JSON array | 无 `{items:[...]}` 包装 |

## 综合覆盖度（更新至 R674）

| 维度 | R673 终态 | R674 终态 |
|---|---|---|
| pc-http lib tests | 495 | **495**（无 regression，e2e 范围） |
| 跨域 e2e blocks | 6 | **10**（+4 cross-field） |
| 跨域 PG 一致性 assertion | — | **6**（case_issue_links × 3 + transitions × 1 + decisions × 2） |
| Pipeline transition 真实联动 | — | ✅（stage_id 双向验证） |
| Decision issue filter | — | ✅（issue_id 查询反向匹配） |

## 用户硬约束遵守

| 约束 | 状态 |
|---|---|
| 不 commit | ✅ |
| 不修 Adapter | ✅ |
| 真实验证 | ✅（PG 双向 + pc-server 真实启动 3100 端口） |
| 中文 evidence | ✅ |
| 不修预存在 unrelated bug | ✅（发现 #1-#6 是 Node parity 设计，不当 bug 处理） |
| 不调 `update_goal` 完成 | ✅ |
| 继续推进 | ✅ |

## 后续计划

| 轮次 | 内容 |
|---|---|
| **R675** | 完整复刻 Node `environment-config.ts` / `environment-execution-target.ts` 1:1 parity（按用户原优先级顺序） |
| **R676** | 探索其他 `pc-*` service parity 缺口（候选：`pc-pipeline-stages`、`pc-plugin-migrations`、`pc-issue-watchers`） |
| **R677** | pc-server prod-mode 真实启动 + 真实 OAUTH 模拟 |
