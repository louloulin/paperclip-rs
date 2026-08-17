# R763 + R764 — pc-tool policy/risk + pc-routines webhook/cwd 集成测试（+16 PASS）

## 目标

补充 pc-tool::policy_validation / pc-tool::risk / pc-routines::webhook_signature_pure / pc-routines::session_cwd 四个模块的纯函数边缘测试。

## R763 — pc-tool::policy_validation + pc-tool::risk（+9 PASS）

### policy_validation（+5 PASS）

| 测试 | 验证 |
|---|---|
| r763_trust_rule_active_no_revoke_no_expire | TrustRuleConfig 无 revoked + 无 expires → active |
| r763_trust_rule_revoked_inactive | revoked_at set → inactive (即使未过期) |
| r763_trust_rule_expired_inactive | expires_at <= now → inactive |
| r763_trust_rule_not_yet_expired_active | expires_at > now → active |
| r763_rate_limit_rule_extract | 合法 {limit, windowSeconds} / 缺字段 / 顶层 fallback / limit=0 |

### risk（+4 PASS）

| 测试 | 验证 |
|---|---|
| r763_verb_matches_case_insensitive_multi_verb | 大小写不敏感 + 多 verb 任一匹配 + 空 pattern/verb 跳过 |
| r763_classify_risk_read_only_hint | read_only_hint=true → Read |
| r763_classify_risk_write_hint | write_hint=true → Write (即使 name 无 write verb) |
| r763_classify_risk_destructive_verb | destructive verb in name → Destructive |

## R764 — pc-routines::webhook_signature_pure + session_cwd（+7 PASS）

### webhook_signature_pure（+5 PASS）

| 测试 | 验证 |
|---|---|
| r764_parse_webhook_signature_header_valid | "t=<ts>,v1=<sig>" → Some |
| r764_parse_webhook_signature_header_invalid_prefix | 缺 v1= / 非数字 ts → None |
| r764_verify_webhook_signature_pure_valid | 正确签名 → Ok |
| r764_verify_webhook_signature_pure_replay_window | delta > replay_window_sec * 1000 → ReplayWindowExceeded |
| r764_verify_webhook_signature_pure_mismatch | 错误签名 → SignatureMismatch |

### session_cwd（+2 PASS）

| 测试 | 验证 |
|---|---|
| r764_normalize_cwd_trailing_slash | 尾 / 去掉 / 根 / 保留 / 空 → / |
| r764_normalize_cwd_dotdot_folding | .. / . 折叠 |

## 验证

```
cargo test -p pc-tool r763
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 215 filtered out

cargo test -p pc-routines r764
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 193 filtered out

cargo test -p pc-tool --lib
test result: ok. 224 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

cargo test -p pc-routines --lib
test result: ok. 200 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 累计

| crate | PASS | 增量 |
|---|---:|---:|
| pc-tool | 224 | +9 |
| pc-routines | 200 | +7 |
| **R763+R764 合计** | **+16** | |

## 整体进度

| crate | PASS |
|---|---:|
| pc-issues | 183 |
| pc-routines | 200 |
| pc-heartbeat | 662 |
| pc-tool | 224 |
| pc-decisions | 179 |
| pc-repos | 650 |
| **合计** | **2098** |

## R765+ 后续计划

- R765 — pc-issues / 其他模块剩余边缘测试
- Adapter 仍按硬约束保持不动
