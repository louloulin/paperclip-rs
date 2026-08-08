# R424：pi-local model_id 委托到 pc_acpx 收尾

## 目标

R417 在 `pc-adapter-pi-local` 中首次实现 `model_provider` / `model_id` helper。
R422 在 `pc_acpx::model_id` 中抽出通用版本（与 opencode-local 共享）。
本轮把 pi-local 的本地实现改为 re-export 向 pc_acpx 委托，消除重复。

## 实施

### pi-local `execute_helpers.rs` 简化

替换前（`pi-local::execute_helpers` 内的完整实现）：

```rust
pub fn model_provider(model: Option<&str>) -> Option<String> {
    let trimmed = model?.trim();
    if !trimmed.contains('/') {
        return None;
    }
    let idx = trimmed.find('/').unwrap();
    let prefix = trimmed[..idx].trim();
    if prefix.is_empty() {
        None
    } else {
        Some(prefix.to_owned())
    }
}
```

替换后（thin wrapper 委托 pc_acpx）：

```rust
pub fn model_provider(model: Option<&str>) -> Option<String> {
    pc_acpx::model_id::parse_model_provider(model)
}
```

`model_id` 同样改造。

## 验证

- `cargo test -p pc-adapter-pi-local`：全量 127 passed（55 lib + 1 round395 + 10 adapter_real + 28 round416 + 33 round417，**无回归**）。
- `pc_acpx::model_id` 仍是权威实现，10 个单测覆盖所有边界。
- 7 个 adapter 全包测试通过。

## 关键设计决策

- **保留 `pub fn model_provider` / `model_id` API**：调用方（pi-local lib.rs execute）仍可通过 `crate::execute_helpers::model_provider` 调用，零改动。
- **thin wrapper over pc_acpx**：未来 pc_acpx::model_id 增强（如支持 array 形式、多 token 拆分等）时，pi-local 自动受益。
- **不直接 `pub use pc_acpx::model_id::*`**：保持 pi-local 命名空间语义清晰（`pc_adapter_pi_local::model_provider` 描述 pi-local 语义）。

## 兼容性

- `pc_adapter_pi_local::model_provider` / `model_id` 仍是 `pub fn`，签名一致。
- 行为完全一致（pc_acpx::model_id 实现就是为此 parity）。
- 旧 fixture 与集成测试不破坏。

## 后续收尾

- gemini-local `render_paperclip_env_note` 仍是自己实现（与 pc_acpx 略有差异——双 `\n\n` 结尾），可在 R425 收尾。
- cursor-local / opencode-local / pi-local 都已消费 `pc_acpx::model_id`。
- 7 个 local adapter 的 Execute helpers parity 已覆盖：billing / biller / model provider / skills home / session helpers / mode normalization。

## 文件清单

- 修改 `crates/pc-adapter-pi-local/src/execute_helpers.rs`（`model_provider` / `model_id` 改为 thin wrapper over `pc_acpx::model_id`）。
