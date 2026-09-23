# 33 · M3-8 adapters 台账（25 项白名单）

本文件由 **M3-8 三批共同持有**（`docs/37` §3.4 的编号登记：批 1 = `LUM-1441` 建立主结构，
批 2 = `LUM-1442`、批 3 = `LUM-1443` 续写各自章节与同一张定族表）。每批落地后**必须**：
① 把该批 provider 的「协议族」列从「待定」改成实定值并写明依据；② 追加一节「批 N 落地记录」。

- 权威计划：`docs/15-M3-PLAN.md` §6（M3-8）、`docs/18-M3-RUNTIME-ADAPTER.md`（契约与 6 步配方）。
- 预飞实测：`docs/37-M3-W3C-PREFLIGHT.md` §5（25 项族直方图、共享设施、单位成本）。
- 上游冻结 commit：**`f41fae6b08fb`**（`docs/37` §5.5 的协议冻结 SHA；本片引用到的每个上游
  行号都在这个 commit 上复核过，复算方式见 §8.4）。
- 批 1 基线：`feat/multica-rs-initial` @ **`e4ee275`**（`multica repo checkout` 自动建的工作分支起点）。

## 1. 状态台账（25 项一张表）

「族」列 = 本仓 `AgentType::protocol_family()`（`crates/mc-runtime/src/catalog.rs`）的取值；
「依据」列 = 该判定**可复查的证据**，不是印象。

| # | `AgentType` | key | launch header | 族 | 依据 | 状态 |
| ---: | --- | --- | --- | --- | --- | --- |
| 1 | `Claude` | `claude` | `claude (stream-json)` | `StreamJson` | 冻结规则（骨架里的 `(stream-json)`） | **批 1 已交付** |
| 2 | `Codebuddy` | `codebuddy` | `codebuddy (stream-json)` | `StreamJson` | 同上 | **批 1 已交付** |
| 3 | `Codex` | `codex` | `codex app-server` | `AppServer` | 冻结规则（`app-server`） | **批 1 已交付** |
| 4 | `Copilot` | `copilot` | `copilot (json)` | `JsonLine` | 批 1 实测：stdout 是**一行一个** `{type,data,…}` 信封（`copilot.go` 的 `copilotEvent`） | **批 1 已交付** |
| 5 | `Opencode` | `opencode` | `opencode run (json)` | `JsonLine` | 批 1 实测：NDJSON（`type`/`sessionID`/`part`） | **批 1 已交付** |
| 6 | `Codearts` | `codearts` | `codearts run (json)` | `JsonLine` | 同 opencode（派生 CLI，schema 逐字段相同） | **批 1 已交付** |
| 7 | `Deveco` | `deveco` | `deveco run (json)` | `JsonLine` | 同 opencode | **批 1 已交付** |
| 8 | `Openclaw` | `openclaw` | `openclaw agent (json)` | `Opaque` | **待批 2/3 定**（`(json)` 只是输出开关，未证实是逐行 JSON） | 待定 |
| 9 | `Hermes` | `hermes` | `hermes acp` | `Acp` | 冻结规则 | 待批 3 |
| 10 | `Pi` | `pi` | `pi (json mode)` | `JsonLine` | M3-2 已落地（`pi -p --mode json`） | 已完成（批 1 回归确认） |
| 11 | `Cursor` | `cursor` | `cursor-agent (stream-json)` | `StreamJson` | 冻结规则 | 待批 2 |
| 12 | `Kimi` | `kimi` | `kimi acp` | `Acp` | 冻结规则 | 待批 2 |
| 13 | `Reasonix` | `reasonix` | `reasonix acp` | `Acp` | 冻结规则 | 待批 3 |
| 14 | `Dsh` | `dsh` | `dsh --profile multica (stdio)` | `Opaque` | **待批 2/3 定**（`(stdio)` 没说是 ACP 还是自有 JSON） | 待定 |
| 15 | `Kiro` | `kiro` | `kiro-cli acp` | `Acp` | 冻结规则 | 待批 2 |
| 16 | `Antigravity` | `antigravity` | `agy -p (non-interactive)` | `Opaque` | **待批 2/3 定**（`-p` 是单次问答形态，无逐行 JSON 证据） | 待定 |
| 17 | `Qoder` | `qoder` | `qodercli --acp` | `Acp` | 冻结规则 | 待批 2 |
| 18 | `QoderCliCn` | `qoderclicn` | `qoderclicn --acp` | `Acp` | 冻结规则 | 待批 2 |
| 19 | `TraeCli` | `traecli` | `traecli acp serve` | `Acp` | 冻结规则 | 待批 2 |
| 20 | `Grok` | `grok` | `grok agent stdio` | `Opaque` | **待批 2/3 定** | 待定 |
| 21 | `Qwen` | `qwen` | `qwen -p (stream-json)` | `StreamJson` | 冻结规则 | 待批 3 |
| 22 | `QwenPaw` | `qwenpaw` | `qwenpaw acp` | `Acp` | 冻结规则 | 待批 3 |
| 23 | `Mcode` | `mcode` | `mcode acp` | `Acp` | 冻结规则 | 待批 3 |
| 24 | `Dim` | `dim` | `dim acp` | `Acp` | 冻结规则 | 待批 3 |
| 25 | `Zeroclaw` | `zeroclaw` | `zeroclaw acp` | `Acp` | 冻结规则 | 待批 3 |

