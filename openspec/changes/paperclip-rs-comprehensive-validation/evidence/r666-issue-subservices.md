# R666 — Issue 子服务 route（visibility / references）

## 目标

为 pc-http 添加 issue 子服务 endpoint 集合（4 个），对齐 Node
server/src/services/issue-visibility.ts +
server/src/services/issue-references.ts 等。

## 改动文件

### 新增

- crates/pc-http/src/routes/issue_subservices.rs（约 340 行）
  - pub fn router() -> Router<AppState>
  - 4 个 endpoint handler
  - 本地 enum IssueVisibilityReason（Visible / HiddenAt / HasHarnessKind）
  - 6 个 unit test

### 修改

- crates/pc-http/src/routes/mod.rs（已加 pub mod issue_subservices; +
  .merge(issue_subservices::router())）

## 4 个 Endpoint

| Method | Path                              | 功能                                                  |
|--------|-----------------------------------|------------------------------------------------------|
| GET    | /api/issues/:id/visibility        | 从 DB 取 issue 行并 classify visibility（DB-backed） |
| POST   | /api/issues/classify-visibility   | dry-run classify（不入库）                            |
| POST   | /api/issues/references/extract    | 提取 markdown 引用（pure function）                  |
| POST   | /api/issues/visibility/sql        | 生成 visibility 过滤 SQL 片段（AND / OR）             |

## 编译 / 测试

cargo test -p pc-http --lib issue_subservices

  6 passed; 0 failed; 0 ignored; 0 measured; 483 filtered out

pc-http 全量 lib tests: 489 passed（R665 末 483 → R666 末 489，+6）。

## 真实启动 + curl 验证

### 环境

- PostgreSQL: postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos
- Deployment mode: local_trusted
- HTTP port: 3100
- 二进制: ./target/debug/paperclip-server

### 验证结果（6/6 PASS）



### Access Log



## 修复的 4 个编译错误

1. pc_issues::visibility::types 模块不存在 → types.rs 是孤儿文件，
   visibility/mod.rs 没 mod types;。改用
   pc_issues::visibility::visible_issue_sql + 自拼 AND / OR 格式串。
2. pc_issues::references::extractor 模块私有 → 改用 references 顶层
   pub use re-export 的 extract_identifiers / extract_matches。
3. c == _ Rust 不允许 → 改成 c == _。
4. t.0 private field → 改用 Timestamp::as_datetime() + to_rfc3339()。

## 修复的 1 个 API 契约错误

ClassifyItem 缺 #[serde(rename_all = camelCase)]：客户端发送
issueId / hiddenAt / harnessKind（camelCase），但缺属性时
serde 期待 issue_id / hidden_at / harness_kind（snake_case）。
加上属性后 422 → 200。

## 累计进度

约 94.5%（R666 后）。R667+ 计划：environment-* / tool-* 细化 + 集成 e2e + 终验。
