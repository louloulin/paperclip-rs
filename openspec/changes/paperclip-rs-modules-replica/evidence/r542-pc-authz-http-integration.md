# R542 / M42 — pc-authz HTTP 路由集成（首次端到端接入）

## 本轮完成

新增 `crates/pc-authz/src/http.rs`，提供路由层便捷函数，并把 `enforce_permission` 接入 `companies.rs::create_label` 作为首个演示。

### http.rs 公开 API

```rust
pub async fn enforce(db: &Db, actor: &AuthContext, resource: Resource, action: Action)
    -> Result<(), AuthzError>;

pub async fn enforce_permission(db: &Db, actor: &AuthContext, company_id: Uuid, perm: PermissionKey)
    -> Result<(), AuthzError>;

pub async fn enforce_issue(db: &Db, actor: &AuthContext, resource: Resource, action: Action)
    -> Result<(), AuthzError>;

pub fn denial_to_string(err: AuthzError) -> String;
pub fn company_resource(company_id: Uuid) -> Resource;
```

### 演示路由接入：companies.rs::create_label

```rust
async fn create_label(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,    // ← 新增
    Json(body): Json<LabelBody>,
) -> ApiResult<Json<Value>> {
    // pc-authz: 写入公司资源需要 UsersInvite 权限（Operator 角色及以上）。
    if let Err(err) = enforce_permission(
        &state.db, &actor, company_id, PermissionKey::UsersInvite,
    ).await {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    // ... 原有业务逻辑
}
```

### 依赖更新

- `crates/pc-http/Cargo.toml`：新增 `pc-authz`
- `crates/pc-authz/Cargo.toml`：新增 `pc-db` + `pc-repos` + `sqlx`（在 M41 已加）

### 验证

- `cargo check -p pc-authz`：0 errors
- `cargo check -p pc-http`：0 errors
- `cargo test -p pc-authz --lib`：**28 passed**（+2 http helpers）
- `cargo test -p pc-http --lib`：**274 passed**（无回归）
- `cargo test --workspace --lib -- --test-threads=1`：**4957 passed**
- `bash scripts/diff-routes.sh`：**100.0%**（node=581 rust=883 missing=0）

### 接入策略（后续轮次参考）

`enforce_*` 函数的接入策略：
1. **写操作**：根据 Node `authorization.ts` 中对应 action 的 require_role 选择 PermissionKey
2. **读操作**：通常 allow company_member，必要时降级到 viewer
3. **跨公司访问**：pc-authz 已经通过 membership 检查自动 deny
4. **Agent key scope**：通过 `enforce_issue` 或自定义 action

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| HTTP 便捷 API（enforce_*） | ✅ M42 |
| 首个路由接入（create_label） | ✅ M42 |
| Mention / consent / parent-report grant 扩展 | ⏳ M43 |
| 全局接入所有受保护路由 | ⏳ M44 |
