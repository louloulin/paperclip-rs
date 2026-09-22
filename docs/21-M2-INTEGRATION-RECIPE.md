# M2 集成配方（集成切片 LUM-1354 的执行手册 + 本轮实测）

> 实测时间：**2026-09-22 19:00–19:20 CST**（autopilot 19:00 cycle，LUM-1373）。
> 实测方式：只读 git（`log` / `diff --stat` / `show` / `ls-tree`）+ 在**独立 checkout** 里跑
> `cargo fmt --all --check`。本轮**没有**在三个并发切片的工作区里跑 cargo（避免抢
> `~/.cargo/.package-cache` 与 target 锁）。
>
> 本文件有两类内容，读的时候不要混：
> - **§2–§3 是实测快照**（数字会随切片提交而变化，集成时重测）；
> - **§4–§7 是规程**（分支/门禁/顺序/扫查），不随快照过期。

---

## 1. 集成基线（实测）

| 项 | 值 |
| --- | --- |
| 集成分支 | `feat/multica-rs-initial` |
| 远端 head | **`69e9f4b`**（`f05b803` = M2-C 合并 → `69e9f4b` = docs） |
| 已并入的 M2 切片 | **M2-C（LUM-1349）**：`fc8727b` → `f05b803`（inbox 仓储 + `/api/inbox` 14 条 + subscriber 4 条，`docs/13-M2-INBOX.md`） |
| 待并入 | M1-E（LUM-1362）、M2-A（LUM-1348）、M2-B（LUM-1350） |
| 迁移编号水位 | `0001`–`0004`（`0005` 空闲，按 `docs/10` §5.1 由集成 master 统一分配） |

`git ls-remote --heads origin` 实测存在的切片分支：`feat/multica-rs-m1a-*`、`m1b-*`、`m1c-*`、
`m2c-inbox`、`feat/multica-rs-m1` —— **M2-A / M2-B / M1-E 的分支尚未 push**，集成时它们必须先把
分支推上去（否则集成方只能从工作区捞未提交改动，风险极高）。

---

## 2. 在飞切片实况（2026-09-22 19:0x CST，工作区实测）

| 切片 | issue | 实际分支 | 基线 | 提交 | 未提交文件（`git diff --stat` 实测） |
| --- | --- | --- | --- | --- | --- |
| M1-E 契约缺口 | LUM-1362 | `feat/multica-rs-m1e-contract-gaps` | `69e9f4b` | `592ef90` + `9ec5b56` | 无（已提交，17 文件 / +1743 −175） |
| M2-A issue 核心 | LUM-1348 | `feat/multica-rs-m2a-issue` | `fd6dfd6` | 无 | `mc-repos/src/issue.rs` +1816、`mc-repos/src/lib.rs` +1、新文件 `issue_status.rs`（25 KB） |
| M2-B comment | LUM-1350 | ⚠️ `agent/devbox5/28f8edc92edd` | `fd6dfd6` | 无 | `mc-http/src/routes/comments.rs` +827、`mc-repos/src/comment.rs` +1404、新文件 `tests/comments.rs` |

**⚠️ M2-B 的分支名偏离约定**：`docs/10-M2-PLAN.md` §2 写的是 `feat/multica-rs-m2b-comment`，
实际是 `multica repo checkout` 自动生成的 `agent/devbox5/28f8edc92edd`。两个后果：

1. 集成方按计划名 `feat/multica-rs-m2b-comment` 找不到分支 → **集成前先 `git branch --show-current` /
   `multica issue runs` 核对工作区，别按计划名盲找**（LUM-1354 已写进描述）；
2. 该分支在 base 上是「沿用现有 checkout」语义，**M2-B 结束时必须显式 push**（别指望 merge 时被动发现）。

`fd6dfd6` → `69e9f4b` 的差集只有 M2-C 的 5 个源文件 + docs（见 §3），所以 M2-A / M2-B 从 `fd6dfd6`
起的分支**可以先 merge 再 rebase，冲突面不变**。

