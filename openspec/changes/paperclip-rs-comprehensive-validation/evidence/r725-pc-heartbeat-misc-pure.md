# R725 — pc-heartbeat/src/misc_pure.rs

## 目标

补足 Node paperclip/server/src/services/heartbeat.ts 中零 DB pure helpers，专注 transient fallback / 错误家族分类 / env binding 验证 / recovery metadata merge。

## 新增 helpers（12 个）

- CodexTransientFallbackMode (enum NativeResume/FreshSpawn/Disabled)
- resolve_codex_transient_fallback_mode(attempt)
- is_max_turn_exhaustion_run(result_json)
- is_spawn_like_failure_message(value)
- is_resolved_interaction_continuation_wake_context(snapshot)
- is_configured_env_binding_value(binding)
- has_github_pr_workflow_skill(desired_skills)
- merge_adapter_recovery_metadata(base, recovery)
- strip_forbidden_env_bindings(env_value, allowed_keys)
- strip_forbidden_env_from_adapter_config(config, allowed_keys)

## 测试结果

cargo test -p pc-heartbeat --lib misc_pure
running 13 tests
... 全部 PASS
test result: ok. 13 passed; 0 failed

## 关键设计

- CodexTransientFallbackMode 三态枚举 + u32 attempt 匹配表精确对齐 Node 逻辑
- is_max_turn_exhaustion_run 通过 code=max_turns_exceeded 或 reason 子串检测
- merge_adapter_recovery_metadata 严格按 Node 优先级：base 优先 + recovery 覆盖
- strip_forbidden_env_bindings 仅在 object 类型上操作
- 所有 helper 零 IO、零 DB、纯函数

## 文件

- 新增：crates/pc-heartbeat/src/misc_pure.rs (242 lines)
- 修改：crates/pc-heartbeat/src/lib.rs (+1 行 pub mod misc_pure;)
