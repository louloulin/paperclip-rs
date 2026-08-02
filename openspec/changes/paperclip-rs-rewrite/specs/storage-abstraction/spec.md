## ADDED Requirements

### Requirement: StorageProvider trait + local-disk + s3 SHALL
The system SHALL satisfy the following behavior.

`pc-storage` 暴露 `StorageProvider` trait；`local_disk` 与 `s3` 两个实现通过配置选择。

#### Scenario: 上传文件
- **WHEN** `provider.put(key, bytes)`
- **THEN** 返回 `Ok(())` 并可通过 `provider.get(key)` 取回
