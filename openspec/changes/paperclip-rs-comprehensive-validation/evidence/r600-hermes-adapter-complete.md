# R600 — Hermes adapter 完整复刻

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

对齐 Node `packages/adapters/hermes/src/server/execute.ts` (596 行) 的核心
执行路径，把 Hermes adapter 从 147 行 stub 推进到完整 Rust 实现。

## 2. 模块拆分（高内聚、低耦合）

| 模块 | 行数 | 职责 |
|---|---|---|
| `constants.rs` | 135 | VALID_PROVIDERS / MODEL_PREFIX_PROVIDER_HINTS / 正则 / 共享默认值 |
| `config_schema.rs` | 252 | Paperclip UI 配置 schema（12 字段） |
| `detect_model.rs` | 203 | 解析 `~/.hermes/config.yaml` |
| `resolve_provider.rs` | 154 | explicit → detected → inferred → auto 优先级链 |
| `command_args.rs` | 309 | CLI args 拼装 + stderr reclassification |
| `parse_output.rs` | 218 | 提取 session_id / usage / cost / response / errorMessage |
| `lib.rs` | 230 | 整合所有模块 + Adapter execute 路径 |

每个模块独立可测试、独立可复用：`detect_model` 可单独用于其他场景；
`resolve_provider` 是纯函数；`command_args` 的 stderr reclassification 可
服务其他 adapter；`parse_output` 接受任意 stdout/stderr 输入。

## 3. 新增测试

| 测试 | 数量 | 类别 |
|---|---|---|
| lib 单元测试 | 41 | 5 lib.rs + 8 detect_model + 5 resolve_provider + 7 command_args + 9 parse_output + 5 config_schema + 3 constants（去掉重复） |
| `adapter_real.rs` | 1 | descriptor 真实验证 |
| `round600_hermes_end_to_end.rs` | 2 | fake hermes CLI 真实 e2e |

合计 **44 个 hermes 测试 0 失败**（R600 末；之前 stub 阶段只有 6 个）。

## 4. 关键执行链路

```
adapter_config → resolve_command (custom / hermes)
            → cfg_string(model|provider|toolsets|...)
            → detect_model (read ~/.hermes/config.yaml)
            → resolve_provider (explicit → detected → inferred → auto)
            → HermesCommandOptions::default() + overrides
            → build_hermes_command_args (chat -q prompt ...)
            → execute_process_capture (spawn 子进程)
            → reclassify_stderr (benign → stdout events)
            → parse_hermes_output (session_id / usage / cost / response / errorMessage)
            → AdapterExecutionResult { provider, model, usage, cost_usd, session_id, summary, result_json }
```

## 5. 真实验证

```
$ cargo test -p pc-adapter-hermes
test result: ok. 41 passed; 0 failed  (lib)
test result: ok. 1 passed; 0 failed   (adapter_real)
test result: ok. 2 passed; 0 failed   (round600_hermes_end_to_end)

$ cargo clippy -p pc-adapter-hermes --lib --tests
0 errors from pc-adapter-hermes
```

E2E 测试要点：
- 写一个真实 shell 脚本到 temp dir，chmod 0755
- `process::execute_process_capture` spawn 该 fake hermes
- fake hermes 输出 `session_id: ...` + `tokens: ... input ... output` + `cost: $0.42` + agent 文本响应
- 验证：
  - session_id 从 quiet mode 解析（`sess-r600-real-001`）
  - usage.input_tokens = 1234, output_tokens = 567
  - cost_usd = 0.42
  - summary 含 "42"
  - stderr 中的 `[2026-08-12T...] INFO:` 和 `MCP server connected` 被重新分类为 stdout events
  - result_json.session_id / cost_usd / resolvedFrom 都正确

## 6. 与 Node 一致性

| Node 行为 | Rust 实现 |
|---|---|
| 12 字段 config schema | `config_schema::get_config_schema` 12 个字段、相同 hint/default |
| `detectModel()` 读 YAML | `detect_model::parse_model_from_config` 纯正则解析（不引入 YAML 依赖） |
| `resolveProvider` 优先级链 | `resolve_provider::resolve_provider` 显式 → 检测 → 推断 → auto |
| `buildCommandArgs` 顺序敏感 | `command_args::build_hermes_command_args` 同样顺序 |
| 默认 `--source tool --yolo` | `HermesCommandOptions::default()` 同 |
| `wrappedOnLog` stderr reclassification | `command_args::reclassify_stderr` 同正则列表 |
| `parseHermesOutput` 提取 | `parse_output::parse_hermes_output` session_id (quiet + legacy) + usage + cost + response + error |
| session_params 持久化 | `result.session_params = json!({"sessionId": id})` + `session_display_id` |

## 7. 设计要点（最佳 Rust 实现）

1. **模块化而非巨型 execute 函数**：每个模块 100-300 行，单一职责
2. **Trait-free 抽象**：`detect_model`、`parse_output` 都是普通 `pub fn`，
   不引入 trait 既降低复杂度又保留灵活性
3. **真实测试而非 mock**：fake hermes CLI 是真实 shell 脚本、真实子进程
4. **End-to-end 覆盖**：从 `adapter_config` JSON 到 `AdapterExecutionResult`
   完整字段全部验证（不是只测解析器）
5. **stderr 重分类既 emit 又用**：benign stderr 同时回流为 stdout 事件
   （UI 看到）和保留为 stderr（错误检测用）