直方图（批 1 后）：**`Acp` 11 + `StreamJson` 4 + `JsonLine` 5 + `AppServer` 1 + `Opaque` 4 = 25**。
`catalog.rs` 的测试 `protocol_family_covers_all_25_with_a_documented_split` 把这份直方图**钉死**：
批 2/3 每定族一项，就要同步改那个断言与本表（直方图变了而测试没变 = 门禁红）。

**为什么"待定"不能留 `Opaque` 交差**：`Opaque` 是兜底语义（"不知道"），而 M3-7 的 hub 能力协商与
M3-4 的 runtime 台账都会按这条读族 —— 读到错族比读到"未知"更糟（`docs/37` §5.1 的结论）。
批 1 的 4 项就是因为这条才必须去读上游 provider 代码定族，而不是照抄 launch header。

## 2. 批 1 交付物

清单（`docs/15` §6 名单 8 项，其中 `pi` 由 M3-2 交付 ⇒ 本批**新增 7 个 adapter**）：

| 文件 | 行数 | 职责 |
| --- | ---: | --- |
| `src/adapters/cli_core/mod.rs` | 441 | 共享 CLI 核：`CliProvider` trait + `impl<P: CliProvider> RuntimeAdapter`、`CliSpec`、`PromptTransport`、`CliCapabilities`、`launch()`（spawn + 三根管道 + 取消槽） |
| `src/adapters/cli_core/args.rs` | 272 | `ArgPolicy`（`blocked` / `modes` / `strip_prompt_like`）与 `filter_extra_args`；`push_flag_value` |
| `src/adapters/cli_core/run.rs` | 630 | 单次 run 的状态机：读 stdout/stderr、超时、取消、收尾与终态归因 |
| `src/adapters/cli_core/decoder.rs` | 350 | `DecoderState`（`output` 累加 / session / usage 表 / 错误）与 `tokens`/`json_u64` 取数工具 |
| `src/adapters/claude_family.rs` | 374 | `claude` / `codebuddy` **共用**的 stream-json 解码器（上游这两个 provider 的 SDK 消息结构体逐字段一致） |
| `src/adapters/opencode_family.rs` | 340 | `opencode` / `codearts` / `deveco` **共用**的 NDJSON 解码器（可切 `strict`） |
| `src/adapters/claude/mod.rs` | 259 | claude argv + spec + 测试 |
| `src/adapters/codebuddy/mod.rs` | 254 | codebuddy argv + spec + 测试 |
| `src/adapters/codex/mod.rs` | 251 | codex argv（`app-server --listen stdio://`）+ spec + 测试 |
| `src/adapters/codex/stream.rs` | 664 | codex 的 **JSON-RPC 客户端**：握手帧、`thread/start`、`turn/start`、`turn/interrupt`、通知解码 |
| `src/adapters/copilot/mod.rs` | 283 | copilot argv + spec + 测试 |
| `src/adapters/copilot/stream.rs` | 495 | copilot 信封解码 + **三来源择一**的 usage 口径 |
| `src/adapters/opencode/mod.rs` | 260 | opencode argv + spec + 测试 |
| `src/adapters/codearts/mod.rs` | 252 | codearts argv + spec + 测试 |
| `src/adapters/deveco/mod.rs` | 266 | deveco argv + spec + 测试 |
| `tests/cli_adapters.rs` | 368 | 9 条跨 adapter 集成测试（注册表 / 族 / argv / stdin / 非零退出 / 取消幂等） |

