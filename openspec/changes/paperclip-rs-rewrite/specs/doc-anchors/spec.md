## ADDED Requirements

### Requirement: 文档锚点与批注模型 SHALL
The system SHALL satisfy the following behavior.

`pc-doc-anchors` 实现文档锚点解析与批注存储（与 `shared/document-anchors.ts` 等价）。

#### Scenario: 创建锚点
- **WHEN** 客户端 `POST /api/.../anchors`
- **THEN** 写入 `document_annotation_anchor_snapshots` 并返回 201
