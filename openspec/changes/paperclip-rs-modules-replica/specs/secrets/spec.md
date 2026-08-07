# secrets (M4)

`pc-secrets` AES-256-GCM 加解密 + aws-secrets-manager + provider 链。

## ADDED Requirements

### Requirement: REQ-M4-1 SecretsProvider Trait
统一 trait，提供 get/put/list。所有 provider 实现共同遵守。


The system SHALL satisfy this requirement.
#### Scenario: trait 抽象
- GIVEN 当前 src
- WHEN `cargo doc -p pc-secrets`
- THEN `SecretsProvider` 与 `Secret = Zeroizing<Vec<u8>>` 公开

### Requirement: REQ-M4-2 AES-256-GCM Local
`local_encrypted` 必须用真实 AES-256-GCM（不简化）+ 随机 nonce + 版本号 salt。


The system SHALL satisfy this requirement.
#### Scenario: 加解密 roundtrip
- GIVEN 主密钥来自 `PC_MASTER_KEY`（base64 32-byte）
- WHEN put → 重启 → get
- THEN 数据原样返回，nonce 不可重用

### Requirement: REQ-M4-3 AWS Secrets Manager
`aws_sm` 接入 `aws-sdk-secretsmanager` 真实 SDK，可选 KMS。


The system SHALL satisfy this requirement.
#### Scenario: localstack SM 通过
- GIVEN localstack
- WHEN put/get/list
- THEN 与 Node 端同等行为

### Requirement: REQ-M4-4 Provider Chain
`configured_provider` 支持多 provider fallback / 灰度切换。


The system SHALL satisfy this requirement.
#### Scenario: provider 链
- GIVEN primary=local_encrypted, fallback=aws_sm
- WHEN primary 不可用
- THEN 自动 fallback

### Requirement: REQ-M4-5 Secret Hygiene
密钥永不进入日志或错误响应。


The system SHALL satisfy this requirement.
#### Scenario: 日志无密钥
- GIVEN 故意触发错误
- WHEN 检查 tracing JSON 输出
- THEN 0 命中 secret 值
