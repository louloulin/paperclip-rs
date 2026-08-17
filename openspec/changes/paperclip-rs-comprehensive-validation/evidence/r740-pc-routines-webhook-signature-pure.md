# R740 -- pc-routines/src/webhook_signature_pure.rs

## 目标

补足 Node paperclip/server/src/services/routines.ts::verifyWebhookSignature 中
签名验证零 DB 逻辑（HMAC-SHA256 + timestamp replay window + hex decode + constant-time compare）。

## 新增 helpers（8 个）

| Node 语义 | Rust 函数 |
|---|---|
| HMAC-SHA256 hex | hmac_sha256_hex(key, payload) |
| constant-time bytes equality | constant_time_eq(a, b) |
| hex string to bytes | hex_decode(s) |
| single hex char to nibble | hex_nibble(b) |
| parse webhook signature header | parse_webhook_signature_header(header) |
| 完整 webhook 签名校验 (pure) | verify_webhook_signature_pure(...) |
| 错误枚举 | WebhookSignatureError enum |

## 测试结果

cargo test -p pc-routines --lib webhook_signature_pure
test result: ok. 19 passed; 0 failed

## 关键设计

- 使用 hmac + sha2 + subtle crates（已在 pc-routines/Cargo.toml）
- hmac_sha256_hex 用 RFC 4231 test case 1 验证
- payload 拼接 = ts_str + 点 + body（与 Node HMAC over "t.body" 一致）
- constant_time_eq 用 subtle::ConstantTimeEq 防侧信道

## 文件

- 新增：crates/pc-routines/src/webhook_signature_pure.rs (8455 bytes)
- 修改：crates/pc-routines/src/lib.rs (+1 行)
