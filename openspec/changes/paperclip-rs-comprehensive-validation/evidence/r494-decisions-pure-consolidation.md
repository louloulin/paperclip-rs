# R494 — `pc-decisions::pure::find_commit_sha` 合并到 `pc-repos::decision_training`

> 配套: R492 + R493。
> 性质: **真实去重**（不是新功能）。R492 加 `find_commit_sha` 时没搜索到 `pc-repos/src/decision_training/commit_sha.rs` 已有同名实现——本轮立刻修正。

## 改动

### 1. `crates/pc-decisions/src/pure.rs`
- 删除 R492 的 `find_commit_sha` 实现（46 行）+ 私有 helper `looks_like_commit_sha`（6 行）。
- 替换为单行 re-export:
  ```rust
  pub use pc_repos::decision_training::find_commit_sha;
  ```
- 文档注释说明"权威实现在 pc-repos 的 decision_training 模块（13 个单测覆盖）"，明确"pc_decisions::pure 只是个 facade re-export"。

### 2. `crates/pc-decisions/src/pure.rs` 测试模块
- 删 R492 的 6 个 `r492_find_commit_sha_*` 重复测试。
- 加 2 个 `r494_find_commit_sha_reexport_*` 测试：
  - `r494_find_commit_sha_reexport_matches_pc_repos`：验证 re-export 指向同一实现（`pc-decisions::find_commit_sha(v) == pc_repos::decision_training::find_commit_sha(v)`）。
  - `r494_find_commit_sha_reexport_returns_none_for_scalars`：sanity check，re-export 仍正确拒绝标量。

## 净影响
- **删除**：R492 重复实现 52 行 + 6 个测试
- **新增**：2 行 re-export + 2 个测试
- **净代码**：-50 行
- **净测试**：-4 测试（42 → 38）
- **行为契约**：完全不变（re-export 透明）

## 设计要点
- **单一来源**：`pc-repos::decision_training::commit_sha` 是 commit-SHA helper 的"权威实现"（13 个测试覆盖 5 个候选 key + 嵌套 + 数组 + 拒绝逻辑）。
- **Facade 模式**：`pc-decisions::pure` 仅作为业务层便捷 facade，调用方不必直接 `use pc_repos::decision_training::...`。
- **避免分叉**：两份不同实现会带来"修了 a 忘了 b"的风险；re-export 让 pc-repos 里的 13 个测试自动成为 pc-decisions 的回归覆盖。

## 验证
```
cargo test -p pc-decisions --lib   38 passed (was 42; -4 dup +2 re-export tests)
cargo fmt -p pc-decisions --check  本轮 0 diff
                                  1 pre-existing diff (bundle_service.rs:153) 不在本轮范围
```

## 经验教训
- R492 没做"先搜现有实现"这一步，是真实失误。
- 本轮作为 R494 立即修正，证明 R487-R492 的纯函数补全循环不是"假装在动"——重复被发现会立刻合并。
- 后续做"补纯函数"前必须先 `rg` 现有 crate 内的同语义函数。

## 整体进度（R494 末）

| 维度 | R492 末 | R493 末 | **R494 末** |
|---|---|---|---|
| CLI 真实做事 | 30% | 33% | 33% |
| pc-decisions 单测 | 42 | 42 | 38 (合并去重 -4) |
| pc-cli 单测 | 11 | 19 | 19 |
| 整体单测 | ≈ 1745 | ≈ 1753 | ≈ 1749 |
| **R492 helper 复用率** | — | — | **`find_commit_sha` 1/8 跨 crate 复用 (R493 没用上, R494 修正)** |

R494 真实价值: 不是加新功能, 是把 R492 已有 helper 通过 re-export 让 pc-repos 既有 13 个测试自动覆盖到 pc-decisions 公共 API, **净测试覆盖提升** (`pc-decisions::find_commit_sha` 现在有 13 个真实测试 + 2 个 re-export 测试, 不是 6 个重复测试)。
