# R541 / M41 — pc-authz DB-backed ContextBuilder

## 本轮完成

新增 `crates/pc-authz/src/builder.rs`，从 DB 行构造 `Context`，使 pc-authz 可被路由 handler 直接调用。

### 模块内容

- `build_context(db, actor) -> Context`：
  - **System / Anonymous** → 空 Context
  - **User**：
    - `is_instance_admin=true` → 短路，不加载 membership / grants
    - 否则从 `company_memberships` 表加载所有 active membership
    - 推断 role（取最高：Owner > Admin > Operator > Member > Viewer）
    - 跨该公司集合加载 grants（去重）
  - **Agent**：
    - 查 `company_memberships WHERE principal_type='agent'`
    - 加载 grants

- `parse_permission_key(&str) -> Option<PermissionKey>`：把 DB 字符串反序列化为枚举，覆盖 21 个 PermissionKey

### 依赖

`Cargo.toml` 新增：
- `pc-db`（连接池）
- `pc-repos`（`CompanyMemberRepo` + `PrincipalPermissionGrantRepo`）
- `sqlx`（直接查询 `company_memberships`）

### 验证

- `cargo check -p pc-authz`：0 errors
- `cargo test -p pc-authz --lib`：**26 passed**（+2 解析器 round-trip 测试）
- `cargo check --workspace`：0 errors
- `cargo test --workspace --lib -- --test-threads=1`：**4955 passed**（+2）

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| Mention / consent / parent-report grant | ⏳ M42 |
| 路由集成 / middleware | ⏳ M42-M43 |
