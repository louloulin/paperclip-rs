# R544 / M44 — pc-authz 多路由接入（approvals + labels）

## 本轮完成

把 `pc-authz::enforce_permission` 接入 3 个真实写路由：

### 路由清单

| 路由 | PermissionKey | 行为 |
|---|---|---|
| `POST /api/companies/:id/labels` | `UsersInvite` | 创建 label 需要 Operator 角色 |
| `POST /api/approvals/:id/approve` | `UsersInvite` | 批准 approval 需要 Operator 角色 |
| `POST /api/approvals/:id/reject` | `UsersInvite` | 拒绝 approval 需要 Operator 角色 |

### 集成模式

```rust
async fn approve_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(body): Json<ApproveRejectBody>,
) -> ApiResult<Json<Value>> {
    // 1. 查 company_id（用于 authz 资源）
    let preview_company: Option<(Uuid,)> = sqlx::query_as(
        "SELECT company_id FROM approvals WHERE id = $1",
    )
    .bind(approval_id)
    .fetch_optional(state.db.pool())
    .await?;
    let preview_company_id = preview_company
        .ok_or_else(|| ApiError::NotFound(format!("approval {approval_id}")))?
        .0;

    // 2. pc-authz 决策
    if let Err(err) = enforce_permission(
        &state.db, &actor, preview_company_id, PermissionKey::UsersInvite,
    ).await {
        return Err(ApiError::Forbidden(err.to_string()));
    }

    // 3. 业务逻辑
    let row = ApprovalRepo::new(&state.db).decide_four_args(...).await?;
    // ...
}
```

### 验证

- `cargo check -p pc-http`：0 errors
- `cargo test --workspace --lib -- --test-threads=1`：**4962 passed**（无回归）
- `bash scripts/diff-routes.sh`：**100.0%**（node=581 rust=883 missing=0）
- 3 个新接入路由，业务路径不变；新加的 403 forbidden 走 pc-authz 决策

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| HTTP 便捷 API（enforce_*） | ✅ M42 |
| Mention / consent / parent-report / responsible_user | ✅ M43 |
| 多路由接入演示（labels + approvals × 2） | ✅ M44 |
| 全量接入所有受保护路由 | ⏳ 渐进（大量路由，需分批） |
