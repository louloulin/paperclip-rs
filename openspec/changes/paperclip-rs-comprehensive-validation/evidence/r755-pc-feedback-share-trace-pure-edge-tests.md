# R755 — pc-feedback share / trace pure 边缘补足

## 目标

pc-feedback::share::pure 与 pc-feedback::trace::pure 已覆盖典型路径，但仍缺以下边缘行为：

- `clamp_payload_byte_size` 对 usize::MAX / usize::MAX - 1 的钳制
- `describe_upload_failure` 在 status=0 时的格式
- `validate_backend_url` 处理空白字符与 tab 换行
- `resolve_trace_limit` 在 MAX_TRACE_LIMIT 边界值上的行为
- `validate_company_id` 对真实 uuid 的接受
- `format_trace_hook_label` 输出格式严格匹配 `trace=<uuid> issue=<uuid>`

本轮新增 6 个 r755_ 前缀单测。

## 实现

- share/pure.rs 在 `clamp_payload_byte_size_normal` 后追加 3 个 r755_ 用例
- trace/pure.rs 在 `format_trace_hook_label_includes_both` 后追加 3 个 r755_ 用例

## 验证结果

```
cargo test -p pc-feedback --lib
cargo test: 96 passed (1 suite, 0.01s)
```

## 关键决策

- 本轮全部使用 r755_ 前缀方便回归检索
- 仅在原 internal_tests 模块末尾追加，未改动任何生产函数

## 后续重点

- UI mutation 冒烟：agent / routine / tool / environment
- Adapter 仍按硬约束保持不动