改动的既有文件（都在写集内）：

| 文件 | 改了什么 |
| --- | --- |
| `src/adapters/mod.rs` | 新增 `pub fn builtin_adapters() -> Vec<Arc<dyn RuntimeAdapter>>`（按白名单顺序的 8 个实例）+ `pub mod` 登记 |
| `src/registry.rs` | `with_builtin_adapters()` 改为遍历 `builtin_adapters()`（不再逐个 `use` provider 类型） |
| `src/catalog.rs` | 新增 `AgentType::protocol_family()`（25 项全覆盖）+ 直方图测试 |
| `src/lib.rs` | re-export 批 1 的类型 + 一行「注册表覆盖本批」的测试 |
| `src/conformance.rs` | `FakeCli` 新增 `live_*` 假 CLI（见 §7.2），**协议契约（`adapter.rs`/`traits.rs`）未动** |
| `docs/33-M3-ADAPTERS.md` | 本文件（新建） |

**注册表设计**：`builtin_adapters()` 放在 `adapters/mod.rs`（provider 列表的归属地），
`registry.rs` 只消费它 —— 这样"可注册的 provider"与"已实现的 provider"结构上不会两处分叉；
批 2/3 加 adapter 只需要在 `builtin_adapters()` 里加一行 + 在本文件 §1 表里改状态。

## 3. 批 1 逐 provider 契约（argv / 传输 / 解码 / 屏蔽表）

`BLOCKED` 一列全部**逐条对齐上游** `*BlockedArgs`（实测：本批 7 项与上游 map 的键集合完全相同）；
`argv` 一列是**上游 `build*Args` 的骨架**，`[…]` 表示按需出现的可选段。

| provider | argv 骨架 | prompt 传输 | 解码器 | `BLOCKED`（键） | 上游出处 |
| --- | --- | --- | --- | --- | --- |
| `claude` | `-p --output-format stream-json --input-format stream-json --verbose --permission-mode bypassPermissions --disallowedTools AskUserQuestion [--model M] [--effort L] [--resume R]` | stdin **JSON 信封**（`--input-format stream-json`） | `claude_family` | `-p` `--output-format` `--input-format` `--permission-mode` `--mcp-config` `--effort` | `claude.go` `buildClaudeArgs`(L727) / `claudeBlockedArgs`(L715) |
| `codebuddy` | 同 claude，但 `--disallowedTools AskUserQuestion EnterPlanMode ExitPlanMode` | stdin JSON 信封 | `claude_family` | 同 claude | `codebuddy.go` L38 / L27 |
| `codex` | `app-server --listen stdio://` | **JSON-RPC over stdio**（`initialize`→`initialized`→`thread/start`→`turn/start`） | `codex::stream` | `--listen` | `codex.go` L362 / L33 |
| `copilot` | `-p <prompt> --output-format json --allow-all --no-ask-user [--model M] [--resume R]` | **argv**（`-p` 的值） | `copilot::stream` | `-p` `--output-format` `--allow-all` `--allow-all-tools` `--allow-all-paths` `--allow-all-urls` `--yolo` `--no-ask-user` `--resume` `--acp` | `copilot.go` `buildCopilotArgs`(L630) / `copilotBlockedArgs`(L613) |
| `opencode` | `run --format json --dangerously-skip-permissions [--dir C] [--model M] [--variant L] [--session S]` | stdin 文本 | `opencode_family`（`strict`） | `--format` `--dir` `--variant` `--dangerously-skip-permissions` | `opencode.go` L95-141 |
| `codearts` | `run --format json --auto [--model M] [--session S]`（**无** `--dir` / `--variant`） | stdin 文本 | `opencode_family`（`strict`） | `--format` `--auto` `--sandbox` `--dir` `--variant` `--dangerously-skip-permissions` | `codearts.go` L82-100 / L38 |
| `deveco` | `run --format json --dangerously-skip-permissions [--dir C] [--model M] [--variant L] [--session S] <prompt>` | **argv**（最末位置参数） | `opencode_family`（非 strict） | `--format` `--dir` `--variant` `--dangerously-skip-permissions` | `deveco.go` L94-120 |

