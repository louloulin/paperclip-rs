# R425 — paperclipEnvNote / apiAccessNote 注入与渲染器去重

## 目标

把 Node `paperclip/packages/adapters/{pi,claude,codex,gemini,cursor,opencode,grok}-local/src/server/execute.ts`
里的 `renderPaperclipEnvNote(env)` / `renderApiAccessNote(env)` 两个提示段统一到
`AdapterExecutionResult.result_json` 的 `paperclipEnvNote` / `apiAccessNote`
字段，与 Node 等价；并把渲染函数下沉到 `pc_acpx::session_config_options`，
统一 7 个 adapter 的依赖入口。

## 主要改动

| 文件 | 改动 |
| --- | --- |
| `crates/pc-acpx/src/session_config_options.rs` | `render_paperclip_env_note` / `render_api_access_note` 改签名接收 `&BTreeMap<String, String>`（与 `env_helpers::has_non_empty_env_value` 风格一致）；不再泄露 API key 明文到 prompt；尾部 `\n\n` 与 Node 行为对齐。 |
| `crates/pc-acpx/src/build_prompt.rs` | 移除 `BTreeMap → HashMap` 拷贝；直接传入 env。 |
| `crates/pc-acpx/tests/round369_path_helpers.rs` / `round380_prompt_compose.rs` | 改用 `BTreeMap`，与新签名对齐。 |
| `crates/pc-adapter-gemini-local/src/execute_helpers.rs` | 删除 `render_paperclip_env_note` / `render_api_access_note` 重复实现，改成 `pub use pc_acpx::session_config_options::{...}`。 |
| `crates/pc-adapter-gemini-local/tests/round420_gemini_execute_helpers.rs` | 同步移除本地渲染测试（已下沉到 pc_acpx 单测）。 |
| `crates/pc-adapter-{claude,codex,gemini,cursor,opencode,pi,grok}-local/src/lib.rs` | 在 `result_json` 中新增 `paperclipEnvNote` 与 `apiAccessNote` 字段；cursor-local 同步合并双重 `result_json` 写入。 |

## Node 等价性

- `renderPaperclipEnvNote(env)`：列 `PAPERCLIP_*` 变量名，按字母排序，无值，
  空输入返空串。Rust 实现完全等价于 Node 版本（`packages/adapter-utils/src/acpx-engine/execute.ts` L2213）。
- `renderApiAccessNote(env)`：要求 `PAPERCLIP_API_URL` 与 `PAPERCLIP_API_KEY` 同时非空，
  输出 curl 示例。Rust 实现把 API key 替换为 `$PAPERCLIP_API_KEY` 引用，
  避免敏感信息进入 prompt（这是 R425 期间发现的回归点，已通过单测锁定）。

## 测试矩阵

| 测试 | 位置 |
| --- | --- |
| `session_config_options::tests::*` (8 项) | `crates/pc-acpx/src/session_config_options.rs` |
| `round369_path_helpers::render_*` (2 项) | `crates/pc-acpx/tests/round369_path_helpers.rs` |
| `round380_prompt_compose::*` | `crates/pc-acpx/tests/round380_prompt_compose.rs` |
| `round425_{adapter}_prompt_injection::result_json_carries_prompt_notes` ×7 | 各 adapter crate `tests/round425_*.rs` |

每个集成测试都会构造一个 `/bin/sh` 脚本 mock CLI，触发对应 adapter 的
`Adapter::execute`，断言 `result_json["paperclipEnvNote"]` 与
`result_json["apiAccessNote"]` 在不同 env 配置下的行为与 Node 等价。

## 验证

- `cargo check --workspace --tests`：0 errors / 391 warnings（warning 均为预先存在的非蛇形测试名）。
- `cargo test -p pc-acpx`：1442 passed / 1 ignored。
- `cargo test --workspace`：除 `pc-agent::agent_actor_integration::supervisor_serializes_concurrent_agent_mutations` 因测试 DB 的 `companies_issue_prefix_idx` 唯一冲突未通过（与本次改动无关，需要清理 DB 后重跑），其余全部通过。
- 7 个 local adapter 的 R425 集成测试全部通过。

## 后续

- R426 进入 pi-local execute 完整 retry loop 复刻。
- 后续 R427+ 复刻 claude / codex / 其他 adapter 的 retry / 错误恢复主循环。
