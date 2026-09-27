//! 两条 **DEPRECATED** shim 的**文案常量**（`onboarding_shim.go` 的字面量面）——
//! **写者 M9-3** / `LUM-1818`。
//!
//! 拆出来是 `DoD` 第 5 条的预判：上游那两个 handler 623 行里，**约一半是文案**
//! （`onboardingAssistantInstructions` 一段系统提示词、`onboardingIssueDescription`、
//! EN/ZH 两份 no-runtime 安装指南）。把它们单独成文件，`shim.rs` 就只留控制流。
//!
//! # 🔴 这些常量是**契约**，不是文案
//!
//! - [`NO_RUNTIME_ISSUE_TITLE`] **必须**与 pre-v3 的 service 常量逐字一致 —— 上游注释：
//!   「MUST match the pre-v3 service constant so `LockAndFindActiveDuplicate` dedupes
//!   correctly across desktop versions.」⇒ 改一个字符，去重就跨版本失效。
//! - [`ONBOARDING_ISSUE_TITLE`] 同理是去重的键。
//! - [`ONBOARDING_ASSISTANT_NAME`] 是「找或建 Helper agent」的匹配键。
//!
//! 因此本文件的用例是**逐字比对**（不是「非空」），见 `tests.rs` 的文案面。

/// 助手 agent 名（上游 `onboardingAssistantName`）。
pub const ONBOARDING_ASSISTANT_NAME: &str = "Multica Helper";

/// 助手 agent 的描述（上游 `onboardingAssistantDescription`）。
pub const ONBOARDING_ASSISTANT_DESCRIPTION: &str =
    "Built-in workspace assistant. Answers Multica questions and runs CLI operations.";

/// 助手 agent 的头像（上游 `onboardingAssistantAvatarURL`，逐字的内联 SVG data URL）。
pub const ONBOARDING_ASSISTANT_AVATAR_URL: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 128 128'%3E%3Cdefs%3E%3ClinearGradient id='t' x1='0' y1='0' x2='0' y2='1'%3E%3Cstop offset='0%25' stop-color='%2323242C'/%3E%3Cstop offset='100%25' stop-color='%2313141A'/%3E%3C/linearGradient%3E%3C/defs%3E%3Crect width='128' height='128' rx='28' fill='url(%23t)'/%3E%3Cg stroke='%23FFFFFF' stroke-width='13' stroke-linecap='round'%3E%3Cline x1='64' y1='32' x2='64' y2='96'/%3E%3Cline x1='32' y1='64' x2='96' y2='64'/%3E%3Cline x1='41.4' y1='41.4' x2='86.6' y2='86.6'/%3E%3Cline x1='86.6' y1='41.4' x2='41.4' y2='86.6'/%3E%3C/g%3E%3C/svg%3E";

/// 助手 agent 的系统提示词（上游 `onboardingAssistantInstructions`）。
///
/// 上游注释逐字：pre-v3 desktop 提交的 `starter_prompt` 会成为 **issue 正文**，
/// 而**这一段**成为 agent 在 `CLAUDE.md` / `AGENTS.md` / `GEMINI.md` 里的身份块。
pub const ONBOARDING_ASSISTANT_INSTRUCTIONS: &str = r#"You are Multica Helper, the built-in AI assistant for this Multica workspace. Your role is to help any member use Multica better — answer questions, give advice, and execute workspace operations on their behalf.

## What Multica is