每个 adapter 的 `capabilities()` 由 `CliCapabilities` + `kind.launch_header()` 合成：

| provider | `protocol` | `streaming` | `thinking` | `tool_events` | `usage_reporting` | `resume` |
| --- | --- | --- | --- | --- | --- | --- |
| claude / codebuddy | `StreamJson` | ✓ | ✓ | ✓ | ✓ | ✓ |
| codex | `AppServer` | ✓ | **✗** | ✓ | ✓ | ✓ |
| copilot | `JsonLine` | ✓ | ✓ | ✓ | ✓ | ✓ |
| opencode / codearts / deveco | `JsonLine` | ✓ | **✗** | ✓ | ✓ | ✓ |

三处**如实自报 `false`** 的地方（宁可自报窄一点，也不让协商层拿到假能力）：

1. `codex.thinking`：`app-server` 的通知里没有推理增量（上游 `codex.go` 里没有 `MessageThinking`）。
2. `opencode`/`codearts`/`deveco.thinking`：`--variant` 只是"推理等级"旋钮，NDJSON 里没有推理事件。
3. `codearts.thinking` 还有一个额外事实：上游**忽略** `ThinkingLevel`（CodeArts 没这个旋钮），
   所以本 adapter 也不拼 `--variant`。

### 3.1 非显然处的逐条说明

- **`--disallowedTools` 一次一个值**（codebuddy）：上游注释实测
  `PermissionUtils.matchPermissionRules` 是**精确匹配**，逗号串一个都匹配不上 ——
  所以 `AskUserQuestion EnterPlanMode ExitPlanMode` 必须写成三个独立 argv 元素。
- **`-p` 的两重语义**：claude/codebuddy 里 `-p`（print 模式）是**无值**开关 →
  `ArgValueMode::Standalone`；copilot 里 `-p` **要值**（prompt 本身）→ `ArgValueMode::WithValue`。
  这是同一个 flag 字面量在不同 provider 下的不同解释，`ArgValueMode` 就是为这种分歧存在的。
- **`--dir` 的归属**：opencode/deveco 支持 `--dir <cwd>`；codearts **不支持**（上游注释：
  靠 `cmd.Dir` + `PWD` 锚定）⇒ codearts 的 `BLOCKED` 里也钉了 `--dir`（用户塞进来会打到一个不认的 flag）。
- **`--session` vs `--resume`**：opencode 家族用 `--session`，claude 家族/copilot 用 `--resume`，
  codex 用 JSON-RPC 的 `thread/resume`（`resume_session` 有值时；无值走 `thread/start`）。
- **codex 的帧必须以 `\n` 结尾**：`app-server` 的 stdin 是 **newline-delimited** JSON-RPC，
  上游 `request`/`notify` 写完都补 `\n`（`codex.go` L2657 / L2706 / L2717 / L2731 四处）。
  本片初版漏了 `\n`，一致性套件里表现为"假 CLI 永远等不到完整报文"→ **握手死锁**。
  任何 JSON-RPC/NDJSON 写方向都要照抄这条。

## 4. 共享 CLI 核（`cli_core`）与 `docs/18` §4 的偏离

`docs/18` §4 的 6 步配方第 2 步写的是「建目录 `src/adapters/<name>/{mod.rs,args.rs,run.rs,stream.rs}`」。
批 1 **没有**照抄这个形态，而是：

- 新增 `src/adapters/cli_core/{mod,args,run,decoder}.rs` = **7 个 provider 共用的 CLI 核**；
- 每个 provider 目录**只留 `mod.rs`**（argv + `CliSpec` + 测试），只有解码器不能共用的两个
  （`codex`、`copilot`）才各有一个 `stream.rs`；
- `claude`/`codebuddy` 共用 `claude_family.rs`；`opencode`/`codearts`/`deveco` 共用 `opencode_family.rs`。

**理由**：7 个 provider 的"spawn + 三根管道 + 超时 + 取消 + 收尾归因 + 逐行喂解码器"逻辑是同一份；
照抄 7 遍不仅多 7 份代码，还会让"取消语义/终态归因"这种容易出错的规则出现 7 个漂移点。
偏离的代价是 `pi_local` 与本核**并存**（`pi_local` 有自己的 `run.rs`/`stream.rs`/会话锁）：

