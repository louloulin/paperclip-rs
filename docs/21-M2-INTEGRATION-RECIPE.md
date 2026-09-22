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
| 远端 head | **`9c57592`**（`f05b803` = M2-C 合并 → `8df6888`/`90cd17c`/`9c57592` = 19:00 cycle 的 docs） |
| 已并入的 M2 切片 | **M2-C（LUM-1349）**：`fc8727b` → `f05b803`（inbox 仓储 + `/api/inbox` 14 条 + subscriber 4 条，`docs/13-M2-INBOX.md`） |
| 待并入 | M1-E（LUM-1362）、M2-A（LUM-1348）、M2-B（LUM-1350） |
| 迁移编号水位 | `0001`–`0004`（`0005` 空闲，按 `docs/10` §5.1 由集成 master 统一分配） |

> ⚠️ **本文件的 SHA 快照会过期，且实测已经过期过一次**：本 cycle 19:0x 首次实测时
> M1-E 还是 `592ef90`+`9ec5b56` 且未 push，M2-B 还是未提交的工作区文件；十几分钟后复测发现
> **M1-E 已 rebase 并 push（`ae1dd3b`+`9851ebf`）、M2-B 已提交并 push（`48666c2`）**。
> 集成时请**以远端分支 + issue 状态为准**，把 §2 当作「怎么核」的样例而不是真值。

---

## 2. 在飞切片实况（2026-09-22 19:0x CST，工作区实测）

| 切片 | issue | 远端分支 | 基线 | 提交（已 push？） | 实测 diff（`git diff --stat <base> <branch>`） |
| --- | --- | --- | --- | --- | --- |
| M1-E 契约缺口 | LUM-1362 | `feat/multica-rs-m1e-contract-gaps` | `69e9f4b` | `ae1dd3b` + `9851ebf`（✓ 已 push） | 17 文件 +1748 −175 |
| M2-A issue 核心 | LUM-1348 | `feat/multica-rs-m2a-issue`（✗ **未 push**） | `fd6dfd6` | **无提交** | 工作区未提交：`mc-repos/src/issue.rs` +1816、`lib.rs` +1、新 `issue_status.rs` |
| M2-B comment | LUM-1350 | `feat/multica-rs-m2b-comment` | `fd6dfd6` | `48666c2`（✓ 已 push） | 4 文件 +3160 −13（含新增 `docs/12-M2-COMMENT.md`） |

两条由此得到的规程：

1. **分支名以远端为准**。M2-B 的工作区分支名曾是 `multica repo checkout` 自动生成的
   `agent/devbox5/28f8edc92edd`，交付时按 `docs/10` §2 的计划名 push 成了 `feat/multica-rs-m2b-comment`；
   集成方不要去猜工作区里的名字（`git branch --show-current` 只能看到本地 checkout 的状态）。
2. **工作区里有改动 ≠ 切片在跑**（反过来也成立）：M2-A 是真在跑（head 仍是 `fd6dfd6`、改动未提交）；
   而 M1-E / M2-B 已提交并 push。判断在飞状态要同时看 `git log <base>..<branch>`、
   `git ls-remote origin <branch>` 与 issue 状态，不能只看文件 mtime。

`fd6dfd6` → 当前 base 的差集只有 M2-C 的 5 个源文件 + docs（见 §3），所以 M2-A / M2-B 从 `fd6dfd6`
起的分支**可以直接 merge，无需 rebase**。

---

## 3. 文件冲突矩阵（A ∩ B = ∅ 为可并行前提，实测）

| 切片 | Repo 层 | HTTP 路由层 | 测试 | docs |
| --- | --- | --- | --- | --- |
| M2-A | `mc-repos/src/issue.rs`、`issue_status.rs`(新)、`lib.rs`(+1 行) | — | — | — |
| M2-B | `mc-repos/src/comment.rs` | `mc-http/src/routes/comments.rs` | `mc-http/tests/comments.rs`(新) | `12-M2-COMMENT.md`(新) |
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

在**独立工作树**里对三个 revision 各跑一次 `cargo fmt --all --check`（复测命令见下方第 0 条）：

| revision | 内容 | `fmt --check` | 差异文件（`^Diff in` 计数） |
| --- | --- | --- | --- |
| **`90cd17c`**（≈`69e9f4b`，当前 base） | 含 M2-C 合并 | **exit 1，54 处** | `crates/mc-http/tests/inbox.rs` 27、`routes/inbox.rs` 11、`mc-repos/inbox.rs` 8、`mc-repos/subscriber.rs` 7、`routes/subscribers.rs` 1 |
| **`9851ebf`** | M1-E 分支 head（`chore(fmt)`；rebase 前的同内容版本 `9ec5b56`） | **exit 0** | — |
| **`48666c2`** | M2-B 分支 head（comment 切片） | **exit 0** | — |

两点结论：

0. 复测方式与可复现性：`git worktree add --detach ../_wt_<rev> <rev>` 后在那个**独立工作树**里跑
   `cargo fmt --all --check`（fmt 不编译、不加 target 锁，所以三个切片并发时也能安全跑；
   本轮对 `90cd17c` / `9851ebf` / `48666c2` 各跑一次，数字如上）。
   ⚠️ 失败输出以 `Diff in <abs-path>:<line>:` 开头，计数用 `grep -c '^Diff in'`；
   管道 + `tail` 会吞掉退出码（要用 `${PIPESTATUS[0]}` 或先重定向再看 `$?`）。

