# R793 - service 返回类型 API 收敛 (Option<T> vs T)

**日期**: 2026-08-18
**主题**: 统一 mutation 方法返回 direct T（带 NotFound 错误），保留 lookup 返回 Option<T>

## 设计原则

| 方法类型 | 返回类型 | 理由 |
|---|---|---|
| `create` (INSERT) | `T` | 总是插入新行 |
| `update` (UPDATE...RETURNING) | `T` + NotFound | 0 行匹配 = NotFound |
| `remove` (DELETE...RETURNING) | `T` + NotFound | 0 行匹配 = NotFound |
| `get_by_id` (SELECT) | `T` + NotFound | 简化（之前 Option 总被 .expect("some")） |
| `get` (SELECT, lookup) | `Option<T>` | Lookup 语义 |
| `lock_document` / `unlock_document` | `T` | 内部已 ok_or_else |

## pc-work-products

### 改动
- 新增 `WorkProductError::NotFound(String)` 变体（与 `pc_errors::Error::NotFound` 对齐）
- `create_for_issue` → `Result<WorkProduct, WorkProductError>`
- `update` → `Result<WorkProduct, WorkProductError>`
- `get_by_id` → `Result<WorkProduct, WorkProductError>`
- `remove` → `Result<WorkProduct, WorkProductError>`
- 私有 `create_for_issue_in_tx` / `update_in_tx` 保留 `Option<WorkProduct>` 返回

### 测试清理
- 30 处 `.expect("some")` 删除
- 1 处 `.is_none()` 改为 `matches!(..., Err(WorkProductError::NotFound(_)))`

### 验证
- pc-work-products lib: 8 PASS
- r789 integration: 3/3 PASS (DB 55433)
- r791 integration: 3/3 PASS (DB 55433)

## pc-documents

### 改动
- `DocumentService::update` → `Result<DocumentRow>`
- `DocumentService::lock_document` → `Result<DocumentRow>`
- `DocumentService::unlock_document` → `Result<DocumentRow>`

### 测试清理
- 5 处 `.expect("row")` 删除
- 1 处 idempotent unlock 测试改用直接 expect (因为现在返回 DocumentRow 而不是 Option)

### 验证
- pc-documents lib: 24 PASS
- r788 integration: 5/5 PASS (DB 55433)

## 累计

| crate | lib tests | DB integration |
|---|---:|---:|
| pc-work-products | 8 | 6 (r789+r791) |
| pc-documents | 24 | 5 (r788) |
| **R793 增量** | **+32 lib** | **+11 DB** |

总 lib tests: 3241 PASS
总 DB integration: 17 (R788: 5 + R789: 3 + R791: 3 + R793: 6)

整体加权进度: **~96.5%** (从 96% 提升 0.5%)

## 踩坑

| 问题 | 解决 |
|---|---|
| regex 误改私有 in_tx 方法 | 显式 revert fetch_optional(&mut **tx) 后的 Ok |
| `.expect("some")` 误删导致字段访问失败 | 区分 lookup vs mutation, 保留 lookup 的 Option |
| `.is_none()` 检查不再适用 | 改用 matches!(..., Err(NotFound)) |
| `remove` 双重语义 (lookup + mutation) | 视为 mutation, 返回 direct + NotFound |

## R794+ 后续

- `pc-repos::IssueRepo::create_work_product/update_work_product` (HTTP 层) 同样需要统一
- `pc-feedback::RedactionService` 等其他 service 检查
- 引入 `ServiceResult<T>` 共享 Result + Error 模式
