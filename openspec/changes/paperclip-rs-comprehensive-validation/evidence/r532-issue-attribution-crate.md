# R532 — port Node `packages/shared/src/issue-attribution.ts` → Rust `pc-issue-attribution`

**日期**: 2026-08-11
**轮次**: R532
**目标**: 把 Node 上游 issue attribution 派生 (responsible user + originating actor) 完整 port 到 Rust
**模块**: 新 crate `crates/pc-issue-attribution/`

---

## 改动

### 上游 Node 实现 (57 LOC)

`packages/shared/src/issue-attribution.ts`:
```typescript
type ResponsibleUserSource = "explicit" | "creator" | "none";

interface ResponsibleUserAttribution {
  userId: string | null;
  source: ResponsibleUserSource;
  isAutoDerived: boolean;
}

export function deriveResponsibleUser(
  issue: Pick<Issue, "responsibleUserId" | "createdByUserId">,
): ResponsibleUserAttribution {
  if (issue.responsibleUserId) return { userId: issue.responsibleUserId, source: "explicit", isAutoDerived: false };
  if (issue.createdByUserId) return { userId: issue.createdByUserId, source: "creator", isAutoDerived: true };
  return { userId: null, source: "none", isAutoDerived: false };
}

type OriginatingActor =
  | { kind: "user"; id: string; viaAgentId?: string }
  | { kind: "agent"; id: string };

export function deriveOriginatingActor(
  issue: Pick<Issue, "createdByUserId" | "createdByAgentId" | "responsibleUserId">,
): OriginatingActor | null {
  if (issue.createdByUserId) return { kind: "user", id: issue.createdByUserId };
  if (issue.createdByAgentId) {
    if (issue.responsibleUserId) return { kind: "user", id: issue.responsibleUserId, viaAgentId: issue.createdByAgentId };
    return { kind: "agent", id: issue.createdByAgentId };
  }
  if (issue.responsibleUserId) return { kind: "user", id: issue.responsibleUserId };
  return null;
}
```

`packages/shared/src/issue-attribution.test.ts` (8 个测试):
- `deriveResponsibleUser` (3 个): explicit 优先 / fallback 到 creator / 全 None
- `deriveOriginatingActor` (5 个): 人类创建者优先 / agent 创建 + responsible user (via) / agent 创建无 responsible user / routine execution / 全 None

### Rust port (单 crate `pc-issue-attribution`, 434 LOC, 18 测试)

**公开 API**:
```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponsibleUserSource { Explicit, Creator, None }

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponsibleUserAttribution {
    pub user_id: Option<String>,
    pub source: ResponsibleUserSource,
    pub is_auto_derived: bool,
}

pub struct ResponsibleUserInput {
    pub responsible_user_id: Option<String>,
    pub created_by_user_id: Option<String>,
}
impl ResponsibleUserInput { pub fn new(...) -> Self; }

pub fn derive_responsible_user(issue: &ResponsibleUserInput) -> ResponsibleUserAttribution;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginatingActor {
    #[serde(rename_all = "camelCase")]
    User { id: String, #[serde(skip_serializing_if = "Option::is_none")] via_agent_id: Option<String> },
    #[serde(rename_all = "camelCase")]
    Agent { id: String },
}
impl OriginatingActor {
    pub fn id(&self) -> &str;
    pub fn is_user(&self) -> bool;
}

pub struct OriginatingActorInput {
    pub created_by_user_id: Option<String>,
    pub created_by_agent_id: Option<String>,
    pub responsible_user_id: Option<String>,
}
impl OriginatingActorInput { pub fn new(...) -> Self; }

pub fn derive_originating_actor(issue: &OriginatingActorInput) -> Option<OriginatingActor>;
```

**私有 helper**:
- `non_empty_str(s: Option<&str>) -> Option<&str>` — 空字符串视为 None

---

## 测试 (18 个)