---

## 3. 文件冲突矩阵（A ∩ B = ∅ 为可并行前提，实测）

| 切片 | Repo 层 | HTTP 路由层 | 测试 | docs |
| --- | --- | --- | --- | --- |
| M2-A | `mc-repos/src/issue.rs`、`issue_status.rs`(新)、`lib.rs`(+1 行) | — | — | — |
| M2-B | `mc-repos/src/comment.rs` | `mc-http/src/routes/comments.rs` | `tests/comments.rs`(新) | — |
| M2-C（已并入） | `mc-repos/src/inbox.rs`、`subscriber.rs` | `routes/inbox.rs`、`subscribers.rs` | `tests/inbox.rs` | `13-M2-INBOX.md` |
| M1-E（在飞） | `mc-repos/src/inbox.rs`、`subscriber.rs` | `routes/{auth,inbox,invitations,mount,pats,subscribers,workspaces}.rs` | `tests/{contract_gaps,inbox,invitations}.rs` | `05/07/08/09/17-*` |

结论（逐对实测）：

- **M2-A ∩ M2-B ∩ M1-E = ∅** → 三条待并分支彼此**无文件级冲突**，可任意顺序 `--no-ff` 合并
  （沿用 M1-D / M2-C 的 merge-commit 惯例）。
- **M1-E 与 M2-C 同文件**（`inbox.rs` / `subscriber.rs` / `routes/inbox.rs` / `routes/subscribers.rs`）：
  因为 M1-E 的基线 `69e9f4b` **已包含** M2-C 合并，这是「顺序修改」不是「并行修改」→ 干净。
- 唯一需要人工看的共享区是 **`routes/mount.rs::router()`**：M1-E 在该函数体里删掉 `post` import
  与两条 M0 幽灵占位（`/api/auth/login`、`/api/auth/session`）。M2 切片的路由注册走各自的
  `mount_slice_*`（在各自文件里），所以**集成时若 `mount.rs` 报冲突，优先保留 M1-E 版本，
  再确认三条 `.merge(mount_slice_*)` 行仍在**。

---

## 4. 门禁清单（每个切片 PR 前 + 集成后都必须全绿）

```bash
export PATH="$HOME/.cargo/bin:$PATH"   # /usr/bin/cargo 是 1.75，构建不了本仓

cargo fmt --all --check                                              # ① 本轮新增（见 §5）
cargo build --workspace                                              # ②
cargo clippy --workspace --all-targets -- -D warnings                # ③
cargo clippy -p mc-http --all-targets --features mc-http/test-util -- -D warnings   # ④
MULTICA_TEST_DATABASE_URL=postgres://... cargo test -p mc-repos -p mc-http -- --ignored   # ⑤ DB e2e
```

- ③④ 是 M1/M2 各切片一直在跑的；**① 是 M2-C 漏掉的那道**（证据见 §5）。
- ⑤ 不接真库时 DB 测试会**静默跳过**（"绿"是空跑）——`docs/04` 与 LUM-1349 的验收记录都在强调这点，
  集成时至少对 `mc-repos` / `mc-http` 各跑一次带 `--ignored` 的真库用例。

---

## 5. `cargo fmt` 债务实测（本轮新增证据）

在独立 checkout（`agent/devbox5/47571b2fa65b`）里对两个 revision 各跑一次 `cargo fmt --all --check`：

| revision | 内容 | `fmt --check` | 差异文件（`^Diff in` 计数） |
| --- | --- | --- | --- |
| **`69e9f4b`** | 当前 base（含 M2-C 合并） | **exit 1，54 处** | `tests/inbox.rs` 27、`routes/inbox.rs` 11、`mc-repos/inbox.rs` 8、`mc-repos/subscriber.rs` 7、`routes/subscribers.rs` 1 |
| **`9ec5b56`** | M1-E 分支上的 fmt 修复提交 | **exit 0** | — |

两点结论：