Multica is an open-source, AI-native team workspace (source: https://github.com/multica-ai/multica). The core idea: AI agents are treated as real teammates — they get assigned issues on a kanban-style board, comment in threads, change status, and run code, exactly like human members. You can also chat directly with agents (chat), group them into squads, and run scheduled or triggered automation (autopilot).

For concept details (workspace / issue / project / agent / runtime / skill / squad / autopilot / inbox / chat session): fetch https://multica.ai/docs via WebFetch — that's authoritative. For the "why" or implementation, fetch the GitHub repo above. Never paraphrase concepts from memory.

For ANY product-usage problem the user runs into (bug, unclear behavior, missing feature, improvement idea), suggest they file an issue at https://github.com/multica-ai/multica/issues — that's the official feedback channel.

## What you can do

Your toolbox is the `multica` CLI. It's already on your PATH and authenticated as the workspace owner.

Your full capability surface = whatever `multica --help` shows. Run `multica --help` first, then `multica <command> --help` for any subcommand; use `--output json` for structured data. The CLI is your manifest — never invent commands or flags.

A few things you can actually do (non-exhaustive — `--help` is the source of truth):
- Create issues, post comments
- Create or iterate on agents
- Manage projects, squads, autopilots, skills, runtimes, etc.

## Tone

Be concise and direct, like a colleague. Respond in the user's language (Chinese in, Chinese out). When pointing at a UI location, name the exact path ("Settings → Agents → New"); when pointing at a doc, link to the specific page, not the homepage. Never fabricate URLs, flags, or file paths."#;

/// runtime-bootstrap 那条 starter issue 的标题（上游 `onboardingIssueTitle`）。
pub const ONBOARDING_ISSUE_TITLE: &str = "Start here: learn Multica with Multica Helper";

/// runtime-bootstrap 那条 starter issue 的**默认**正文（上游 `onboardingIssueDescription`）。
///
/// ⚠️ 客户端提交了非空 `starter_prompt` 时，正文被**整个替换**成那个 prompt
/// （shim.rs 的那一格）。
pub const ONBOARDING_ISSUE_DESCRIPTION: &str = "Welcome to Multica.

This is your guided first run. Multica Helper is assigned to this issue and will help you try the core workflow:

1. Read Multica Helper's first comment.
2. Reply with something you want to build, fix, write, or plan.
3. @mention Multica Helper when you want it to continue.
4. Open Agents and Runtimes later when you want to customize the teammate or the computer it runs on.

You can close this issue when the workflow makes sense.";

/// no-runtime-bootstrap 那条 guide issue 的标题（上游 `noRuntimeIssueTitle`）。
///
/// 🔴 **必须**与 pre-v3 的 service 常量逐字一致（去重跨版本生效的前提，见本文件模块头）。
pub const NO_RUNTIME_ISSUE_TITLE: &str = "Connect a runtime to start using agents";

/// no-runtime 那条 issue 的**英文**正文（上游 `enNoRuntimeIssueDescription`）。
pub const NO_RUNTIME_ISSUE_DESCRIPTION_EN: &str = "Welcome to Multica.

Agents need a runtime before they can execute work. You can still use Multica as a lightweight project-management workspace while you install one.

## Try Multica first

Before the runtime is ready, you can:

1. Create a project for your current work.
2. Create a few issues and move them across backlog, todo, in_progress, and done.
3. Add priorities, labels, comments, and subscriptions.
4. Use Inbox to track assignments and mentions.

That gives you the project-management layer first. Once a runtime is connected, agents can start working from the same issues.

## Install your first agent runtime

Full guide: https://multica.ai/docs/install-agent-runtime

For English users, the fastest first path is Codex:

1. Make sure Node.js is installed.
2. Install Codex:
   npm i -g @openai/codex
3. Sign in:
   codex
4. Confirm your terminal can find it:
   which codex
   codex --version
5. Restart the Multica daemon:
   multica daemon restart
   If you use the desktop app, restarting the app is enough.
6. Return to Runtimes and refresh. You should see a Codex runtime online.
7. Create your first agent from that runtime, then assign an issue to the agent and set status to todo.

Codex reference: https://developers.openai.com/codex/cli

When the runtime is connected, you can create Multica Helper for a guided first run.";

/// no-runtime 那条 issue 的**中文**正文（上游 `zhNoRuntimeIssueDescription`）。
pub const NO_RUNTIME_ISSUE_DESCRIPTION_ZH: &str = "欢迎来到 Multica。

智能体需要先连上运行时才能执行工作。运行时还没准备好时，你也可以先把 Multica 当作轻量项目管理工具体验起来。

## 先体验项目管理功能

运行时安装前，你可以先做这些事：

1. 为当前工作创建一个项目。
2. 新建几个 issue，并在 backlog、todo、in_progress、done 之间流转。
3. 给 issue 加优先级、标签、评论和订阅。
4. 用收件箱追踪分配给你的事项和 @mention。

这样你先熟悉项目管理层。连上运行时后，智能体会直接在这些 issue 上开始工作。

## 安装第一个 Agent 运行时

完整文档：https://multica.ai/docs/install-agent-runtime

中文用户建议先装 Kimi CLI：

1. 在 macOS / Linux 终端安装 Kimi CLI：
   curl -LsSf https://code.kimi.com/install.sh | bash
   Windows PowerShell：
   Invoke-RestMethod https://code.kimi.com/install.ps1 | Invoke-Expression
2. 确认终端能找到 Kimi：
   kimi --version
3. 在你想让 Kimi 工作的项目目录里启动一次：
   kimi
4. 首次启动后输入 /login，按提示完成 Kimi Code 或 API key 配置。
5. 重启 Multica 守护进程：
   multica daemon restart
   如果你用桌面端，重启 app 即可。
6. 回到 Runtimes 页面刷新。你应该能看到一个在线的 Kimi 运行时。
7. 用这个运行时创建第一个智能体，再把一个 issue 分配给它，并把状态切到 todo。

Kimi CLI 官方文档：https://moonshotai.github.io/kimi-cli/zh/guides/getting-started.html

运行时连上后，你就可以创建 Multica Helper，开始一次有智能体参与的上手引导。";

/// 上游 `noRuntimeIssueDescription(language)`：语言以 `zh` 开头 ⇒ 中文，否则英文。
///
/// 上游逐字注释：「ZH selected on any `zh*` prefix (zh, zh-CN, zh-Hans).」
#[must_use]
pub fn no_runtime_issue_description(language: Option<&str>) -> &'static str {
    match language {
        Some(lang) if lang.starts_with("zh") => NO_RUNTIME_ISSUE_DESCRIPTION_ZH,
        _ => NO_RUNTIME_ISSUE_DESCRIPTION_EN,
    }
}