- **为什么不把 `pi_local` 也迁到 `cli_core`**：本片的任务边界是"不改 M3-2 的公开契约 + 不碰 `pi_local`"，
  而迁移会同时改到 `pi_local` 的运行期行为（它有自己的会话文件锁与 `TextDrain` 消毒），
  风险与收益不对等。⇒ **两个 CLI 核并存是本片的已知技术债**，登记给 M4 统一（见 §6.4）。
- **为什么 `pi_local` 的 `TextDrain` 消毒不套用到新 provider**：那是 pi 自己的控制 token 协议，
  上游其它 provider 的流里没有这类 token；照搬会误清正文。

本核的内部约定（批 2/3 复用时要遵守）：

1. `PromptTransport` 四态：`StdinText` / `StdinJsonEnvelope` / `Argv` / `JsonRpc`；**没有 `StdinNone`**。
2. `ArgPolicy.strip_prompt_like`：prompt 走 argv 的 provider（copilot/deveco）**必须** `false`
   （否则用户传的裸位置参数会被当 prompt 剔掉）。
3. `prompt_write_is_fatal`：stdin 传输默认 `true`；`JsonRpc` 与 argv 传输为 `false`
   （对端先退出导致 EPIPE 是常态，**退出码才是权威**）。
4. 默认值：`DEFAULT_TIMEOUT = 2h`、`DEFAULT_DRAIN_GRACE = 1s`、`DEFAULT_VERSION_PROBE_TIMEOUT = 10s`；
   版本探测一律 `--version`。
5. `cwd`：`LaunchRequest::cwd` 有值时，核会 `current_dir(cwd)` **并**在环境里覆盖 `PWD`
   （对齐上游 `cmd.Dir` + `PWD` 的锚定方式；有些 CLI 只读 `PWD`）。
6. 取消槽是**全局**的（`static SLOTS: OnceLock<Mutex<HashMap<RunId, watch::Sender<bool>>>>`），
   不是 per-instance；同一 `RunId` 的第二次 `cancel()` 幂等（`NotRunning`）。

## 5. 终态归因与取消顺序（实现契约，不是可选风格）

`CliRun::finalize` 的判定顺序（**顺序即语义**）：

1. 协议层 `terminal_error` 先给状态打底；
2. `timed_out` → `Timeout`；
3. `cancelled` → 有协议错时 `Failed`，否则 `Cancelled`；
4. 否则 `Completed`：`wait_error` / `write_error` / 协议错 任一存在 → `Failed`。

`FailureReason` 映射：`Completed → None`、`Failed → AgentError`、`Timeout → Timeout`、`Cancelled → Manual`。

**取消落地顺序**（本片实测后定死，批 2/3 复用）：

1. 通过 outbox 发出协议自有的取消帧（codex = `turn/interrupt`）；
2. `stream_drain_grace.min(200ms)` 的短暂等待（**仅当**确实有取消帧要发）；
3. drop outbox；
4. `child.start_kill()`；
5. 有界地读 stdout 到 EOF（上限 `stream_drain_grace`）；
6. `finish()` + `summary()` + trace。

**必须按这个顺序**的原因：`opencode`/`codearts` 这类 **fail-closed** 解码器只在流被截断时报错。
取消时若先 kill 再把半截流喂给它，"用户主动取消"就会变成 `Failed/AgentError`（假失败）。
先发取消帧、给 CLI 一个把流收尾的机会，是让 `Cancelled` 语义成立的前提。
**这条有可观察后果**：在取消前只回放了**半截**事件流（没有终态行）的 run，取消后仍然是 `Failed`
—— `tests/cli_adapters.rs` 的取消用例因此回放**完整**流（含 `step_finish`）后才取消，
这是刻意的：截断流被判失败是 fail-closed 解码器的**正确行为**，不是 bug。

