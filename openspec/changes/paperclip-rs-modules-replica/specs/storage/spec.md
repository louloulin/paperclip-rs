# storage (M3)

`pc-storage` 的 `StorageProvider` trait + `local_disk` + `s3` 真实 SDK 接入。

## ADDED Requirements

### Requirement: REQ-M3-1 StorageProvider Trait
必须存在统一 `StorageProvider` trait，提供 put/get/head/delete/list/presign_get 六个方法，且各实现必须满足 trait object safety。


The system SHALL satisfy this requirement.
#### Scenario: trait 抽象存在
- GIVEN 当前 src
- WHEN `cargo doc -p pc-storage`
- THEN `StorageProvider` 出现在公共 API 中

### Requirement: REQ-M3-2 Local Disk Real Implementation
`local_disk` provider 必须真实走 tokio fs（不依赖内存模拟），支持原子写入、SHA-256 元数据、目录列表、presigned token（HMAC）。


The system SHALL satisfy this requirement.
#### Scenario: 本地 put/get
- GIVEN 一个本地目录作为 backend
- WHEN 调用 put + get
- THEN 数据往返一致，SHA-256 元数据正确

### Requirement: REQ-M3-3 S3 Real SDK
`s3` provider 必须接入 `aws-sdk-s3` 真实 SDK，支持 multipart（≥8MB 触发）、presigned URL、retry。


The system SHALL satisfy this requirement.
#### Scenario: localstack 通过
- GIVEN localstack 起的 S3 容器
- WHEN 调用 put/get/presign
- THEN 数据与 URL 在本地与 SDK 间一致

### Requirement: REQ-M3-4 Provider Registry
按 `STORAGE_KIND` env 自动选用 provider；fallback 路径明确。


The system SHALL satisfy this requirement.
#### Scenario: kind=local
- GIVEN `STORAGE_KIND=local`
- WHEN `pc-server` 启动
- THEN 真实使用 local_disk
