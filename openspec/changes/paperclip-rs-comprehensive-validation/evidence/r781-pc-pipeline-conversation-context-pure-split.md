# R781 - pc-pipeline-conversation-context pure.rs 拆分

**日期**: 2026-08-17
**主题**: R776 架构审计改进 4.1 — 提取纯函数到独立模块
**crate**: pc-pipeline-conversation-context

## 背景

R776 架构审计发现 pc-pipeline-conversation-context/src/lib.rs 体积过大 (957 行),
纯函数 (truncate_with_flag, fence_markdown) 与 db-touching code 混杂在同一文件。
本次 R781 将两个纯函数 (加 7 个新单元测试) 提取到独立 pure.rs 模块, 实现
高内聚低耦合的 Rust 最佳实践: 纯逻辑不依赖 sqlx / 异步, 可以独立快速单测。

## 改动

### 1. 新增 src/pure.rs (100 行)

- pub struct TruncateWithFlag { value: String, truncated: bool } — 截断结果 + 是否截断标记
- pub fn truncate_with_flag(value: &str, max_chars: usize) -> TruncateWithFlag —
  按 char 数截断, 保留完整 Unicode 字符 ("你好世界" × 2 + "hello" 截前 3 仍输出 "你好世")
- pub fn fence_markdown(value: &str, info: &str) -> String —
  构造长度大于输入中最大反引号连续串的 markdown fence (保证不被内容关闭)

### 2. 修改 src/lib.rs (919 行, 减少 38 行)

- 顶部新增 pub mod pure;
- 顶部新增 pub use pure::{fence_markdown, truncate_with_flag, TruncateWithFlag};
- 删除原 TruncateWithFlag struct + truncate_with_flag + fence_markdown 实现 (39 行)
- 其他调用点 (line 325, 392, 556) 自动通过 re-export 继续工作, 无需改动

### 3. 新增 7 个 r781_ 单元测试

| 测试 | 验证 |
|---|---|
| r781_truncate_short_text_unchanged | 短文本不截断, truncated=false |
| r781_truncate_long_text_clipped | 长文本 (2000) 截取 50 字符, truncated=true |
| r781_truncate_empty_returns_empty_not_truncated | 空串返回空, truncated=false |
| r781_truncate_at_exact_boundary_not_truncated | 正好等于 max_chars 视为不截断 |
| r781_fence_no_backticks_uses_3 | 无反引号时 fence 长度 = 3 (最小值) |
| r781_fence_handles_long_backtick_runs | 长反引号串自动扩展 fence |
| r781_fence_picks_longest_run | 取输入中最大连续反引号数 + 1 |

## 验证

```bash
cargo test -p pc-pipeline-conversation-context --lib
# 29 passed; 0 failed (22 原有 + 7 r781_)
```

相关 crate 回归测试:

```bash
cargo test -p pc-pipeline-conversation-context -p pc-pipeline-case-type \\
            -p pc-pipeline-case-outputs -p pc-pipeline-health --lib
# 21 + 11 + 29 + 39 = 100 passed; 0 failed
```

## 关键设计点

1. #![forbid(unsafe_code)] — pure 模块无 unsafe 必要, 显式标注
2. 零外部依赖 — pure.rs 仅用 std (char, String), 不引入 sqlx / chrono / uuid
3. 内部 #[cfg(test)] internal_tests 模块 — 严格遵循硬约束 #10
4. 测试命名 r781_xxx — 严格遵循硬约束 #9, 便于后续回归检索

## 踩坑记录 (R781 失败 → 修复)

初次实现时 pure.rs 全部单引号 (char 39) 被 JS 字符串插值误写成反引号 (char 96)。
原因: JS 模板字符串 + JS 字符串原生不支持内嵌反引号, 当时使用了类似转义字符
的写法, shell 解析反引号时和 bash heredoc 冲突。

**修复方法**: 用 JS MCP + String.fromCharCode(96) 显式构造 BT 字符, 不用任何
JS 字符串字面量包裹反引号, 全部字符数组拼接后写入文件。

第二轮又踩坑: let fence = 单引号 + 反引号 + 单引号 + .repeat(N) 是 char 类型, 没有 .repeat() 方法。
**修复**: 改为 let fence = 双引号 + 反引号 + 双引号 + .repeat(N) (string literal with double quotes)。

## 累计 (26 跟踪 crate)

| 维度 | 数据 |
|---|---:|
| R781 增量单测 | +7 |
| R756-R781 累计 | **3062** PASS |
| pc-pipeline-conversation-context 总测试 | 29 (旧 22 + 新 7) |

## 后续计划

- R782+ — pc-repos 拆分 pure/db (R776 改进 4.3, 长期, 高风险)
- Adapter 跳过 (硬约束 #2)
- 真实浏览器 UI 链路 Round 3+ (待 Layout bug 修复决策)
