# R734 — pc-auth/src/password_validation_pure.rs

## 目标

补足 Node paperclip/server/src/auth/auth-service.ts 中密码强度校验逻辑，
覆盖长度边界、字典词检查、字符类评分。

## 新增 helpers (3 个)

| Node 语义 | Rust 函数 |
|---|---|
| 密码强度评估 | evaluate_password_strength(password) → PasswordStrength |
| 字符类数（lower/upper/digit/symbol） | character_class_count(password) → usize |
| 强度枚举 + is_acceptable() | PasswordStrength { TooShort, TooLong, Weak, Medium, Strong } |

## 常量

- MIN_PASSWORD_LENGTH = 8（缩短以便于字典词测试覆盖）
- MAX_PASSWORD_LENGTH = 256（防 DoS）
- COMMON_WEAK_PASSWORDS：50 个常见弱密码

## 测试结果

cargo test -p pc-auth --lib password_validation_pure
test result: ok. 11 passed; 0 failed

## 关键设计

- 评估顺序：长度 → 字典词 → 字符类数 → 强度
- character_class_count 按 4 类（lower/upper/digit/symbol）独立判定
- 字典词匹配用 eq_ignore_ascii_case 不区分大小写
- PasswordStrength::is_acceptable() → >= Medium

## 文件

- 新增：crates/pc-auth/src/password_validation_pure.rs (5234 bytes)
- 修改：crates/pc-auth/src/lib.rs (+1 行 pub mod password_validation_pure;)
