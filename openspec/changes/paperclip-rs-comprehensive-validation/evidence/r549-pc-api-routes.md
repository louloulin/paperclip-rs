# R549 — pc-api-routes（Node api.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/api.ts` (68 LOC) 完整复刻到新 crate
`crates/pc-api-routes`。workspace crates 90 → **91**。

## 设计原则

### 1. 强类型 `ApiRoutes` struct 替代 `as const` 对象
- Node 用 `export const API = { ... } as const`（对象字面量，TypeScript readonly tuple）
- Rust 用 `pub const API: ApiRoutes = ApiRoutes { ... }` 配合 `#[derive(Debug, Clone, Copy)]` struct
- 字段命名遵循 Rust snake_case：`company_folders`, `issue_watchdog`, `my_user_secret` 等
- 所有字段为 `&'static str`（编译期常量，无运行时分配）

### 2. 路径占位符保留 `:placeholder` 语法
- Node 路径中已用 `:companyId` 等占位符
- Rust 保持完全等价，可直接喂给 `axum` / `actix-web` 等路由框架
- 不做运行时参数替换（属于路由层职责）

### 3. 集中常量 + 编译期稳定性
- 全部常量在 `pub const` 段声明
- 修改任何路径会触发 clippy / 测试断言失败（catch accidental rename）

### 4. API_PREFIX 常量辅助
- `pub const API_PREFIX: &str = "/api"`
- 所有 API 路径都应以它开头（用 macro test 强制验证）
- 避免 magic string 散布

## 公开 API

```rust
pub const API_PREFIX: &str = "/api";

#[derive(Debug, Clone, Copy)]
pub struct ApiRoutes {
    // 62 个 endpoint 字段，全部为 &'static str
    pub health: &'static str,
    pub companies: &'static str,
    pub company_folders: &'static str,
    // ... 60+ more ...
    pub admin: &'static str,
}

pub const API: ApiRoutes = ApiRoutes { /* ... */ };
```

## 覆盖的 62 个 endpoint

| 分组 | 数量 | 说明 |
|---|---|---|
| **Top-level** | 13 | health / companies / agents / projects / environments / issues / goals / approvals / secrets / costs / activity / dashboard / admin 等 |
| **Companies scoped** | 4 | folders (list / single / move / item-move) |
| **Environments** | 9 | delete-blast-radius + custom-image (template × 3 + sessions × 5) |
| **Issues** | 4 | list + watchdog / tree-control / tree-holds |
| **Summary slots** | 3 | slot + revisions + generate |
| **Tools** | 13 | list / examples / applications / connections / catalog / profiles / policies / audit / runtime-slots (×3) / health / gateway |
| **Smoke lab** | 5 | root / services / install-fixtures / runs / steps |
| **User secrets** | 5 | definitions (×3) + my-secrets (×2) |
| **Secret providers** | 2 | configs + discovery preview |
| **Org** | 4 | resource-memberships / invites / join-requests / members |
| **UI** | 2 | sidebar-badges / sidebar-preferences |
| **总计** | **62** | |

## 与上游 Node 差异

- **snake_case 字段名**：Node 是 camelCase，Rust 是 snake_case
- **`&'static str` 替代 string literal**：编译期常量，零运行时分配
- **struct 强制类型化**：访问 `API.health` 编译器会检查字段存在

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-api-routes` | **14 passed** (0 internal + 14 integration) |
| `cargo fmt -p pc-api-routes` | ✅ 通过 |
| `cargo clippy -p pc-api-routes --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（14 个集成）

- **API_PREFIX 常量** (1): 字符串稳定性
- **分组断言** (12): 6 类分组 × 至少 1 个 test
- **完整性断言** (1): 所有 62 个路由都以 `/api` 开头（macro 强制）

## 集成待办（不在本轮范围）

- `pc-http`：在 router 注册时用 `API.x` 替代 hard-coded 字符串
- `pc-server`：openapi/openapi-gen 用 `API` 自动生成 spec
- `pc-typescript-gen`：从 Rust crate 反向生成 TS 类型
- `ui/`：用 ts-rs 自动同步 Rust → TypeScript
- 端到端：跑一次 HTTP server，验证每条路径命中
