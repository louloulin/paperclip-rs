# R735 — pc-realtime/src/event_payload_pure.rs

## 目标

补足 Node paperclip/server/src/realtime 中 event / channel / payload 校验
零依赖 pure helpers。

## 新增 helpers (4 个)

| Node 语义 | Rust 函数 |
|---|---|
| event name 校验（trim + 长度 + 字符类） | validate_event_name(name) |
| channel name 校验（trim + 长度 + 控制字符） | validate_channel_name(channel) |
| payload 字节大小校验 | validate_payload_size(payload) |
| channel 是否全局 ("*") | is_global_channel(channel) |
| channels 列表去重 + trim | dedup_channels(channels) |

## 常量

- MAX_EVENT_PAYLOAD_BYTES = 4096
- MIN_EVENT_NAME_LENGTH = 3
- MAX_EVENT_NAME_LENGTH = 128
- MAX_CHANNEL_NAME_LENGTH = 256

## 测试结果

cargo test -p pc-realtime --lib event_payload_pure
test result: ok. 14 passed; 0 failed

## 关键设计

- validate_event_name 严格拒 whitespace / control 字符
- validate_payload_size 用 serde_json 序列化估算字节数
- is_global_channel 只 trim 后严格等于 "*"
- dedup_channels 用 BTreeSet 实现 dedup + trim

## 文件

- 新增：crates/pc-realtime/src/event_payload_pure.rs (5282 bytes)
- 修改：crates/pc-realtime/src/lib.rs (+1 行 pub mod event_payload_pure;)
