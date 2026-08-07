# R383 — Server-Utils Final 5 Gaps Closure

## 目标

R382 把 `server-utils.ts` 5 个 stub(plan review / task watchdog /
liveness continuation / annotation deltas / continuation summary)
落到了 typed normalize + render。但 `prompt_compose.rs` 仍然有 5 个
"半实现"节点继续依赖 `Vec<Value>` / `Option<Value>` 占位、缺 render
分支、缺 Node parity 行为。本轮把这 5 个全部闭合,让
`render_paperclip_wake_prompt` 与 Node `server-utils.ts` 字段级一致。

## 范围

- `paperclip-rs/crates/pc-acpx/src/prompt_compose.rs`
- `paperclip-rs/crates/pc-acpx/tests/round383_remaining_gaps.rs`(新)
- 跟 Node `paperclip/packages/adapter-utils/src/server-utils.ts`
  的 L1028-1042 / L1066-1077 / L1208-1210 / L1231-1243 /
  L1247-1254 / L1455-1460 / L1770-1785 行对齐。

## 闭合的 5 个 Gap

### Gap 1 — typed blocker summaries (Node L1028-1042)

之前:`NormalizedPaperclipWake.unresolved_blocker_summaries: Vec<Value>`
依赖 render 阶段 `v.as_object()` 重新解包 — 既无类型又冗余。

R383:新增 `PaperclipWakeBlockerSummary { id / identifier / title /
status / priority }` typed struct,`normalize_paperclip_wake_blocker_summary`
过滤全空条目,render 阶段直接 `.iter().map(|b| ...)` 生成
`"PC-B-1 Auth blocker (open)"` 风格行。

### Gap 2 — execution principal labels (Node L1066-1077 + L1455-1460)

之前:`PaperclipWakeExecutionStage` 缺失 `currentParticipant` /
`returnAssignee` 字段,render 阶段无任何执行人信息输出。

R383:
- `PaperclipWakeExecutionPrincipal { principal_type / agent_id / user_id }` typed
- `normalize_paperclip_wake_execution_principal` 接受 `"agent"` / `"user"`
  (大小写不敏感)并 reject 其他 type → None
- `principal_label(Option<&PaperclipWakeExecutionPrincipal>) -> String`
  返回 `"agent <id>"` / `"agent"` / `"user <id>"` / `"user"` / `"unknown"`
- render 加 `- execution participant: <label>` + `- execution return assignee: <label>`

### Gap 3 — review request instructions body (Node L1770-1785)

之前:Node 在 `executionStage.reviewRequest.instructions` 非空时输出
"Review request instructions:" + body,以及 reviewer/approver/executor
角色专属 trailer 段。Rust 完全缺失。

R383:
- `PaperclipWakeReviewRequest { instructions: String }` typed
- `normalize_paperclip_wake_review_request` trim + 空白 → None
- render 阶段:
  - 已有 review_request → 插空行 + "Review request instructions:" + 内容
  - wakeRole == reviewer/approver → 插 "You are waking as the active ... for this issue." 等 4 行 reviewer/approver 段
  - wakeRole == executor → 插 "You are waking because changes were requested in the execution workflow." 等 2 行 executor 段

### Gap 4 — execution workspace sanitize (Node L1231-1243)

之前:`normalize_execution_workspace` 只读 `branchName` / `workspaceId`
不过滤控制字符、不截断长度 — 违反 git ref 规则且 prompt 注入风险。

R383:
- `MAX_EXECUTION_WORKSPACE_BRANCH_CHARS = 300` 常量
- filter `cp > 0x1f && cp != 0x7f` (U+0000-U+001F + U+007F)
- trim + chars().take(300)
- 全空 → 返回 None(否则 Some(...))

### Gap 5 — markdown_inline_code trailing space (Node L1247-1254)

之前:`format!("{} {}", fence, value)` 缺 trailing space,与 Node
格式不一致(`format!("{fence} {value} {fence}", ...)`)。

