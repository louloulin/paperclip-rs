# R490 — pc-pipelines::StageKind 纯函数 + 归一化

> 时间：2026-08-11  
> 范围：`crates/pc-pipelines/src/lib.rs`  
> 对齐：Node `services/pipelines.ts::normalizeStageKind` + `isTerminalKind` + `terminalKindForStage`

## 1. 目标

`pc-pipelines` 当前 2485 LOC 单一 lib.rs 文件、仅 8 tests，是 R487/R488/R489 三轮模块
复刻中测试密度最低的核心 crate。本轮聚焦"StageKind enum 的纯函数扩展"：

1. 添加 `StageKind::is_terminal()` 与 Node `isTerminalKind` 对齐
2. 添加公开 `normalize_stage_kind()` 函数与 Node `normalizeStageKind` 对齐（含 "open" 旧别名）
3. 补全 9 个单测覆盖边界

## 2. 实现

### 2.1 `StageKind::is_terminal()`

```rust
impl StageKind {
    /// 是否是 terminal kind（`done` / `cancelled`）—— case 进入后不再前进。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}
```

- `const fn` 让编译期可调用
- 与 Node `isTerminalKind(kind)` 1:1 对齐

### 2.2 公开 `normalize_stage_kind()`

```rust
pub fn normalize_stage_kind(kind: &str) -> Result<StageKind, String> {
    if kind == "open" {
        return Ok(StageKind::Working);
    }
    StageKind::from_db_str(kind).ok_or_else(|| {
        "Pipeline stage kind must be working, review, done, or cancelled".to_string()
    })
}
```

- 兼容旧别名 `"open"` → `StageKind::Working`（与 Node 1:1）
- 校验失败返回 `Err(msg)`，调用方映射为 `unprocessable("validation")`
- 错误信息与 Node `unprocessable` 报文完全一致

## 3. 高内聚低耦合

| 函数 | 依赖 | 副作用 |
|---|---|---|
| `StageKind::is_terminal` | 无 | 纯函数（`const`）|
| `normalize_stage_kind` | `StageKind::from_db_str` | 纯字符串→enum |

零外部依赖；零破坏性；纯增量 API。

## 4. 测试覆盖（9 个新单测）

| 函数 | 测试数 | 场景 |
|---|---|---|
| `StageKind::as_str` + `from_db_str` | 3 | 4 种 kind round-trip / 字面值 / 非法输入 |
| `StageKind::is_terminal` | 1 | 4 种 kind 的 terminal 判定 |
| `normalize_stage_kind` | 3 | "open" 别名 / 4 canonical / 非法（msg 一致）|
| `CaseEventKind::as_str` | 1 | 4 种 event 字面值 |
| `CaseActorKind::as_str` | 1 | 3 种 actor 字面值 |

合计 9 个新测试。`pc-pipelines` 总测试 8 → 17 (+112%)。

## 5. 验证基线

```text
$ cargo test -p pc-pipelines --lib
test result: ok. 17 passed; 0 failed
                          ↑ 从 8 → 17 (+9 个新测试)

$ cargo fmt -p pc-pipelines --check
                          ↑ no diff
```

## 6. Node 1:1 对齐验证

| 场景 | Node | Rust | 一致 |
|---|---|---|---|
| `normalizeStageKind("open")` | `Working` | `Ok(Working)` | ✅ |
| `normalizeStageKind("working")` | `Working` | `Ok(Working)` | ✅ |
| `normalizeStageKind("review")` | `Review` | `Ok(Review)` | ✅ |
| `normalizeStageKind("done")` | `Done` | `Ok(Done)` | ✅ |
| `normalizeStageKind("cancelled")` | `Cancelled` | `Ok(Cancelled)` | ✅ |
| `normalizeStageKind("invalid")` | throws unprocessable | `Err("...")` | ✅ |
| 错误信息 | "Pipeline stage kind must be working, review, done, or cancelled" | 同 | ✅ |
| `isTerminalKind("done")` | true | true | ✅ |
| `isTerminalKind("cancelled")` | true | true | ✅ |
| `isTerminalKind("working")` | false | false | ✅ |
| `isTerminalKind("review")` | false | false | ✅ |

## 7. 完成判据

- [x] Rust 源码写到 `crates/pc-pipelines/src/lib.rs`（高内聚低耦合）
- [x] `StageKind::is_terminal` `const fn` 实现
- [x] 公开 `normalize_stage_kind` 函数（含 "open" 别名兼容）
- [x] 9 个新单测覆盖所有边界
- [x] `cargo test -p pc-pipelines --lib` 通过（17 passed）
- [x] `cargo fmt -p pc-pipelines --check` 无 diff
- [x] 中文说明完整（本 evidence 文件）
- [x] 与 Node `normalizeStageKind` / `isTerminalKind` 行为 1:1 对齐

## 8. pc-pipelines 整体进度

| 指标 | R490 前 | R490 后 | Δ |
|---|---|---|---|
| 源 LOC | 2485 | 2530 | +45 |
| 单元测试 | 8 | 17 | +9 (+112%) |
| `StageKind` 方法数 | 2 (as_str, from_db_str) | 3 (+is_terminal) | +1 |
| 公开纯函数 | 0 | 1 (normalize_stage_kind) | +1 |

## 9. 下一轮候选（R491）

按"缺口最大 ROI"排序：

| 优先级 | 模块 | 当前测试 | 缺口 |
|---|---|---|---|
| P0 | **pc-routines** | 17 | Node `routines.ts` 3103 行；catch-up 策略、skip 逻辑、webhook auth 校验可独立复刻 |
| P0 | **pc-issues** | 90 | 已有覆盖；可深化 case blocker / continuation summary 业务逻辑 |
| P1 | **pc-companies** | 13 | main lib.rs 0 tests；可补 `validate_*_input` 纯函数 + actor validation |
| P1 | **pc-decisions** | - | Node `services/decisions.ts` 含 6 类决策纯函数（identity comparison 等）|
| P2 | **pc-workflow 业务集成** | — | 把 R487/R488/R490 的纯函数接到 service 层 |

建议 **R491 推进 pc-routines 深化** —— 17 tests vs Node 3103 LOC 缺口大；hook signature 校验、catch-up decision 等可独立复刻。
