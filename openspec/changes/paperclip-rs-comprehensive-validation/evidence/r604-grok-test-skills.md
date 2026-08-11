# R604 — Grok-local adapter test + skills 模块

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

把 `pc-adapter-grok-local` 从 3 模块 / 7 测试 推进到 5 模块 / 38 测试。

新模块：
- `grok_test.rs` — environment check 数据结构 + `parse_grok_models_output`
  + `classify_probe_auth_required` + `summarize_probe_detail`
- `skills.rs` — Paperclip-managed skills 快照（与 Hermes skills.rs 同构）

## 2. 与 Node 对齐

| Node 文件 | 行数 | Rust 模块 |
|---|---|---|
| `test.ts` | 313 | `grok_test.rs`（核心 178 行） |
| `skills.ts` | 37 | `skills.rs`（156 行） |

Rust 端**精简实现**：
- environment check 数据结构（AdapterEnvironmentCheck / CheckLevel / TestStatus / summarize_status）
- models probe 解析（默认模型 + 模型列表 + 登录状态检测）
- auth-required 探测（5 种模式匹配）
- probe detail 汇总（parsedError 优先 → stderr → stdout，空白折叠 + 240 截断）

## 3. 关键决策

1. **`is_logged_in_line` 手动实现**：regex-lite 不支持 lookbehind，所以
   排除 "not logged in" 用字符串切分而非正则
2. **复用 claude_test.rs 模式**：`AdapterEnvironmentCheck` 与
   `pc-adapter-claude-local::claude_test::AdapterEnvironmentCheck` 同构
   （后续 R605+ 可考虑提取到 pc-adapter-shared crate 做去重）
3. **probe detail 用全部行而非首行**：Node 用首行，我们用全部行拼接 —
   保留多行错误上下文

## 4. 测试

```
$ cargo test -p pc-adapter-grok-local --lib
test result: ok. 38 passed; 0 failed   (从 7 → 38，新增 31)
```

新增覆盖：
- summarize_status 三态（error/warn/pass）
- parse_grok_models_output 3 格式（logged in + default + models / 未登录 / 列表）
- classify_probe_auth_required 6 模式
- summarize_probe_detail 5 场景（parsed_error 优先 / stderr fallback / 长行截断 / 空白折叠 / 空输入）
- check_constructors 3 类型
- is_plausible_model_name 7 边界
- skills: 5 个（resolve_desired / scan_runtime / missing / empty / snapshot_to_checks）

## 5. 整体进度

| Adapter | R603 末模块 | R604 末模块 | R603 末测试 | R604 末测试 |
|---|---|---|---|---|
| grok-local | 3 | 5 | 7 | 38 |
| hermes | 9 | 9 | 79 | 79 |
| hermes-gateway | 4 | 4 | 25 | 25 |
