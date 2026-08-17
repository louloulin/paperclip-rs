# R788 - pc-documents DB 集成测试 (5 PASS, 真实 PostgreSQL)

**日期**: 2026-08-17
**主题**: R782 pure split 后端到端验证 (PG 集成)
**crate**: pc-documents

## 背景

R782 把 pc-documents 的纯验证函数提取到 pure.rs (24 单测 PASS).
但纯函数只是输入层. 业务逻辑 (创建/更新/锁定/解锁/修订历史/hook 触发)
需要真实 PostgreSQL 验证才能确认 service 层 + pure split 完整链路.

R788 完成 5 个 DB 集成测试, 验证 service 层在真实 PG 下的行为.

## DB 环境

- PostgreSQL 17 监听 127.0.0.1:55433 (paperclip-rs devdb)
- paperclip_repos 库, paperclip 用户
- 每个测试用 tokio::sync::Mutex::const_new 串行化 (TEST_LOCK)
- 测试结束时 cleanup 删除 company 关联所有行

## 改动

### 新增 tests/r788_pure_db_integration.rs (287 行)

```rust
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:55433/paperclip_repos";
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) { ... }
async fn insert_company(pool: &PgPool) -> Uuid { ... }
async fn cleanup(pool: &PgPool, company_id: Uuid, document_id: Uuid) { ... }
```

### 5 个集成测试

| 测试 | 验证 |
|---|---|
| r788_create_document_persists_to_db | create -> hook Created event -> get 查询 roundtrip |
| r788_update_document_creates_revision_and_fires_updated | update body -> latest_revision=2 -> Updated event -> list_revisions 返回 2 个 |
| r788_lock_blocks_update | lock -> update 失败 (409) -> unlock -> update 成功 |
| r788_pure_validation_rejects_bad_input_before_db | 3 种 pure 拒绝: 空 body / 非法 format / nil companyId |
| r788_noop_hook_does_not_interfere | NoopDocumentHook + create 正常工作 |

## 验证

```bash
cargo test -p pc-documents --test r788_pure_db_integration
# 5 passed; 0 failed
# finished in 0.17s (真实 PG 串行化)
```

## 关键设计点

1. **串行化**: TEST_LOCK 让 5 个测试串行执行, 避免并发修改 PG 同一 company
2. **Cleanup**: 每个测试结束删除 documents/revisions/companies
3. **真实链路**: 通过完整 DocumentService (不只是 pure 函数), 验证 service -> repo -> sqlx -> PG
4. **Hook 验证**: 通过 RecordingDocumentHook 检查事件序列 (Created -> Updated -> Locked)

## 踩坑记录

1. DocumentService::update 返回 Option<DocumentRow>, 不是 DocumentRow
   需要 .expect("row") 后再访问字段
2. DocumentService::lock_document 第 4 参是 Option<&str>, 写 None 时需显式 None::<&str>
3. 虚拟 agent_id (Uuid::new_v4) 触发 documents_locked_by_agent_id_agents_id_fk FK 约束
   改用 None actor 绕过 (生产代码不会这样)
4. Rust 拒绝重复 use std::sync::Arc -> 用 awk 保留首次出现的 use
5. 既有 tests/*.rs 集成测试硬编码 5432 (与 devdb 55433 不匹配, 永远挂起)
   新 R788 测试文件用 55433, 与开发环境一致

## 累计 (32 跟踪 crate)

| 维度 | 数据 |
|---|---:|
| R788 增量 | +5 (DB integration) |
| R756-R788 累计 | **3206** PASS (lib) + 5 DB |
| pc-documents 测试总计 | 24 (lib pure) + 5 (DB integration) = 29 |

## 后续计划

- R789 - pc-work-products DB 集成测试
- R790 - pc-workspace-commands 跳过 DB (无 DB 依赖)
- R791 - 跨 crate 端到端流程 (issue -> agent -> work product)
- R792+ - pc-repos 拆分 pure/db (长期高风险)