**derive_responsible_user (5 个)**:
| # | 测试 | 验证 |
|---|---|---|
| 1 | `r532_responsible_prefers_explicit` | explicit > creator (上游测试 1) |
| 2 | `r532_responsible_falls_back_to_creator` | 无 explicit → creator (上游测试 2) |
| 3 | `r532_responsible_returns_none_when_nothing_available` | 都空 → None source (上游测试 3) |
| 4 | `r532_responsible_treats_empty_string_as_none` | 空字符串视为 None (Node `if(issue.responsibleUserId)` falsy) |
| 5 | `r532_responsible_explicit_empty_with_no_creator_returns_none` | explicit="" + no creator → None |

**derive_originating_actor (8 个)**:
| # | 测试 | 验证 |
|---|---|---|
| 6 | `r532_originating_prefers_human_creator` | 人类创建者优先 (上游测试 4) |
| 7 | `r532_originating_agent_creator_with_responsible_user` | agent 创建 + responsible user → via agent (上游测试 5) |
| 8 | `r532_originating_agent_creator_without_responsible_user` | agent 创建无 responsible user → agent (上游测试 6) |
| 9 | `r532_originating_routine_execution_no_creator` | routine 执行无 creator → responsible user (上游测试 7) |
| 10 | `r532_originating_returns_null_when_nothing_attributable` | 全 None → null (上游测试 8) |
| 11 | `r532_originating_human_creator_overrides_agent` | 三者都有, 人类创建者胜出 |
| 12 | `r532_originating_treats_empty_string_as_none` | 空字符串视为 missing, 走 fallback |
| 13 | `r532_originating_empty_strings_all_around` | 三个字段都 "" → null |

**OriginatingActor helpers + serde (5 个)**:
| # | 测试 | 验证 |
|---|---|---|
| 14 | `r532_originating_actor_id_helper` | `.id()` 返回 user/agent 的 id |
| 15 | `r532_originating_actor_is_user_helper` | `.is_user()` 区分 user/agent |
| 16 | `r532_originating_actor_serde_roundtrip` | `{"kind":"user","id":"u1","viaAgentId":"a1"}` roundtrip |
| 17 | `r532_originating_actor_serde_omits_none_via_agent` | via_agent_id=None → 字段不出现在 JSON |
| 18 | `r532_responsible_source_serde_roundtrip` | source → `"explicit"` / `"creator"` / `"none"` |

---

## 验证

```bash
$ cargo test -p pc-issue-attribution --lib
running 18 tests
... (all 18 passed)
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test --workspace --lib
... (all 75 crates pass)
Total passed: 6598; Total failed: 0
```

---

## 设计要点

### 1. 强类型 enum 替代 string union
- Node: `type ResponsibleUserSource = "explicit" | "creator" | "none"` (TS string union)
- Rust: `enum ResponsibleUserSource { Explicit, Creator, None }` (强类型)
- `#[serde(rename_all = "snake_case")]` 让 JSON 输出仍是 `"explicit"` / `"creator"` / `"none"` 对齐 API 契约

### 2. Serde camelCase 对齐 Node API
- Node 字段是 `viaAgentId` / `isAutoDerived` / `userId` (camelCase)
- Rust 结构体默认 snake_case, 用 `#[serde(rename_all = "camelCase")]` 转换
- 测试 `r532_originating_actor_serde_roundtrip` 验证 JSON 包含 `"viaAgentId":"a1"` 而不是 `"via_agent_id"`

### 3. Tagged union for `OriginatingActor`
- Node 用 discriminated union (`{kind: "user", id, viaAgentId?} | {kind: "agent", id}`)
- Rust 用 `#[serde(tag = "kind")]` enum
- JSON 输出 `{"kind":"user","id":"u1","viaAgentId":"a1"}` 完全对齐 Node
- 测试 `r532_originating_actor_serde_roundtrip` 验证

### 4. `Option<None>` 强类型 vs Node `string | null`
- Node `string | null | undefined` 三态, Rust `Option<String>` 一态
- 调用方必须显式 `Some(...)` 或 `None`, 编译期阻止忘记处理 None
- 与 R529 `ConnectionInput` enum、R530 `Option<&str>`、R531 `Option<&str>` 设计哲学一致