1. **M2-C 是以 fmt 不干净的形态并入 base 的**：54 处差异 100% 落在 M2-C 的 5 个文件里，
   说明 M2-C 的验收只跑了 clippy/build/test，**没跑①**。base 在 M1-E 的 `chore(fmt)` 提交落进 base 之前一直是脏的。
2. M1-E 的 `9851ebf chore(fmt)` **确实只是格式化**，但**别用 `git diff -w` 去证明**：
   rustfmt 的「多行链重新合成一行」在 `-w` 下仍是真实行变更，本处 `git diff -w ae1dd3b 9851ebf`
   实测**仍有 346 行残余差异**，所以 `-w` 只能排除「纯空白改动」，证明不了「仅格式化」。
   能证明的可复现检查是**删掉空白与逗号后逐文件比哈希**（rustfmt 换行 + 尾逗号是它唯一能引入的 token 差）：

   ```bash
   for f in crates/mc-http/src/routes/inbox.rs crates/mc-http/src/routes/subscribers.rs \
            crates/mc-http/tests/inbox.rs crates/mc-repos/src/inbox.rs crates/mc-repos/src/subscriber.rs; do
     a=$(git show ae1dd3b:$f | tr -d '[:space:],' | sha256sum | cut -c1-12)
     b=$(git show 9851ebf:$f | tr -d '[:space:],' | sha256sum | cut -c1-12)
     [ "$a" = "$b" ] && echo "OK  $f" || echo "REAL DIFF  $f"
   done
   # 本轮实测：5/5 文件均 OK（删除空白+逗号后哈希相同 → 只剩空白与尾逗号的改动）
   ```

   结论：该提交可以安全并入，但它修的是 **M2-C 的文件**（属 M1-E 越界改动，已在 commit message 注明）；
   → **集成时 M1-E 必须先于任何 `fmt --check` 判定**，否则会把「M2-C 的历史欠账」
   误判成「M1-E 引入的回归」。

---

## 6. 重复路由检查（两层守卫，勿信“.merge 不 panic”的传闻）

**源码级实测（axum 0.7.9，本机 registry）**：同 `(method, path)` 重复注册/合并会 **panic**，
不是静默覆盖：

- `src/routing/method_routing.rs:1053` — `panic!("Overlapping method route. Handler for `{name} {path}` already exists")`
- 官方测试 `src/routing/tests/mod.rs:544` `merging_with_overlapping_method_routes`（`Router::merge(app.clone())`）
  期望这条 panic；同文件 `:552` 另有“同 path 不同 method 可以 merge”的对照测试。

所以集成时的守卫是两层的：

1. **构建 router 就 panic**——只要有测试/启动路径真的构造 `router(state)`，重复路由会当场炸，
   不会被静默掩盖（集成后务必让 DB e2e / smoke 真的把 `router(state)` 构造一遍）；
2. **静态提前发现**（比等 panic 更快定位到是哪两个切片撞了）。注意：**只看 path 的
   `grep ... | uniq -d` 会误报**——同 path 不同 method（`GET` + `PATCH /api/workspaces/:id`）是合法的，
   本轮实测它在当前 base 上报出 `/api/workspaces/:id`、`/api/workspaces/:id/members` 两个假阳性。
   必须把 method 一起抽出来配对（下列脚本逐字符配平括号后取整条 `method(...)` 链，本轮实测
   在 `8df6888` 上得到 **71 个 `(method, path)` 对、零重复**）：

```python
import re, glob
from collections import defaultdict
pairs = defaultdict(list)
for f in glob.glob("crates/mc-http/src/routes/*.rs"):
    s = open(f).read()
    for m in re.finditer(r'\.route\(\s*"([^"]+)"', s):
        path, i, depth, start = m.group(1), m.end(), 0, m.end()
        while i < len(s):
            if s[i] in '([': depth += 1
            elif s[i] in ')]':
                if depth == 0: break
                depth -= 1
            i += 1
        for meth in (re.findall(r'\b(get|post|put|patch|delete|head|options|any)\s*\(', s[start:i]) or ['?']):
            pairs[(meth.upper(), path)].append(f.split('/')[-1])
dups = {k: v for k, v in pairs.items() if len(v) > 1}
print(f"(method, path) = {len(pairs)}; dups = {dups or 'none'}")
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

---

## 9. 运行中断（daemon restart）与被中断切片的状态

**2026-09-22 11:35:34Z daemon 重启**，三个在飞切片（M2-A/LUM-1348、M1-F/LUM-1375、T1/LUM-1376）
被同时判死并同秒自动重试（`error="daemon restarted while task was in flight"`、
`failure_reason=runtime_recovery`、`attempt 2`）。重试 run 拿到的是**全新空工作区**，旧工作区里
未提交的改动（M2-A 有 2h02m 工作量：+4134 −13 + 2 个新文件）需要**主动抢救**，平台不提供指针。

- 完整事实、可复制的抢救命令、必须做的三项校验、以及「不要写别人的 workdir」这条硬规则 →
  **`docs/23-RUN-RECOVERY.md`**（实测于本 cycle）。
- 对集成的影响：§2 表格里 M2-A 的「未提交」状态就是从被中断的旧工作区读到的；
  集成前请以**远端分支 + `docs/23` §3 的哈希对账**为准，别把「只恢复了一半」的分支当完整切片。
- 运维面：切片在跑时重启 daemon 的实测代价 ≈ 2h45m 并行算力，而 M2-A 正在关键路径上。