**2026-09-23 08:30 补一条实现约束（集成 cycle 实测）**：「取消前先把终态行喂进去」在**假 CLI 里必须是一次写**。
`FakeCli::replaying_then_sleeping` / `live_script` 原来用 `while IFS= read -r line; do printf '%s\n' "$line"; done < transcript`
逐行回放 —— 那是**多次 write**，于是在「读到首个正文事件就立刻取消」的用例里留下一个负载相关的窗口：
进程被 kill 时终态行可能还没落进管道 ⇒ fail-closed 解码器如实报"流被截断" ⇒ 期望 `Cancelled` 的用例偶发拿到 `Failed`。
实测（本机 32 核 + 20 个 `yes` 占满 CPU）：`cargo test -p mc-runtime --lib conformance_cancel_is_idempotent` **30 次 4 次假红**
（`codearts` / `opencode` 各半，正是两个 fail-closed 解码器）；把回放改成 `cat <transcript>`（一次 write，数据在首个事件可读时
已整段进管道，`run` 的 `BufReader` 会入库，kill 之后的 drain 仍能读完）后**同条件 30/30 绿**。
⇒ 后续给取消类用例写假 CLI 时，回放一律用 `cat`（或任何一次写的形式），不要用 `read` 循环。

## 6. 本批的刻意偏离 / 简化 / 契约缺口

### 6.1 刻意不复制上游的"优化"（都是有意的）

| 项 | 上游做法 | 本片做法 | 影响 |
| --- | --- | --- | --- |
| `output` 正文 | claude 用 `result` 的 `finalResultText`；copilot 每个 turn 重置为"最后一个完整 turn" | **一律 = 已发出的 `Text` 增量拼接** | 与上游的 `output` 字段在"多 turn"场景下可能不同；换来的是 7 个 provider 一致的、可从事件流重建的定义 |
| claude 失败信号 | `is_error` 与 `terminal_reason != "success"` 都算失败 | 失败 = **`is_error`**；`terminal_reason` 非 success 只发一条诊断 `Error` | 非 success 的 `terminal_reason` 不会把 run 判死（更宽） |
| copilot 正文权威源 | 有 `assistant.message` 时整段覆盖增量 | 增量优先；只有**该 turn 完全没有增量**时才用 `assistant.message` | 极端情况下与上游取到的字符串不同（同样只影响 `output`） |
| codex 解码 | 聚合 bytes + 活跃 turn 记账 + 子代理/异线程 `threadId` 守卫 | 每 item 的"已交付前缀"去重；不做聚合合并、不做异线程守卫 | 少了两处防御；当前假 CLI 与真实 `app-server` 的单线程流下行为一致（见 §6.3 登记） |
| codex 服务端请求 | 处理 `app-server` 反向请求 | **忽略**（只记录） | 若上游未来用反向请求做审批，需要补 |

### 6.2 usage 口径（三个 provider 三种，都写在代码注释里）

- `claude`/`codebuddy`：assistant 消息上的 `usage` **累加**；`result` 层的 usage（优先 `modelUsage`）
  **整体替换**用量表（`set_usage_map`）；空模型名/零 token 的条目丢弃。
- `copilot`：三个来源**择一**（`session.shutdown` 仅在**非续跑**时优先 → `assistant.usage` → `assistant.message`），
  也是整体替换；输入口径 = `inputTokens − cacheRead − cacheWrite`（上游 `addUsage`）。
- `opencode`/`codearts`/`deveco`：`step_finish.part.tokens` 累加；
  `total = input + output + cacheRead + cacheWrite`。

### 6.3 显式登记的简化（批 2/3 或 M4 可收敛）

1. **opencode v2 契约未处理**：只实现 1.x 的 argv（`run --format json …`）。
2. **claude 的 `control_request` 不应答**：上游有 ask/answer 通道，本片不实现（无 UI 可渲染，见
   `--disallowedTools AskUserQuestion` 的理由）；流里出现 `control_request` 会被当成未知行忽略。
3. **arg 过滤与 `pi_local/args.rs` 有一份重复**（`pi_local::args` 是私有模块，本批没有为了复用去改它）。
4. 上表中 codex 的三处解码简化。
5. **codex 握手不做逐请求超时/重试**：上游给 `initialize` / `thread/start` 各有握手超时，
   并且在 `thread/resume` 失败时**回退**到 `thread/start`；本片只靠 run 级 timeout 兜底。

### 6.4 契约承载不了的上游字段（**不是**本片的选择，是 M3-2 契约的缺口）

