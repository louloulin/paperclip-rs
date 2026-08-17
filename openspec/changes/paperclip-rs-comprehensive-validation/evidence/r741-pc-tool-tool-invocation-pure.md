# R741 -- pc-tool/src/tool_invocation_pure.rs

## 目标

补足 Node paperclip/server/src/services/tool-access.ts 中零依赖 pure utility helpers（normalizeKey / connectionUid / numberValue / percent / percentile / oauthActorType / actorBinding）。

## 新增 helpers (7 个)

| Node 函数 | Rust 函数 |
|---|---|
| normalizeKey(input) | normalize_key(input) |
| connectionUid(ns, name, id) | connection_uid(ns, name, id) |
| numberValue(value) | number_value(value) |
| percent(num, den) | percent(num, den) |
| percentile(values, p) | percentile(values, p) |
| oauthActorType(value) | oauth_actor_type(value) + ActorType enum |
| actorBinding(actor) | actor_binding(actor_type, actor_id, session_id) + ActorBinding struct |

## 测试结果

cargo test -p pc-tool --lib tool_invocation_pure
test result: ok. 21 passed; 0 failed

## 关键设计

- normalize_key 按 char-by-char 扫描避免 regex 依赖（Node 用 .replace(/[^...]+/g, "-")）
- percent 保留 1 位小数 (x * 10).round() / 10
- percentile 用 ceil((p/100) * n) - 1 然后 clamp
- ActorType enum 强类型化 OAuth actor type
- ActorBinding 集中处理 trim + empty check

## 文件

- 新增：crates/pc-tool/src/tool_invocation_pure.rs (7508 bytes)
- 修改：crates/pc-tool/src/lib.rs (+1 行 pub mod tool_invocation_pure)
