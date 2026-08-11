# R540 — pc-frontmatter（Node frontmatter.ts 复刻）

日期：2026-08-11

## 完成内容

- 将 `paperclip/packages/shared/src/frontmatter.ts` (644 LOC) 复刻到独立 crate `crates/pc-frontmatter`。
- 公开 API（强类型 + serde camelCase 对齐 Node JSON wire format）：
  - 类型守卫：`is_plain_record` / `as_string` / `as_boolean` / `as_string_array`
  - 块拆分：`split_frontmatter_block` / `join_frontmatter_block`（byte-exact 互逆）
  - 解析：`parse_frontmatter_fields` / `parse_frontmatter_markdown`（含 CRLF 归一化、空白 / 注释行容忍）
  - 序列化：`stringify_frontmatter`（含块标量 `|` / `>` + chomping `+` / `-`、嵌套对象、数组、空值 `[]` / `{}`、YAML key 校验）
  - Round-trip：`analyze_frontmatter_block` / `detect_frontmatter_round_trip_issues`
  - Skill 工具：`get_skill_frontmatter_unknown_keys`（zod schema 暂不内嵌）
  - 错误类型：`FrontmatterSerializeError`（`NonFiniteNumber` / `UnsupportedValueType` / `UnsupportedKey`）
- 手写 YAML 解析器（零外部 YAML 依赖）：
  - 缩进块（`record` + `array`）
  - 块标量（literal `|` / folded `>`）+ chomping `+` / `-`
  - 数组项作为行内对象（如 `  - kind: github-dir`）
  - 数字 / 布尔 / null / inline JSON 数组 / 对象
  - 行尾浮点 `"1."` 不被误识别为数字（与 Node 一致）
- 手写 YAML 序列化器：递归 record/array，块标量检测（字符串含 `\n`），empty `[]` / `{}` 短路，YAML 标量需要是 `is_plain_yaml_scalar` 才直接输出，否则 JSON 包裹。
- 零外部 YAML 依赖：仅 `serde_json` (with `preserve_order` feature) + `serde` + `regex-lite` + `once_cell` + `thiserror`。
- 自包含：仅依赖上述几个 crate；不依赖 `pc-core` / `pc-repos` / `pc-http`。

## 与 Node 上游的差异（已记录）

- `analyzeFrontmatterBlock` 的 "simple round-trippable" 测试要求 `stringify(parsed) == raw`。Node 因为 JS object 的 key 插入序保留，所以序列化输出 key 顺序与原文一致。Rust 侧 `serde_json::Map` 默认用 `BTreeMap`（按 key 排序）。本 crate 启用 `preserve_order` feature 让 `Map` 用 `IndexMap` 行为，与 Node 行为一致。
- `FrontmatterSerializeError::NonFiniteNumber`：serde_json::Value 本身无法表示非有限数（`Number::from_f64(NaN)` 返回 None），所以该错误分支从正常 API 不可达。测试改为验证 error 变体 + finite 正常路径。
- `zod`-based `skillFrontmatterSchema` 暂未内嵌（依赖 zod 在 Rust 中没有对应物；如需严格等价 schema，需要在调用方业务 crate 中用 `validator` / `garde` / 自写校验实现）。

## 真实验证

- `cargo test -p pc-frontmatter`：**32 passed**（type guards / split / parse / fold / literal / chomping / inline object array / CRLF / serialize / nested / arrays / block scalar / empty / round-trip stable / issue detection / analyze simple+list+metadata+comment+folded+empty / skill unknowns / error variants / YAML scalar）。
- `cargo fmt --package pc-frontmatter -- --check`：通过。
- `cargo clippy -p pc-frontmatter --all-targets`：0 errors，4 个非阻断风格警告（LazyLock 迁移、unwrap_or、inclusive range），与本仓库其他小型 pure-function crate 风格一致。

## 集成待办（不在本轮范围）

- 接入 `pc-documents` 文档服务：revision 保存时用 `parse_frontmatter_markdown` 拆分 → `analyze_frontmatter_block` 决定 UI 模式。
- 接入 `pc-skills` / `pc-skills-catalog`：使用 `parse_frontmatter_fields` + skill slug regex 校验 `name` 字段。
- 完整 Playwright e2e：Skill Studio FrontmatterPanel fields-mode ↔ raw-YAML 切换。
- Node 端 `bundled/skills-catalog/catalog/**/*.md` 全量 byte-identity round-trip 验证（类似 Node 测试的 `splits every bundled skill markdown file without losing bytes`），用 `pc-frontmatter` 跑 workspace 内 fixture。