R383:改为 `format!("{} {} {}", fence, value, fence)` — leading AND
trailing space,确保嵌入的 backtick run 不能提前关闭 span。

加 3 个回归测试覆盖:
- 普通 value(无 backtick)
- 含单 backtick value(fence = ````)
- 含 4 长 backtick run 的 value(fence = ```````)

## 测试

- 11 个新单元测试注入 `prompt_compose::tests`(同 `mod tests` 块)
- 13 个新集成测试在 `tests/round383_remaining_gaps.rs`

合计 R383 新增 24 个测试,全部绿色。

## Node 源码参考

```ts
// L1028-1042
function normalizePaperclipWakeBlockerSummary(value): PaperclipWakeBlockerSummary | null {
  const blocker = parseObject(value);
  const id = asString(blocker.id, "").trim() || null;
  ...
  if (!id && !identifier && !title && !status) return null;
  return { id, identifier, title, status, priority };
}

// L1066-1077
function normalizePaperclipWakeExecutionPrincipal(value): PaperclipWakeExecutionPrincipal | null {
  const principal = parseObject(value);
  const typeRaw = asString(principal.type, "").trim().toLowerCase();
  if (typeRaw !== "agent" && typeRaw !== "user") return null;
  return { type: typeRaw, agentId: ..., userId: ... };
}

// L1231-1243
function normalizePaperclipWakeExecutionWorkspace(value) {
  const workspace = parseObject(value);
  const branchName = asString(workspace.branchName, "")
    .replace(/[\u0000-\u001f\u007f]/g, "")
    .trim()
    .slice(0, 300) || null;
  if (!branchName) return null;
  return { branchName };
}

// L1247-1254
function markdownInlineCode(value) {
  const longestBacktickRun = value.match(/`+/g)?.reduce((m, r) => Math.max(m, r.length), 0) ?? 0;
  if (longestBacktickRun === 0) return `\`${value}\``;
  const fence = "`".repeat(longestBacktickRun + 1);
  return `${fence} ${value} ${fence}`;  // 注意:trailing space
}

// L1455-1460
const principalLabel = (principal) => {
  if (!principal || !principal.type) return "unknown";
  if (principal.type === "agent") return principal.agentId ? `agent ${principal.agentId}` : "agent";
  return principal.userId ? `user ${principal.userId}` : "user";
};

// L1770-1785 — executionStage render 段
if (executionStage.reviewRequest) {
  lines.push("", "Review request instructions:", executionStage.reviewRequest.instructions);
}
lines.push("");
if (executionStage.wakeRole === "reviewer" || executionStage.wakeRole === "approver") {
  lines.push(
    `You are waking as the active ${executionStage.wakeRole} for this issue.`,
    "Do not execute the task itself or continue executor work.",
    "Review the issue and choose one of the allowed actions above.",
    "If you request changes, the workflow routes back to the stored return assignee.",
    "",
  );
} else if (executionStage.wakeRole === "executor") {
  lines.push(
    "You are waking because changes were requested in the execution workflow.",
    "Address the requested changes on this issue and resubmit when the work is ready.",
    "",
  );
}
```

## 验证

```
cd paperclip-rs && cargo test -p pc-acpx
```

结果:493 个 pc-acpx tests 通过 (293 lib + 200 integration),
0 失败 0 回归。`round383_remaining_gaps.rs` 13 个新增测试全绿。

## 下一步

- **R384**:继续复刻 `server-utils.ts` 的 advisory recovery plan /
  additional blocks (`- liveness block: continue` / `- watchdog
  actions available` / 复审 plan revisions 等)。
- **R385+**:把 `issue_render.rs` 的 plan review `selectedText` /
  `prefixText` / `suffixText` trim / truncate 行为补齐(Node parity)。
- **R386+**:复刻 wake prompt 后置的 `## State` / `## Resume
  contract` / `## Resume acknowledgement` / `## Recent wake history`
  段(目前 Rust 端没有这些 block)。
