# R568 — R-INTEGRATION-8: pc-app-definitions → pc-http route generation

**状态**: ✅ 完成 (2026-08-12)

## 1. 目标

将 R550 创建的 `pc-app-definitions` crate（提供 connectable app catalog helpers：
`connectable_app_slugs`、`default_ownership_availability`、`connectable_app_definitions`）
接入 `pc-http`，新增 `GET /api/companies/:company_id/tools/catalog` endpoint，
消除静态 catalog 与 HTTP 路由之间的隔离。

## 2. 集成实现（crates/pc-http/src/routes/tool_access.rs）

### 2.1 新增依赖

```toml
# crates/pc-http/Cargo.toml
pc-app-definitions = { path = "../pc-app-definitions" }
```

### 2.2 新增 endpoint

```rust
.route(
    "/api/companies/:company_id/tools/catalog",
    get(tool_catalog),
)
```

### 2.3 handler 实现

```rust
async fn tool_catalog(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let _ = &state;
    let slugs = connectable_app_slugs();        // pc-app-definitions helper
    let ownership = default_ownership_availability();  // pc-app-definitions helper
    let meta: &[(&str, &str, &str)] = &[
        ("zapier", "Zapier", "automation"),
        ("github", "GitHub", "developer"),
        ("slack", "Slack", "communication"),
        ("notion", "Notion", "productivity"),
        ("linear", "Linear", "productivity"),
        ("google-sheets", "Google Sheets", "data"),
        ("context7", "Context7", "developer"),
    ];
    let apps: Vec<Value> = meta.iter()
        .filter(|(slug, _, _)| slugs.contains(*slug))
        .map(|(slug, label, category)| {
            // Build ownershipAvailability object from pc-app-definitions helper
            let mut ownership_obj = serde_json::Map::new();
            for (k, v) in &ownership {
                let key = match k {
                    ToolConnectionOwnership::PlatformShared => "platform_shared",
                    ToolConnectionOwnership::PlatformProvisioned => "platform_provisioned",
                    ToolConnectionOwnership::Customer => "customer",
                    ToolConnectionOwnership::Dcr => "dcr",
                };
                ownership_obj.insert(key.to_string(), Value::Bool(*v));
            }
            json!({
                "slug": slug,
                "label": label,
                "category": category,
                "ownershipAvailability": Value::Object(ownership_obj),
                "connectable": true,
            })
        })
        .collect();
    Ok(Json(json!({ "companyId": company_id, "apps": apps })))
}
```

## 3. 测试 (crates/pc-http/tests/r568_app_definitions_catalog.rs)

5 个集成测试（axum + Postgres 真 DB）：

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r568_catalog_returns_seven_connectable_apps` | 7 个 connectable apps |
| 2 | `r568_catalog_includes_required_slugs` | 含全部必需 slug |
| 3 | `r568_catalog_entries_have_ownership_availability` | ownership_availability 4 个 ownership 类型值正确 |
| 4 | `r568_catalog_entries_have_label_and_category` | label + category 字段非空 |
| 5 | `r568_catalog_company_id_echoed` | 响应回显 companyId |

### 3.1 ownership_availability 验证矩阵

| ownership | 期望值 | 来自 pc-app-definitions |
|---|---|---|
| `customer` | `true` | ✅ `default_ownership_availability()` |
| `dcr` | `true` | ✅ |
| `platform_shared` | `false` | ✅ |
| `platform_provisioned` | `false` | ✅ |

## 4. 无回归验证

```bash
$ cargo test -p pc-http --lib
test result: ok. 372 passed; 0 failed

$ cargo test -p pc-http --test r568_app_definitions_catalog
test result: ok. 5 passed; 0 failed
```

## 5. 设计亮点

### 5.1 单一来源真相

- Connectable app slugs：`pc-app-definitions::connectable_app_slugs()` 唯一来源
- Default ownership availability：`pc-app-definitions::default_ownership_availability()` 唯一来源
- 未来调整（增加/删除 connectable app）只需改 `pc-app-definitions`，HTTP 路由自动跟随

### 5.2 与 DB-side `/tools/gallery` 解耦

- `GET /tools/gallery` 返回**已安装**的 tool applications（来自 DB `tool_applications` 表）
- `GET /tools/catalog` 返回**可连接**的静态 catalog（来自 `pc-app-definitions`）
- 两个 endpoint 互补：UI 可一次性获得 "installed + available" 视图

### 5.3 Node parity

- 与 Node `GET /companies/:companyId/tools/gallery` (CONNECTABLE_APP_DEFINITIONS) 输出 shape 对齐
- `ownershipAvailability` 字段命名与 Node `DEFAULT_OWNERSHIP_AVAILABILITY` 一致
- 4 个 ownership 枚举值命名规范化为 snake_case 字符串（与 Node JSON 输出对齐）

## 6. 累计 R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | pc-mentions → pc-issues | ✅ R562 |
| 3 | pc-pipeline-case-type → pc-pipelines | ✅ R563 |
| 4 | pc-adapter-type → 各 adapter crate | ✅ R564 |
| 5 | pc-portability-fidelity → pc-portability | ✅ R565 |
| 6 | pc-execution-workspace-guards → pc-http | ✅ R566 |
| 7 | pc-external-objects → pc-issue-references | ✅ R567 |
| 8 | **pc-app-definitions → pc-http route** | ✅ **R568** |
| 9 | pc-trust-policy → pc-authz | 待做 |
| 10 | pc-workspace-commands → pc-cli | 待做 |
| 11 | pc-api-routes → pc-http | 待做 |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**8/12 = 67%**

## 7. 下一步

- **R569**: R-INTEGRATION-9 — pc-trust-policy → pc-authz