1. **M2-C 是以 fmt 不干净的形态并入 base 的**：54 处差异 100% 落在 M2-C 的 5 个文件里，
   说明 M2-C 的验收只跑了 clippy/build/test，**没跑①**。base 在 `9ec5b56` 落入 base 之前一直是脏的。
2. `9ec5b56`（题面写 `chore(fmt)`）**确实只是格式化**：`git diff -w 69e9f4b 9ec5b56 -- <文件>`
   的残余差异全部是 rustfmt 换行/尾逗号引起的「一行拆成多行」（如 `self.retouch(...)` 拆成两行、
   `assert_eq!(a, b, "msg")` 拆成三行），**无逻辑改动**；结论是它可以安全并入，但仍应作为
   M1-E「越界修 M2-C 文件」在 PR 里注明（它已在 commit message 里注明）。
   → 因此：**集成时 M1-E 必须先于任何 `fmt --check` 判定**，否则会把「M2-C 的历史欠账」
   误判成「M1-E 引入的回归」。

---

## 6. 重复路由静态扫查（唯一守卫）

`.merge()` 对同 `(method, path)` 的重叠**不会 panic**（M1-D 实测结论，见 `docs/10` §5.3），
所以重复注册只能靠静态扫查发现。集成后执行：

```bash
# 抽出所有路由注册的字面 path（含 mount_slice_* 文件），看有无跨切片重复
grep -rhoE '\.route\( *"[^"]+"' crates/mc-http/src/routes/*.rs \
  | sed 's/.*"\(.*\)"/\1/' | sort | uniq -d
# 期望：M2-A/M2-B/M2-C/M1-E 之间的 path 集合无交集（只有 M0 以来就存在的 /api/health* 等共享项）
```

同时按 `docs/09` §7.4 扫一遍 **`{param}` 字面量段**（axum 0.7 必须写 `:param`，写错恒 404 且不报错）：

```bash
grep -rnoE '\.route\( *"[^"]*\{[a-zA-Z_]+\}' crates/mc-http/src/routes/*.rs   # 期望：无输出
```

---

## 7. 集成执行顺序与冲突预案（给 LUM-1354）

1. **先推分支**：M1-E / M2-A / M2-B 各自 `git push -u origin <实际分支名>`；集成方 `git fetch` 后
   用 `git log --oneline origin/feat/multica-rs-initial..<分支>` 核对「分支包含哪些提交」，不要只看 issue 状态。
2. **按「先 M1-E、后 M2-A、再 M2-B」顺序 `--no-ff` 合并**（M1-E 基线最新且改动面最杂，先并它可以让
   后面两条的冲突被判在最新 base 上）。
3. **每次 merge 后跑 §4 的五道门禁**（尤其①，见 §5），再跑 §6 的静态扫查。
4. **冲突预案**：
   - `mc-repos/src/lib.rs`（M2-A 加 1 行 `pub mod`）——保留双方 `pub mod` 行；
   - `routes/mount.rs`——保留 M1-E 的幽灵占位删除 + 三条 `mount_slice_*` merge 行（§3）；
   - `mc-repos/src/{inbox,subscriber}.rs`——M1-E 版本优先（它已在 M2-C 之上改过）。
5. **合并后必须写回 base 的东西**：更新 `docs/10-M2-PLAN.md` §5.3 的路由计数与
   `docs/09-M1-INTEGRATION.md` 的合并树记录；把本轮 §2 的分支实况替换成最终 merge commit 图。

---

## 8. 环境警示（沿用 `docs/20-UPSTREAM-ANALYSIS-RECIPE.md`）

**禁止**在 `git init` + `fetch --filter=blob:none` 的 blobby 仓库里跑 `git grep`（逐 blob 懒取，
单次 ETA ≈ 74 分钟，2026-09-22 实测卡死两个并发位）。要 grep 就先做 5.4s 的全 blob 浅克隆，
或按 `docs/20` §救援 给对象库补 blob。集成切片最容易踩到这个坑（要跨切片 grep 同名字段/路由）。
