# R789 + R791 - pc-work-products DB 集成 + 跨 crate 工作流 (6 PASS)

**日期**: 2026-08-18
**主题**: 真实 PostgreSQL 端到端验证 (DB 链路 + 跨 crate 工作流)
**crate**: pc-work-products + pc-issues (dev-dep)

## 背景

R782 (pc-documents pure split) + R783-R787 (5 个 0-测试 crate 加测) 完成单测维度.
但单测只能验证纯逻辑. 业务服务的真实 DB 行为需要集成测试.

R789 + R791 共 6 个集成测试, 在真实 55433 devdb 上验证:
- R789: pc-work-products 服务层完整 DB 链路 (pure -> create -> DB -> get)
- R791: 跨 crate 工作流 (pc-issues create/update + pc-work-products create/list)

## DB 环境

- PostgreSQL 17 监听 127.0.0.1:55433 (paperclip-rs devdb)
- paperclip_repos 库, paperclip 用户
- tokio::sync::Mutex 串行化 (TEST_LOCK)
- 测试结束 cleanup 删除 company 关联所有行

## R789 改动 (tests/r789_pure_db_integration.rs, 288 行)

### 3 个集成测试

| 测试 | 验证 |
|---|---|
| r789_pure_to_db_end_to_end | ImportIssueWorkProductRow -> pure import_row_to_create_input -> create_for_issue -> get_by_id 全链路 |
| r789_secondary_primary_clears_primary | 同 kind 第二次 is_primary=true -> 第一次被清空 |
| r789_different_kind_preserves_primary | 不同 kind (pr vs deployment) 各自保留 primary |

## R791 改动 (tests/r791_cross_crate_workflow.rs, 跨 crate)

### Cargo.toml 改动

pc-work-products 新增 dev-dep:
```toml
[dev-dependencies]
pc-issues = { path = "../pc-issues" }
```

### 3 个跨 crate 工作流测试

| 测试 | 验证 |
|---|---|
| r791_issue_to_work_product_lifecycle | create issue (todo) -> create PR WP -> update_status (in_progress) -> list_for_issue 验证关联不中断 |
| r791_issue_close_with_work_product | create PR + deployment WP -> close issue (done) -> WP 仍可访问 |
| r791_multiple_issues_independent_work_products | 2 个 issue 各自独立 WP, 互不干扰 |

## 验证

```bash
cargo test -p pc-work-products --test r789_pure_db_integration
# 3 passed; 0 failed

cargo test -p pc-work-products --test r791_cross_crate_workflow
# 3 passed; 0 failed
# finished in 0.19s
```

## 关键发现: service API 不一致

多个 crate 的 service 返回类型不一致, 这是改进点:

| service 方法 | 返回类型 | 用法 |
|---|---|---|
| pc-issues::IssueService::create | IssueRow 直接 | .await.expect("create") |
| pc-issues::IssueService::update_status | IssueRow 直接 | .await.expect("update") |
| pc-issues::IssueService::get | Option<IssueRow> | .await.expect("get").expect("some") |
| pc-work-products::create_for_issue | Result<Option<WorkProduct>> | .await.expect("create").expect("some") |
| pc-work-products::get_by_id | Result<Option<WorkProduct>> | 同上 |
| pc-documents::DocumentService::update | Result<Option<DocumentRow>> | .await.expect("update").expect("row") |
| pc-documents::DocumentService::lock_document | Result<Option<DocumentRow>> | 同上 |

改进方向 (R793+): 统一为 Result<T, ServiceError> 或 Result<Option<T>> 文档化.

## 累计 (32 跟踪 crate)

| 维度 | 数据 |
|---|---:|
| R789 增量 | +3 |
| R791 增量 | +3 |
| R756-R791 累计 | **3212** PASS (lib) + 11 DB integration |

## 后续计划

- R792 - pc-repos 拆分 pure/db (长期高风险, R776 改进 4.3)
- R793 - 统一 service API 返回类型 + 文档
- R794 - pc-tool 各子模块加测
- Adapter 永远跳过 (硬约束 #2)