`LaunchRequest`（`adapter.rs`，M3-2 冻结）只有
`prompt/cwd/model/thinking_level/resume_session/timeout/env/extra_args`。因此**无法**传递上游的：

| 上游字段 | 影响的 provider | 结果 |
| --- | --- | --- |
| `MaxTurns` | `claude`（`--max-turns`） | 该 flag 不拼；上限只能由 `timeout` 兜 |
| `McpConfig` / `ClaudeSettingsPath` | `claude`/`codebuddy`（`--mcp-config`/`--strict-mcp-config`/`--settings`） | 不拼；同时把这些 flag **钉进 `BLOCKED`**，避免用户从 `extra_args` 塞一个 daemon 管不到的值 |
| `SystemPrompt` | `claude`（`--append-system-prompt`） | 上游本来就"刻意不转发"（用 workdir 的 `CLAUDE.md`），与本片一致 ✓ |

⇒ 若 M4 要给 adapter 传 MCP 配置 / 轮数上限，正确做法是**扩 `LaunchRequest`**（一次契约变更），
而不是让 adapter 去读环境变量。本片不做该变更（任务边界：不改 M3-2 公开契约）。

## 7. 测试与门禁证据

### 7.1 计数

```
cargo test -p mc-runtime
  lib              192 passed / 0 failed / 0 ignored      （其中一致性套件 64 = 8 adapter × 8）
  tests/cli_adapters.rs    9 passed / 0 failed
  tests/pi_local_e2e.rs    7 passed / 0 failed            （M3-2 的 7 条 e2e 回归全绿）
  doc-tests        0 passed / 2 ignored（原有）
```

新增模块贡献的 lib 测试 **119 条**：`cli_core` 16 + `claude_family` 5 + `opencode_family` 5 +
7 个 provider 模块 93（claude 11 / codebuddy 11 / codex 20 / copilot 20 / opencode 10 / codearts 10 / deveco 11），
其中含 **7 × 8 = 56 条**一致性套件（`crate::adapter_conformance!(X)`）+ `pi` 的 8 条。
另有 `tests/cli_adapters.rs` 的 **9 条**跨 adapter 集成测试。

**`#[ignore]` 约定**：本批**没有新增任何 `#[ignore]`**（`docs/37` §5.4 说的"缺真二进制就 skip"
这条没有成为首例）—— 全部用例都跑在 `FakeCli` 生成的假 CLI 上，机器上不需要装
`claude`/`codex`/`copilot`/…；`mc-runtime` 的 2 条 ignored 是**原有**的 doc-test，与本批无关。

### 7.2 `FakeCli` 的两点扩展（在 `conformance.rs`，协议契约未动）

1. 新增 `live_*` 家族：`live_script` / `live_replaying` / `live_failing` / `live_replaying_then_sleeping`。
   它们是 **内容门控**（foreground `read_gate` 状态机：循环从 stdin 读行、追加到 `stdin.txt`，
   读到包含门字符串的行就继续，EOF 就退出）而不是后台 `cat` + 轮询 sleep。
   原因（实测记录）：`app-server`/长驻 CLI 的 stdin 不会 EOF，普通 `cat` 假 CLI 永远等不到流结束；
   而用后台 `cat` 抓 stdin 在 `dash` 下会掉进"异步列表的 stdin 被接到 `/dev/null`"的坑。
2. 假 CLI 仍然捕获 `argv.txt` / `stdin.txt`（`recorded_argv()` / `recorded_stdin()`），
   集成测试据此断言"prompt 到底走 argv 还是 stdin"。
3. **两道门是必须的**（`live_replaying(transcript, after, until)`）：若进程在 adapter 写出带
   prompt 的那帧之前就退出，写端会拿到 EPIPE，`stdin.txt` 里就看不到 prompt，
   "prompt 必须走 stdin"的断言会**随机**失败 ⇒ 回放后要再等一道 `until` 才退出。
4. **脚本不由本进程直接写**（`std::fs::write`）：测试是多线程跑的，别的线程在 `fork` 到
   `execve` 之间会复制本进程的 FD；本进程若正持有脚本的写 FD，就会随机得到 `ETXTBSY`
   （Text file busy，实测 25 次连跑挂 2 次）。⇒ 交给子进程写（内容走 stdin）+ `chmod +x` + 等它退出。
   写新假 CLI 的 batch 2/3 请照拄这条。

