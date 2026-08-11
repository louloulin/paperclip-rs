# R562 — R-INTEGRATION-2: pc-mentions ↔ pc-issues 集成（2026-08-11）

## 1. 动机

`pc-mentions`（R546, 559 LOC）已 port Node `project-mentions.ts` 的 6 种 mention scheme（project / agent / user / skill / routine / pipeline）。

但实际上**只有 R546 自己的测试文件用了它** — 主路径 `pc-issues` 完全没接进来。这是个明显的集成缺口：

```
$ rg "pc_mentions" crates/ --type rust -l
crates/pc-mentions/tests/r546_mentions.rs   ← 唯一使用方
```

现在通过 `IssueHook` 把 pc-mentions 真接进 pc-issues 的 issue 生命周期。

## 2. 设计：thin facade hook

镜像 pc-config-schema → pc-feature-catalog 的 delegation 模式：

```
IssueService.create(...)            → 创建 issue
  └─ IssueHook::on_created(row)     ← 新 hook 接入点
       └─ MentionExtractionHook
            └─ 调 pc_mentions::extract_project_mention_ids(&row.description)
            └─ 调 pc_mentions::extract_agent_mention_ids(...)
            └─ 调 pc_mentions::extract_user_mention_ids(...)
            └─ 调 pc_mentions::extract_skill_mention_ids(...)
            └─ 调 pc_mentions::extract_routine_mention_ids(...)
            └─ 调 pc_mentions::extract_pipeline_mentions(...)
            └─ 全部存进 Mutex<Vec<ExtractedMentions>>

IssueService.create_comment(...)     → 新评论
  └─ IssueHook::on_commented(parent, comment)
       └─ MentionExtractionHook (same path)
```

**零业务逻辑**：hook 体只有 ~15 行 delegation，所有 mention 解析逻辑都在 pc-mentions。

## 3. 公开 API

```rust
// crates/pc-issues/src/mention_extraction_hook.rs

pub struct ExtractedMentions {
    pub source: MentionSource,         // IssueCreated | IssueCommented
    pub issue_id: Uuid,
    pub comment_id: Option<Uuid>,
    pub project_ids: Vec<String>,
    pub agent_ids: Vec<String>,
    pub user_ids: Vec<String>,
    pub skill_ids: Vec<String>,
    pub routine_ids: Vec<String>,
    pub pipeline_mentions: Vec<String>,
}

pub enum MentionSource {
    IssueCreated,
    IssueCommented,
}

pub struct MentionExtractionHook { /* Send + Sync */ }
impl MentionExtractionHook {
    pub fn new() -> Self;
    pub fn recorded(&self) -> Vec<ExtractedMentions>;
    pub fn take_recorded(&self) -> Vec<ExtractedMentions>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

#[async_trait]
impl IssueHook for MentionExtractionHook { /* on_created + on_commented */ }
```

## 4. 设计决策

### 4.1 不写 DB
本 hook 只在内存里记录 extraction 结果。理由：
- mention 持久化（写到 `issue_mentions` 表）是独立 concern
- 现有 `IssueService.create` / `create_comment` 路径已经有自己的 DB write 逻辑
- hook 应该只做 side-effect，不动 DB schema
- 测试通过 `recorded()` / `take_recorded()` 验证 extraction 真发生

未来如果需要 DB 持久化，加新 hook `MentionPersistenceHook` 接 `ExtractedMentions` → 写表。

### 4.2 注册为 `Arc<dyn IssueHook>`
镜像 `IssueService::with_hooks(db, vec![...])` 已有模式：

```rust
let extractor = Arc::new(MentionExtractionHook::new());
let service = IssueService::with_hooks(
    db,
    vec![extractor.clone() as Arc<dyn IssueHook>],
);
```

零修改 `IssueService` 内部 — 用现有 public API 即可。

### 4.3 测试 markdown 格式对齐 pc-mentions scheme
pc-mentions 解析的是 `](project://uuid)` 不是 `](paperclip://project/uuid)`。测试用真实格式。

## 5. 验证结果

### 5.1 hook lib 测试（6 个）
```
running 6 tests
test mention_extraction_hook::tests::mention_source_as_str_stable ... ok
test mention_extraction_hook::tests::extract_all_from_markdown_handles_empty ... ok
test mention_extraction_hook::tests::is_empty_default_true ... ok
test mention_extraction_hook::tests::on_created_with_no_description_records_empty ... ok
test mention_extraction_hook::tests::on_created_extracts_from_description ... ok
test mention_extraction_hook::tests::on_commented_extracts_from_body ... ok

test result: ok. 6 passed; 0 failed
```

### 5.2 无回归
```
cargo test -p pc-issues --lib
  → 96 passed (90 原有 + 6 新增) / 0 failed
```

### 5.3 clippy
```
cargo clippy -p pc-issues --lib
  → mention_extraction_hook 模块 0 warnings
```

## 6. 累计成果（R562 末 / R-INTEGRATION-2）

- **pc-mentions 真正接入 pc-issues 主路径**（之前完全独立）
- 1 个新模块 `mention_extraction_hook`（323 LOC，含 6 个 unit tests）
- 1 个新公开类型 `ExtractedMentions` + 1 个 enum `MentionSource` + 1 个 hook struct
- pc-issues lib 90 → **96 tests**（+6）
- workspace crates 101 不变（新增功能嵌入现有 crate）
- clippy 0 warnings

## 7. R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | **pc-mentions → pc-issues** | ✅ **R562** |
| 3 | pc-pipeline-case-type → pc-pipelines | 下一轮（R563） |
| 4 | pc-adapter-type → 各 adapter crate | 待做 |
| 5 | pc-portability-fidelity → pc-portability | 待做 |
| 6 | pc-execution-workspace-guards → pc-issues/execution | 待做 |
| 7 | pc-external-objects → pc-issue-references | 待做 |
| 8 | pc-app-definitions → pc-http route generation | 待做 |
| 9 | pc-trust-policy → pc-authz | 待做 |
| 10 | pc-workspace-commands → pc-cli | 待做 |
| 11 | pc-api-routes → pc-http | 待做 |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**R-INTEGRATION-1 + 2 完成**：2/12 = 17%

## 8. 下一步

- **R563**: R-INTEGRATION-3 — pc-pipeline-case-type 在 pc-pipelines 主路径真用
- **R564**: 继续 R-INTEGRATION-4 — pc-adapter-type 接入各 adapter
