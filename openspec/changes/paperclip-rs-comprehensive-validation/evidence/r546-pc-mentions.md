# R546 — pc-mentions（Node project-mentions.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/project-mentions.ts` (322 LOC) 完整复刻到新 crate
`crates/pc-mentions`。workspace crates 87 → **88**。

支持 6 个 mention schemes：project / agent / user / skill / routine / pipeline。

## 设计原则

### 1. 强类型 Parsed structs
- 每个 scheme 一个 `ParsedXxxMention` struct，字段语义与 Node 接口 1:1 对应
- `Option<String>` 表达"可选附属信息"（color / icon / slug / stage_key）
- 与 Node `interface ParsedProjectMention { projectId, color }` 完全等价

### 2. 纯函数 API，零全局状态
- 所有函数为 `pub fn`，输入 `&str`，输出 `Option<ParsedXxxMention>` / `String` / `Vec<_>`
- 不依赖运行时 env / I/O；测试 100% deterministic
- 与 Node 风格一致：`export function parseXxxMentionHref(href): ParsedXxxMention | null`

### 3. Node 行为等价
- `SchemeUrl` 内部类型手写实现 URL 解析（避免引入 `url` crate）
- 完全镜像 Node `new URL(href)` + `url.hostname + url.pathname` + `url.searchParams.get`
- `host_path_id()` 等价 `\`${url.hostname}${url.pathname}\`.replace(/^\/+/, "").trim()`
- `query_param` 等价 `searchParams.get`，但支持 `c` / `color` 双别名

### 4. percent-encoding 完全镜像 `encodeURIComponent`
- 自定义 `ENCODE_URI_COMPONENT: AsciiSet` = complement(EMPTY) - alphanumeric - `-._~` - `!*'()`
- 区别于 RFC 3986 严格 query set（`encodeURIComponent` 也保留 `!*'()`）
- 测试覆盖 `-` 不被错误编码为 `%2D`

### 5. `extract_*` 用相同内部 helper 复用
- `find_mention_hrefs` 一次扫描 markdown，按 scheme 找到 `](<scheme>://...)` 区间
- 终止符为 `)` 或 whitespace（镜像 Node regex `[^)\s]+`）
- 每个 `extract_*_mention_ids` 独立 dedup 但共享 helper

### 6. pipeline mentions 按 string key dedup
- Node 用 `\`${pipelineId}:${stageKey ?? ""}\`` 作为 dedup key
- Rust 端用 `HashSet<String>` 持有同样的 key
- 与按 struct hash 区别：避免 `ParsedPipelineMention` 之外的隐式 `Eq` 约束

## 公开 API

```rust
pub const PROJECT_MENTION_SCHEME: &str = "project://"
pub const AGENT_MENTION_SCHEME: &str = "agent://"
pub const USER_MENTION_SCHEME: &str = "user://"
pub const SKILL_MENTION_SCHEME: &str = "skill://"
pub const ROUTINE_MENTION_SCHEME: &str = "routine://"
pub const PIPELINE_MENTION_SCHEME: &str = "pipeline://"

pub struct ParsedProjectMention { project_id: String, color: Option<String> }
pub struct ParsedAgentMention { agent_id: String, icon: Option<String> }
pub struct ParsedUserMention { user_id: String }
pub struct ParsedSkillMention { skill_id: String, slug: Option<String> }
pub struct ParsedRoutineMention { routine_id: String }
pub struct ParsedPipelineMention { pipeline_id: String, stage_key: Option<String> }

pub fn build_project_mention_href(project_id: &str, color: Option<&str>) -> String
pub fn build_agent_mention_href(agent_id: &str, icon: Option<&str>) -> String
pub fn build_user_mention_href(user_id: &str) -> String
pub fn build_skill_mention_href(skill_id: &str, slug: Option<&str>) -> String
pub fn build_routine_mention_href(routine_id: &str) -> String
pub fn build_pipeline_mention_href(pipeline_id: &str, stage_key: Option<&str>) -> String

pub fn parse_project_mention_href(href: &str) -> Option<ParsedProjectMention>
pub fn parse_agent_mention_href(href: &str) -> Option<ParsedAgentMention>
pub fn parse_user_mention_href(href: &str) -> Option<ParsedUserMention>
pub fn parse_skill_mention_href(href: &str) -> Option<ParsedSkillMention>
pub fn parse_routine_mention_href(href: &str) -> Option<ParsedRoutineMention>
pub fn parse_pipeline_mention_href(href: &str) -> Option<ParsedPipelineMention>

pub fn extract_project_mention_ids(markdown: &str) -> Vec<String>
pub fn extract_agent_mention_ids(markdown: &str) -> Vec<String>
pub fn extract_user_mention_ids(markdown: &str) -> Vec<String>
pub fn extract_skill_mention_ids(markdown: &str) -> Vec<String>
pub fn extract_routine_mention_ids(markdown: &str) -> Vec<String>
pub fn extract_pipeline_mentions(markdown: &str) -> Vec<ParsedPipelineMention>
```

## 修复的两个 bug

### bug #1 — `find_mention_hrefs` 索引偏移
- 初版用 `find("](project://")` 后 +1 起点，导致返回 `(project://p1`（含括号）
- 修复：起点应跳过 `](` 完整 2 字符，返回完整 `<scheme>://<id>` 片段
- 影响所有 `extract_*` 函数

### bug #2 — `NON_ALPHANUMERIC` 把 `-` 错误编码
- 初版用 `percent_encoding::NON_ALPHANUMERIC`，导致 `my-slug` → `my%2Dslug`
- 修复：手写 `ENCODE_URI_COMPONENT = AsciiSet::EMPTY.complement() - alphanumeric - "-_.~" - "!*'()"`
- 等价 Node `encodeURIComponent` 行为

## 与上游 Node 差异

- **驼峰 → 蛇形**：`projectId` → `project_id`，保持 Rust 命名习惯
- **`SchemeUrl` 内部结构体**：代替 Node 直接用 `URL` 全局对象
- **`HashSet<String>` 字符串 key dedup**（pipeline）：镜像 Node `\`${id}:${stage ?? ""}\`` 模式
- **依赖最小化**：仅 `percent-encoding`（Node 不需要，因为 `URL` 内置）

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-mentions` | **42 passed** (3 internal + 39 integration) |
| `cargo fmt -p pc-mentions -- --check` | ✅ 通过 |
| `cargo clippy -p pc-mentions --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（39 个集成 + 3 internal）

- **6 × build** (12): 无 option / 有 option / 无效 option 丢弃 / round-trip
- **6 × parse** (12): 基础 / alias / wrong-scheme / empty-id / round-trip
- **5 × extract_project/agent/user/skill/routine + 1 × extract_pipeline** (10): basic / dedup / empty / 跨 scheme 忽略
- **normalize helpers** (3 internal): hex_color / agent_icon / skill_slug

## 集成待办（不在本轮范围）

- `pc-server` markdown 渲染管线：`extract_*_mention_ids` 用于链接实体 → 卡片预览
- `pc-pipeline-conversation-context`：消费 `extract_pipeline_mentions` 收集 pipeline stage 引用
- `pc-routines` UI：用 `extract_routine_mention_ids` 自动关联 routine 卡片
- 端到端：在 chat markdown 中嵌 `project://xxx?c=ff00aa`，验证前后端解析一致
