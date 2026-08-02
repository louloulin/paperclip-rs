## ADDED Requirements

### Requirement: SecretsProvider trait + local-encrypted + aws-sm SHALL
The system SHALL satisfy the following behavior.

`pc-secrets` 提供 `SecretsProvider` trait；`local_encrypted`（AES-GCM，主密钥来自 `PAPERCLIP_MASTER_KEY`）与 `aws_sm` 两个实现。

#### Scenario: 存读密钥
- **WHEN** `provider.put("api_key", secret)` 然后 `provider.get("api_key")`
- **THEN** 返回原始 secret
