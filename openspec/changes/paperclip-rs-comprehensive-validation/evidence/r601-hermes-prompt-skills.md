# R601 — Hermes prompt_template + wake_prompt + skills 模块

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

把 Hermes adapter 从"CLI adapter"升级为"完整 Paperclip 集成 adapter"：
- **prompt 渲染**：模板变量替换 + 条件段（对齐 Node `renderTemplate` +
  `renderConditionalSections` + `joinPromptSections`）
- **wake payload**：recovery / execution contract 渲染（对齐 Node
  `renderPaperclipWakePrompt` + `selectPaperclipTaskMarkdown`）
- **skills 扫描**：合并 `~/.hermes/skills/` + Paperclip-managed runtime
  skills（对齐 Node `buildHermesSkillSnapshot`）

## 2. 新增模块（高内聚、低耦合）

| 模块 | 行数 | 职责 |
|---|---|---|
| `prompt_template.rs` | 270 | `render_template` + `render_conditional_sections` + `join_prompt_sections` 纯函数 |
| `wake_prompt.rs` | 200 | wake payload → prompt markdown 渲染 + task markdown variant 选择 |
| `skills.rs` | 450 | 真实 fs 扫描 + frontmatter 解析 + snapshot 合并 |

## 3. 测试（真实验证）

```
$ cargo test -p pc-adapter-hermes
test result: ok. 76 passed; 0 failed  (lib, +25 from R600)
test result: ok. 1 passed; 0 failed   (adapter_real)
test result: ok. 2 passed; 0 failed   (round600 e2e)
test result: ok. 7 passed; 0 failed   (round601 e2e)
```

合计 **86 个 hermes 测试 0 失败**（R600 末 44 + R601 新增 42）。

## 4. R601 round601 e2e 测试要点

| 测试 | 验证 |
|---|---|
| `prompt_template_full_render_path` | 模板 `{{agent.id}}` + `{{#noTask}}` + `{{#taskTitle}}` 完整组合 |
| `prompt_template_join_with_real_sections` | 空白段被过滤（不出现 `\n\n\n`） |
| `wake_prompt_full_render_with_recovery_contract` | `recovery.cause=process_lost` → prompt 含 `Recovery contract` |
| `wake_prompt_assignment_full_vs_resume_compact` | 4 种组合 (fresh/resume × assignment/non-assignment) 全对 |
| `wake_payload_json_serializes_for_env` | `stringify_wake_payload` → JSON → 反序列化对称 |
| `skills_real_fs_with_runtime_and_hermes` | 真实 fs fixture：`~/.hermes/skills/terminal` + `<runtime>/code-review` |
| `skills_desired_filter_marks_managed_as_configured` | desired skill → `state = Configured`；非 desired → `Available` |

## 5. 关键执行链路（完整 Hermes adapter）

```
adapter_config (JSON)
  ├─ resolve_command       (lib.rs) — command path / hermes CLI
  ├─ cfg_string/model      (lib.rs) — user overrides
  ├─ detect_model          (detect_model.rs) — read ~/.hermes/config.yaml
  ├─ resolve_provider      (resolve_provider.rs) — explicit → detected → inferred → auto
  ├─ build_hermes_command_args (command_args.rs) — chat -q prompt ...
  ├─ prompt rendering      (prompt_template.rs + wake_prompt.rs) — wake/task/template
  ├─ execute_process_capture (pc_adapter_process) — spawn 子进程
  ├─ reclassify_stderr     (command_args.rs) — benign stderr → stdout events
  ├─ parse_hermes_output   (parse_output.rs) — session_id/usage/cost/response
  └─ build_skill_snapshot  (skills.rs) — runtime API for UI

完整模块：8 个（constants / config_schema / detect_model / resolve_provider
                  / command_args / parse_output / prompt_template / wake_prompt / skills）
```

## 6. 设计要点

1. **三个新模块全部纯函数 / async 纯函数**，不依赖 `execute_process_capture`、
   不依赖 `AdapterExecutionContext` — 可独立测试 + 独立复用
2. **真实 fs 测试**：`skills` 测试用 `tokio::fs` 写真实 temp dir，验证
   `~/.hermes/skills/<category>/<skill>/SKILL.md` frontmatter 解析
3. **精简但语义对齐**：wake_prompt 简化为 recovery contract + execution
   contract + 基本字段，省略 Node 端 100+ 行特定 reason 指令
4. **不引入 YAML 依赖**：frontmatter 解析用纯 Rust regex（与 detect_model
   一致 — 避免引入 serde_yaml）
