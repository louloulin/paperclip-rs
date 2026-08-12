# R636 — middleware 补齐 batch 2 (validate / board-mutation-guard / error-handler 映射)

## Status

DONE — 三个 middleware + 错误映射扩展对齐 Node `middleware/error-handler.ts`
全部分支。pc-http 全量 473 测试绿；pc-server 装配编译通过。

## Files added / modified

| Path | Status | Notes |
|---|---|---|
| crates/pc-http/src/middleware/validate.rs | new (~75 LOC) | `serde_path_to_error` 校验器 + Node 形状 zod details |
| crates/pc-http/src/middleware/board_mutation_guard.rs | new (228 LOC) | board actor origin/referer 守卫 + trusted-origin 解析 |
| crates/pc-http/src/middleware/mod.rs | modified | 注册 + 重新导出 |
| crates/pc-http/Cargo.toml | modified | + serde_path_to_error |
| crates/pc-http/src/error.rs | rewrite (~290 LOC) | Node `error-handler.ts` 全部分支 + Zod 形状 |
| apps/pc-server/src/main.rs | modified | 注册 board_mutation_guard_layer（auth 之后） |
