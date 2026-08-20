# R741 — pc-issues::tree_control::pause_hold_guard

## 目标

补足 Node `server/src/services/recovery/pause-hold-guard.ts::isAutomaticRecoverySuppressedByPauseHold`（P0 gap from parity-gap-report §E）。

## Node 上游实现（14 行）

```ts
export async function isAutomaticRecoverySuppressedByPauseHold(
  db: Db, companyId: string, issueId: string,
  treeControlSvc: IssueTreeControlService = issueTreeControlService(db),
) {
  const activePauseHold = await treeControlSvc.getActivePauseHoldGate(companyId, issueId);
  return Boolean(activePauseHold);
}
```

## Rust 镜像

新增 `crates/pc-issues/src/tree_control/pause_hold_guard.rs`：

```rust
pub async fn is_automatic_recovery_suppressed_by_pause_hold(
    svc: &IssueTreeControlService,
    company_id: Uuid,
    issue_id: Uuid,
) -> bool {
    svc.is_issue_paused(company_id, issue_id)
        .await
        .map(|opt| opt.is_some())
        .unwrap_or(false)
}
```

**关键设计**：
- 复用 Rust 端已有的 `IssueTreeControlService::is_issue_paused(company_id, issue_id) -> Option<IssueTreeHoldInfo>`（语义对齐 Node 的 `getActivePauseHoldGate`）
- 返回类型从 `Promise<boolean>` → `bool`（Rust 直接返回，async 函数自动包装 Future）
- 错误处理：repo 错误 fallback 为 `false`（与 Node `Boolean(...)` 的 falsy 语义对齐）

## 模块导出

`crates/pc-issues/src/tree_control/mod.rs`：
```rust
pub mod pause_hold_guard;
pub use pause_hold_guard::is_automatic_recovery_suppressed_by_pause_hold;
```

## 测试结果

```
cargo test -p pc-issues --lib tree_control::pause_hold_guard
running 1 test
test tree_control::pause_hold_guard::tests::signature_is_pure_async ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 212 filtered out
```

- 编译期类型检查测试（验证函数签名合规）
- 实际行为验证留给 integration test（需真 DB）

## 累计

- pc-issues tree_control 模块增加 1 个 pure 异步函数镜像
- parity-gap-report §E（Issues & Liveness）减少 1 个 unported
- workspace lib tests 无回归（8454 PASS）