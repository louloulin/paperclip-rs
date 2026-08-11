# R603 — Hermes adapter execute 路径整合 prompt/wake/skills 模块

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

把 R601 新增的 `prompt_template` + `wake_prompt` + `skills` 模块真正接入
`HermesAdapter::execute` 路径，提供可选的 `promptTemplate` 配置渲染 + wake
prompt 合并 + task markdown 注入能力。

## 2. 关键改动

新增 `render_full_prompt` 公共函数（`crates/pc-adapter-hermes/src/lib.rs`）：

```rust
pub fn render_full_prompt(
    context_prompt: &str,
    config: &Value,
    wake_payload: Option<&Value>,
    task_markdown: Option<&str>,
    session_handoff_markdown: Option<&str>,
) -> String
```

行为（对齐 Node `buildPrompt`）：
1. 渲染 `adapterConfig.promptTemplate`（若有）→ 条件段 → 变量替换
2. 渲染 wake prompt（`render_wake_prompt`）
3. 拼接顺序：wake prompt → handoff markdown → task markdown → 模板渲染结果
4. 空白段被过滤（`join_prompt_sections`）
5. 若无 `promptTemplate` → 直接用 `context.prompt`

## 3. 测试

新增 3 个集成测试：
- `render_full_prompt_no_template_returns_context_prompt`
- `render_full_prompt_with_template_renders_variables`  
- `render_full_prompt_joins_wake_and_task_sections`

```
$ cargo test -p pc-adapter-hermes --lib
test result: ok. 79 passed; 0 failed   (从 76 → 79，新增 3 个)
```

## 4. 整体架构现状（R603 末）

| Adapter | Rust lib.rs 行数 | Rust 子模块数 | Node execute.ts 行数 |
|---|---|---|---|
| hermes | 295 → 488（含 R603） | 9 | 596 |
| hermes-gateway | 296 | 4 (constants/config_schema/transport_security/lib) | 959 (HTTP 部分待后续) |
| cursor-local | 516 | 4 | 763 |
| pi-local | 312 | 4 | 847 |
| codex-local | ~600 | 13 | 1504 |
| claude-local | ~500 | 6 | 1270 |

Hermes 现在是子模块拆分最完整的 adapter（9 个模块），为后续 cursor-cloud /
grok / opencode / openclaw 复刻提供模板。

## 5. 关键架构经验

1. **adapter 子模块拆分模板**（以 Hermes 为例）：
   - `constants` — 常量（VALUES / 枚举 / 默认值）
   - `config_schema` — Paperclip UI schema
   - `detect_*` — 探测本地环境（~/.config, env, fs）
   - `resolve_*` — 决策谓词（优先级链）
   - `command_args` — CLI args 拼装 + stderr reclassification
   - `parse_output` — 子进程输出解析
   - `prompt_template` — 模板变量 / 条件段
   - `wake_prompt` — Paperclip wake payload 渲染
   - `skills` — skills 快照
   - `lib` — 整合所有模块 + Adapter execute

2. **transport_security 模式**（来自 Hermes-gateway）：
   - 把安全校验提到 Adapter execute **入口**（loopback vs 远端 HTTP）
   - escape hatch 显式开关，避免生产误用

3. **真实测试优先**：每个 adapter 都有 `roundXXX_*_end_to_end.rs` 用
   real shell 脚本 fake CLI 跑完整 execute 路径；不是只测解析器

## 6. 后续 R604+ 计划

| 优先级 | Adapter | Node 行数 | 状态 |
|---|---|---|---|
| P1 | cursor-cloud | 611 | stub 147 → 需要 SDK 集成 |
| P1 | openclaw-gateway | 1491 | stub 147 → 大 stub |
| P2 | grok-local | 588 | 255 行 lib |
| P2 | opencode-local | 720 | 266 行 lib |
| P2 | gemini-local | 759 | 274 行 lib |

