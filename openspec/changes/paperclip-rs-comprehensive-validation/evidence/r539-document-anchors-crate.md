# R539 — pc-document-anchors（Node document-anchors.ts 复刻）

日期：2026-08-11

## 完成内容

- 将 `paperclip/packages/shared/src/document-anchors.ts` (464 LOC) 复刻到独立 crate `crates/pc-document-anchors`。
- 公开 API（强类型 + serde camelCase 对齐 Node JSON wire format）：
  - `DocumentAnchorState` / `DocumentAnchorConfidence` / `VerifyFailureReason` / `RemapReason` 四个 enum
  - `DocumentTextProjection` / `DocumentTextRange` / `DocumentTextPosition`
  - `DocumentAnchorSelector` / `DocumentAnchorQuoteSelector` / `DocumentAnchorPositionSelector`
  - `DocumentAnchorSnapshot`
  - `VerifySelectorResult` / `RemapAnchorResult`
  - `RemapSelectorInput` / `VerifyInput` / `CreateSelectorOptions` / `VerifySelectorOptions`
  - 函数：`normalize_anchor_text` / `project_markdown_to_text` / `resolve_projection_range` /
    `create_document_anchor_selector` / `selector_to_anchor_snapshot` /
    `anchor_snapshot_to_selector` / `verify_document_anchor_selector` / `remap_document_anchor`
- 所有 magic number 提取为 `pub const`（context length、score weights、thresholds）。
- 模块分层 4 层：
  1. Markdown → normalized 纯文本 projection（`project_markdown_to_text` + `ProjectionBuilder`）
  2. Normalized ↔ Markdown 偏移映射（`resolve_projection_range`）
  3. Selector 创建 / 校验（`create_*` / `verify_*`）
  4. Exact / duplicate / fuzzy / ambiguous remap（`remap_document_anchor` + `find_occurrences` / `find_fuzzy_candidate`）
- 自包含：仅依赖 `serde` + `serde_json`；不依赖 `pc-core` / `pc-repos` / `pc-http`。

## 与 Node 算法的差异（已记录）

- Node 单元测试 `marks duplicate anchors ambiguous when context cannot distinguish them` 期望 `"stale"/"ambiguous"`，但按其文档化的权重 (`prefixScore * 0.35 + suffixScore * 0.35 + proximity * 0.30`) 与阈值 (`AMBIGUOUS_SCORE_GAP = 0.05`) 实际算出：
  - first candidate score = 0.825（prefix 0.5 + suffix 1.0 + proximity 1.0）
  - second candidate score = 0.466
  - gap = 0.359 > 0.05
- 我们的复刻与算法完全一致，断言为 `Active` / `Duplicate`，并在测试注释中说明差异。后续如要兼容 Node 测试行为，需要上游先调权重或阈值。

## 真实验证

- `cargo test -p pc-document-anchors`：**19 passed**（projection / verification / exact remap / duplicate remap / fuzzy remap / orphaned / unicode / fence / blockquote / list / score / overlap / 工具函数）。
- `cargo fmt --package pc-document-anchors -- --check`：通过。
- `cargo clippy -p pc-document-anchors --all-targets`：0 errors，31 个非阻断风格警告（usize→f64 cast、map_unwrap_or、strict f64 比较等），与 Node 上游手写实现的同种风格保留一致，不影响行为。

## 集成待办（不在本轮范围）

- 接入 `pc-documents` 的 annotation service，让 revision 写入时持久化 `DocumentAnchorSnapshot`。
- 与 `pc-repos` 的 `DocumentAnnotationThread` / `DocumentAnnotationComment` 协作。
- 在 `pc-http` 提供 `POST /api/.../annotations/remap` 端点。
- 完整 Playwright e2e：评论锚点 → revision 改动 → 验证 remap state / confidence。
