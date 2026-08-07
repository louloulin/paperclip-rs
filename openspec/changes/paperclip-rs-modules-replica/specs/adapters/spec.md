# adapters (M13)

`pc-adapter-api` + 11 个适配器 crate。

## ADDED Requirements

### Requirement: REQ-M13-1 AdapterRuntime Trait
统一 trait，10 个内置适配器全部实现。


The system SHALL satisfy this requirement.
#### Scenario: 全部实现
- GIVEN `pc-adapter-api`
- WHEN `cargo build --workspace`
- THEN 11 个 adapter crate 全部实现 + 编译通过

### Requirement: REQ-M13-2 Size 与 Node 对齐
| Adapter | Node 行数 | Rust 目标 |
|---|---|---|
| claude-local | 7114 | ≥ 8k |
| codex-local | 13520 | ≥ 10k |
| cursor-local | 3579 | ≥ 3k |
| cursor-cloud | 1799 | ≥ 1.5k |
| gemini-local | 4388 | ≥ 4k |
| grok-local | 1984 | ≥ 1.5k |
| opencode-local | 3470 | ≥ 3k |
| pi-local | 3580 | ≥ 3k |
| hermes | 5406 | ≥ 4k |
| hermes-gateway | 18 | ≥ 完整 |
| openclaw-gateway | 2207 | ≥ 2k |


The system SHALL satisfy this requirement.
#### Scenario: 行数达标
- GIVEN 11 个 crate
- WHEN `find crates/pc-adapter-* -name "*.rs" | xargs wc -l`
- THEN 全部达标

### Requirement: REQ-M13-3 每 crate 集成测试
每 crate ≥ 1 happy + ≥ 1 failure 测试。


The system SHALL satisfy this requirement.
#### Scenario: 全测通过
- GIVEN 11 个 crate
- WHEN `cargo test --workspace`
- THEN adapter-* 全绿