### 5. `Pick`-style 输入 struct
- Node `Pick<Issue, "responsibleUserId" | "createdByUserId">` 只取所需字段
- Rust: `ResponsibleUserInput { responsible_user_id, created_by_user_id }` 同样只取所需
- 避免依赖完整 `Issue` row, 测试也不需要 mock 整个 Issue

### 6. 空字符串视为 None (Node quirk)
- Node: `if (issue.responsibleUserId)` — 空字符串 falsy
- Rust: `non_empty_str` helper 显式处理 `Option<&str>::filter(|v| !v.is_empty())`
- 测试 `r532_responsible_treats_empty_string_as_none` + `r532_originating_*` 多个空字符串测试验证

### 7. 集成层 (留给后续 round)
- `pc-repos::issue` 持久化层: 调用 `derive_responsible_user(row)` 填入响应 DTO
- server `routes/issues.ts` endpoint: 返回 JSON 时直接 emit `ResponsibleUserAttribution`
- UI 端 "Originating: User via Agent" 渲染 (UI 是冻结契约, 不动)

---

## V 真实进度更新

| V | R532 前 | R532 后 | 增量 |
|---|---|---|---|
| V1 | ~80% | ~80% | — |
| V2 | 61% | 61% | — |
| V3 | 100% | 100% | — |
| V4 | ~60% | ~60% | — |
| V5 | ~85% | ~85% | — |
| V6 | ~100% | ~100% | — |
| V8 | 0% | 0% | — |
| V9 | ~40% | ~40% | — |
| V10 | ~30% | ~30% | — |
| V11/V12 | 0% | 0% | — |

R532 是**质量层 + 重构**轮次: 把 Node 业务规则 (responsible user + originating actor 派生) 提到独立 crate, 可独立测试 + 跨 crate 复用 (pc-repos, pc-issue, pc-routines, pc-runs 等多处都用)。

---

## 教训

1. **closure lifetime 陷阱**: 第一次写 `let non_empty = |s: &Option<String>| s.as_deref().filter(|v| !v.is_empty());` 触发 "lifetime may not live long enough" 编译错误. 嵌套闭包 borrow checker 处理不好, 改成命名函数 `fn non_empty_str(s: Option<&str>) -> Option<&str>` 干净利落.
2. **serde rename_all 范围**: enum-level `rename_all = "snake_case"` 只影响 variant names, 不影响字段名. 字段 camelCase 需要在每个 variant 上单独 `#[serde(rename_all = "camelCase")]`. 一开始我只放了 enum-level, 测试 `viaAgentId` 失败才发现.
3. **`#[serde(skip_serializing_if = "Option::is_none")]`**: 不加这个, JSON 会多一个 `"viaAgentId": null` 字段. Node 上游通过 `viaAgentId?: string` (optional) 不输出, Rust 也要镜像. 测试 `r532_originating_actor_serde_omits_none_via_agent` 验证.
4. **discriminated union 对齐 Node**: Node TS 用 `{kind: "user", id, viaAgentId?} | {kind: "agent", id}` 区分, Rust 用 enum + `#[serde(tag = "kind")]` 完美 1:1 镜像.

---

## 下一步

### R533 (推荐)
- **port `packages/shared/external-objects.ts` (52 LOC)** — `formatExternalObjectMentionSourceLabel` 简单函数, 用于 UI mention source label
- 或者 **pc-pipelines 继续模块化** (拆 `pipeline_health.rs` / `pipeline_automation.rs`)

### R534
- **V8 远程 SSH execution**: `restoreRemoteWorkspace` + `materializeRemoteClaudeConfig`
- **V10 plugin 互操作**: spawn 真实 subprocess 跑 plugin

### R535
- **pc-secret-binding 集成层**: 把 R527 `pc-secret-redaction` + R526 `pc-log-redaction` helpers 接到 `pc-http` middleware

### R536+ (V11/V12/V13)
- UI 60 client happy 跑
- Playwright 真实 UI 剧本
- 长跑性能 baseline