### 7.3 门禁（`bash scripts/gates.sh`，**不带** `--with-db`，0 库依赖）

```
GATE_FMT_EXIT=0        GATE_BUILD_EXIT=0      GATE_CLIPPY_EXIT=0    GATE_CLIPPY_TEST_UTIL_EXIT=0
GATE_TEST_EXIT=0       GATE_ROUTE_PARITY_EXIT=0                    GATE_CONFORMANCE_EXIT=0
GATE_FILE_SIZE_EXIT=0
（⑤ 工作区合计 681 passed / 0 failed；⑩ 本批最大新文件 664 行 < 800）
```

- **⑦ 计数未变**（0 路由）：`upstream 456 | local 156 registered | baseline 156`、
  `implemented 128 real + 10 placeholder = 138`、`regression 0`；
  `docs/fixtures/route-parity-baseline.json` 与 `crates/mc-conformance/report.json`
  **本片一字未动**（`git diff` 为空）。
- ⑨ 对同一份 `report.json` 比对通过 ⇒ 无漂移。

### 7.4 复算命令

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p mc-runtime                       # lib 192 / cli_adapters 9 / pi_local_e2e 7
bash scripts/gates.sh                          # ①–⑤ + ⑦ + ⑨ + ⑩
python3 scripts/route_parity.py                # ⑦ 的两个计数（本片不应变化）
grep -c 'adapter_conformance!' crates/mc-runtime/src/adapters/*/mod.rs   # 每个 provider 接 1 次
# 上游行号复核（只读 clone，commit 必须是 f41fae6b08fb）：
#   server/pkg/agent/{claude,codebuddy,codex,copilot,opencode,codearts,deveco}.go
```

## 8. 批 2/3 接入指引

1. **先复用 `cli_core`**：`impl CliProvider`（`kind` / `spec` / `config` / `decoder_for`）即可白得
   `impl RuntimeAdapter`、spawn/管道/超时/取消/收尾归因。只有解码器不能共用时才写 `<key>/stream.rs`。
2. **`Acp` 11 项是本批之外的最大成本**（批 2 有 5 项、批 3 有 6 项）：ACP 不是 CLI 文本流，
   建议照 `codex/stream.rs` 的路子做一个 **ACP 核**（JSON-RPC over stdio：握手 → session/new →
   session/prompt → session/cancel），然后 11 项填差异。**先做一项、验完再铺开**
   （`docs/37` §5.3 的建议，本批的 opencode 家族证明了它有效：一份解码器覆盖了 3 个 provider）。
3. **定族要写依据**：把 §1 表里那 4 个 `Opaque` 改成实定值时，必须同时给出"证据列"内容
   （读上游 provider 代码的哪一段 / 实测的哪条命令），并同步改 `catalog.rs` 的直方图断言。
4. **launch header 逐字不许美化**：`AgentType::launch_header()` 是一致性套件的断言对象
   （`caps.launch_header == kind.launch_header()`），也是 M3-7 协商读的值。
5. **`#[ignore]` 若要用**（例如必须真二进制才能验的用例），按 `docs/37` §5.4 的约定在**本文件**登记
   理由，并说明 `FakeCli` 为什么覆盖不了 —— 否则后人会把它当 DB 专用标记。
6. 交付后**续写本文件**：追加「批 N 落地记录」一节（改了什么 / 计数 / 门禁尾部），并更新 §1 状态列。

## 9. 本片没做什么（边界）

- 不做 `execenv`（`LUM-1440`）、不做 daemon 面（M3-7）、不做批 2/3 的类型。
- 不为 adapter 新增路由、不落库、不改迁移、不改 `Cargo.lock`（无依赖 delta）。
- 不改 `docs/16` 的协议冻结常量；不改 M3-2 的 `adapter.rs` / `traits.rs` 公开契约。
- 不改 `pi_local` 的实现（只做回归确认：`pi_local_e2e` 7 条 + 一致性套件 8 条全绿）。
- 不改 `docs/fixtures/route-parity-baseline.json`、不改 `crates/mc-conformance/report.json`
  （0 路由 ⇒ 两个共享快照都不该动；且此刻被其它在审切片改着，避免合并冲突）。
