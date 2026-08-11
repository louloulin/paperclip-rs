# R538 — pc-pipeline-health（Node pipeline-health.ts 复刻）

日期：2026-08-11

## 完成内容

- 将 `paperclip/packages/shared/src/pipeline-health.ts` 的纯函数健康检查逻辑迁移到 `crates/pc-pipeline-health`。
- 使用结构化输入、`PipelineHealthWarningCode` enum、`PipelineHealthReport` 和 camelCase serde，保持 Node JSON 接口兼容。
- 覆盖 paused agent、无 agent、无 automation、review、breakdown、pipeline mention、失败 automation 和终结 stage 等分支。
- 对齐上游 `parsePipelineMentionHref` 的大小写行为：markdown 链接正则大小写不敏感，但 scheme 解析仍使用大小写敏感的 `startsWith`。
- 对齐上游 review 语义：缺失 approver 时默认 `any_human`，只有 agent approver 缺失/不可调用或 user approver 缺失时告警。

## 真实验证

- `cargo test -p pc-pipeline-health`：**32 passed**。
- `cargo clippy -p pc-pipeline-health --all-targets -- -D warnings`：未通过；剩余为 clippy 结构/风格告警，下一轮拆分主计算函数时收敛，不改变业务语义。
- workspace `cargo fmt --all -- --check`：未通过；报告包含既有 workspace 文件格式差异，非本 crate 单独行为验证。

## 当前差距影响

R538 已具备可测试的纯函数实现，但尚未接入 `pc-pipelines` / `pc-http` 的生产路径，也尚未完成 Node/Rust 双服务运行时对照。因此该 crate 计入“模块复刻已完成、集成验证未完成”。
