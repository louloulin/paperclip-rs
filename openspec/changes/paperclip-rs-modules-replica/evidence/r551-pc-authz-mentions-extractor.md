# R551 / M52 — pc-authz mention 解析器

## 本轮完成

新增 `crates/pc-authz/src/mentions.rs`，对齐 Node `packages/shared/src/project-mentions.ts` 的核心 mention 解析逻辑。

### 模块内容

- **6 个 mention scheme**：`agent://` / `user://` / `skill://` / `routine://` / `pipeline://` / `project://`
- **`parse_agent_mention_href`** — 解析 `agent://uuid[?i=icon]`，返回 `ParsedAgentMention { agent_id, icon }`
- **`parse_user_mention_href`** — 解析 `user://userId`，返回 `ParsedUserMention { user_id }`
- **`extract_agent_mention_ids`** — 从 markdown body 提取所有 agent mention UUIDs（去重）
- **`extract_user_mention_ids`** — 从 markdown 提取 user mention IDs（去重）
- **`extract_pipeline_mention_ids`** / `extract_routine_mention_ids` / `extract_skill_mention_ids`
- **`build_agent_mention_href(agent_id, icon?)`** — 构造 href
- **`build_user_mention_href(user_id)`** — 构造 href
- **`extract_hrefs`** — 内部：从 markdown 扫描所有 `[label](href)` 链接
- **`parse_query_param`** / `url_decode` / `hex_digit` / `is_agent_icon_name` 工具函数

### Markdown 链接格式

```
[@显示名](agent://uuid?i=icon)
```

匹配规则（与 Node `[^)\s]+` 行为对齐）：
- href 不能含空格或换行
- href 必须以指定 scheme 开头
- 多个相同 href 自动去重

### 验证

- `cargo test -p pc-authz --lib`：**64 passed**（+17 mentions 测试）
  - parse_agent_mention_href × 4
  - parse_user_mention_href × 2
  - extract_agent_mention_ids × 5
  - extract_user_mention_ids × 1
  - extract_routine_mention_ids × 1
  - extract_skill_mention_ids × 1
  - extract_agent_mention_ids_empty × 1
  - build_/parse_ round-trip × 2
- `cargo test --workspace --lib -- --test-threads=1`：**4993 passed**（+17）

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| HTTP 便捷 API（enforce_*） | ✅ M42 |
| Mention / consent / parent-report / responsible_user | ✅ M43 |
| 路由接入（34 个） | ✅ M42-M51 |
| e2e parity 测试 vs Node（22 + 6） | ✅ M46 |
| Trust preset + low-trust boundary | ✅ M47 |
| **Mention 解析器** | ✅ M52 |
| 全量接入所有受保护路由 | ⏳ 渐进 |
