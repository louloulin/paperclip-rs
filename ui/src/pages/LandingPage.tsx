import { useState } from "react";
import { Link } from "@/lib/router";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import {
  Bot,
  Zap,
  Shield,
  Globe,
  Layers,
  Code2,
  ArrowRight,
  CheckCircle2,
  ChevronDown,
  Sparkles,
  Workflow,
  BrainCircuit,
  Lock,
  Gauge,
} from "lucide-react";

const FEATURES = [
  {
    icon: Bot,
    title: "多 Agent 编排",
    description: "部署 Claude、Codex、Gemini 等多个 Agent，协调完成复杂任务，自动分配工作负载。",
  },
  {
    icon: Workflow,
    title: "自动化工作流",
    description: "用 Routines 构建 Cron 驱动的自动化流程，触发条件精确控制，结果自动归档。",
  },
  {
    icon: BrainCircuit,
    title: "技能工作室",
    description: "用自然语言构建和测试 Agent 技能，无需写代码，直接在可视化界面调试 prompt。",
  },
  {
    icon: Shield,
    title: "企业级安全",
    description: "本地密钥存储、细粒度权限控制、决策签名防篡改，符合企业安全合规要求。",
  },
  {
    icon: Layers,
    title: "插件生态",
    description: "插件化架构，支持 Webhook、API 调用、文件处理等多种扩展，生态持续增长。",
  },
  {
    icon: Gauge,
    title: "实时监控",
    description: "心跳守护、Agent 活跃面板、成本追踪，仪表盘一览全局运行状态。",
  },
];

const STEPS = [
  {
    number: "01",
    title: "连接你的 Agent",
    description: "一键连接 Claude Code、OpenAI Codex、 Gemini 等主流 Agent，支持本地部署和云端 API。",
  },
  {
    number: "02",
    title: "创建任务与工作流",
    description: "用 Issue 描述任务，Routines 定义自动化流程，Skills 积累可复用技能模板。",
  },
  {
    number: "03",
    title: "监控与优化",
    description: "实时查看 Agent 执行日志、成本消耗和决策链路，持续优化你的 AI 工作流。",
  },
];

export function LandingPage() {
  const [email, setEmail] = useState("");
  const [submitted, setSubmitted] = useState(false);

  return (
    <div className="min-h-screen bg-background text-foreground">
      {/* Nav */}
      <header className="sticky top-0 z-50 border-b bg-background/80 backdrop-blur-md">
        <div className="mx-auto flex h-14 max-w-6xl items-center justify-between px-4">
          <div className="flex items-center gap-2">
            <Sparkles className="h-5 w-5 text-primary" />
            <span className="text-sm font-semibold">Lumos</span>
          </div>
          <nav className="hidden items-center gap-6 md:flex">
            <a href="#features" className="text-sm text-muted-foreground hover:text-foreground transition-colors">
              功能
            </a>
            <a href="#how-it-works" className="text-sm text-muted-foreground hover:text-foreground transition-colors">
              工作原理
            </a>
            <a href="#pricing" className="text-sm text-muted-foreground hover:text-foreground transition-colors">
              定价
            </a>
          </nav>
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="sm" asChild>
              <Link to="/auth">登录</Link>
            </Button>
            <Button size="sm" asChild>
              <Link to="/auth?mode=sign_up">开始使用</Link>
            </Button>
          </div>
        </div>
      </header>

      {/* Hero */}
      <section className="relative overflow-hidden px-4 pt-24 pb-20">
        <div className="absolute inset-0 bg-[radial-gradient(ellipse_80%_50%_at_50%_-20%,oklch(0.75_0.15_270/0.08),transparent)]" />
        <div className="relative mx-auto max-w-4xl text-center">
          <div className="mb-4 inline-flex items-center gap-1.5 rounded-full border bg-muted/50 px-3 py-1 text-xs font-medium text-muted-foreground">
            <Sparkles className="h-3 w-3" />
            AI Agent 工作台
          </div>
          <h1 className="text-balance text-4xl font-bold tracking-tight md:text-6xl">
            让 AI Agent
            <br />
            <span className="text-primary">替你工作</span>
          </h1>
          <p className="mt-6 text-balance text-lg text-muted-foreground md:text-xl">
            Lumos 是下一代 AI Agent 编排平台——连接多个 Agent、自动化工作流、
            <br className="hidden md:block" />
            监控执行过程，在一个界面中掌控你的 AI 生产力。
          </p>
          <div className="mt-10 flex flex-col items-center gap-3 sm:flex-row sm:justify-center">
            <Button size="lg" asChild>
              <Link to="/auth?mode=sign_up">
                免费开始
                <ArrowRight className="ml-1.5 h-4 w-4" />
              </Link>
            </Button>
            <Button size="lg" variant="outline" asChild>
              <a href="#features">
                了解更多
              </a>
            </Button>
          </div>
          <p className="mt-4 text-xs text-muted-foreground">
            无需信用卡 · 5 分钟部署 · 支持本地运行
          </p>
        </div>

        {/* Dashboard preview placeholder */}
        <div className="relative mx-auto mt-16 max-w-5xl">
          <div className="overflow-hidden rounded-xl border bg-card shadow-2xl">
            <div className="flex items-center gap-1.5 border-b px-4 py-3">
              <div className="h-3 w-3 rounded-full bg-red-400" />
              <div className="h-3 w-3 rounded-full bg-yellow-400" />
              <div className="h-3 w-3 rounded-full bg-green-400" />
              <span className="ml-2 text-xs text-muted-foreground">Lumos Dashboard</span>
            </div>
            <div className="grid h-72 grid-cols-3 gap-4 bg-muted/30 p-4 md:h-96">
              {/* Metric cards */}
              <div className="col-span-3 grid grid-cols-4 gap-3">
                {[
                  { label: "活跃 Agent", value: "12", trend: "+2" },
                  { label: "今日运行", value: "847", trend: "+18%" },
                  { label: "节省时间", value: "4.2h", trend: "人均" },
                  { label: "成功率", value: "97.3%", trend: "+0.5%" },
                ].map((m) => (
                  <div key={m.label} className="rounded-lg border bg-card p-3 shadow-sm">
                    <div className="text-xs text-muted-foreground">{m.label}</div>
                    <div className="mt-1 text-2xl font-semibold">{m.value}</div>
                    <div className="text-xs text-green-600 dark:text-green-400">{m.trend}</div>
                  </div>
                ))}
              </div>
              {/* Activity feed */}
              <div className="col-span-2 rounded-lg border bg-card p-3 shadow-sm">
                <div className="mb-2 text-xs font-medium text-muted-foreground">最近活动</div>
                {[
                  { agent: "claude-code", task: "生成 API 文档", time: "刚刚" },
                  { agent: "codex", task: "修复登录 Bug", time: "2 分钟前" },
                  { agent: "gemini", task: "数据清洗完成", time: "5 分钟前" },
                ].map((a, i) => (
                  <div key={i} className="flex items-center justify-between border-b py-2 last:border-0">
                    <div className="flex items-center gap-2">
                      <div className="h-5 w-5 rounded-full bg-primary/20" />
                      <span className="text-xs font-medium">{a.agent}</span>
                    </div>
                    <span className="text-xs text-muted-foreground">{a.task}</span>
                    <span className="text-xs text-muted-foreground">{a.time}</span>
                  </div>
                ))}
              </div>
              {/* Agent status */}
              <div className="col-span-1 rounded-lg border bg-card p-3 shadow-sm">
                <div className="mb-2 text-xs font-medium text-muted-foreground">Agent 状态</div>
                {[
                  { name: "Claude Code", status: "在线", color: "bg-green-500" },
                  { name: "Codex", status: "忙碌", color: "bg-yellow-500" },
                  { name: "Gemini Pro", status: "空闲", color: "bg-blue-500" },
                ].map((a, i) => (
                  <div key={i} className="flex items-center justify-between border-b py-2 last:border-0">
                    <span className="text-xs">{a.name}</span>
                    <div className="flex items-center gap-1.5">
                      <div className={cn("h-1.5 w-1.5 rounded-full", a.color)} />
                      <span className="text-xs text-muted-foreground">{a.status}</span>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* Features */}
      <section id="features" className="px-4 py-24">
        <div className="mx-auto max-w-6xl">
          <div className="mb-16 text-center">
            <h2 className="text-3xl font-bold tracking-tight md:text-4xl">
              为 AI 原生团队打造
            </h2>
            <p className="mt-4 text-muted-foreground">
              从单人开发者到千人企业，Lumos 满足各种规模的 AI 工作流需求
            </p>
          </div>
          <div className="grid gap-6 md:grid-cols-2 lg:grid-cols-3">
            {FEATURES.map((feature) => (
              <Card key={feature.title} className="border bg-card transition-colors hover:border-primary/30">
                <CardHeader>
                  <div className="mb-3 inline-flex h-10 w-10 items-center justify-center rounded-lg bg-primary/10">
                    <feature.icon className="h-5 w-5 text-primary" />
                  </div>
                  <CardTitle className="text-lg">{feature.title}</CardTitle>
                </CardHeader>
                <CardContent>
                  <CardDescription>{feature.description}</CardDescription>
                </CardContent>
              </Card>
            ))}
          </div>
        </div>
      </section>

      {/* How it works */}
      <section id="how-it-works" className="border-y bg-muted/30 px-4 py-24">
        <div className="mx-auto max-w-6xl">
          <div className="mb-16 text-center">
            <h2 className="text-3xl font-bold tracking-tight md:text-4xl">
              3 步启动 AI 生产力
            </h2>
            <p className="mt-4 text-muted-foreground">
              从注册到第一个自动化工作流，不超过 10 分钟
            </p>
          </div>
          <div className="grid gap-8 md:grid-cols-3">
            {STEPS.map((step, i) => (
              <div key={step.number} className="relative">
                {i < STEPS.length - 1 && (
                  <div className="absolute -right-4 top-8 hidden h-0.5 w-8 bg-border md:block" />
                )}
                <div className="text-5xl font-bold text-muted-foreground/30">{step.number}</div>
                <h3 className="mt-3 text-xl font-semibold">{step.title}</h3>
                <p className="mt-2 text-muted-foreground">{step.description}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* CTA / Email capture */}
      <section className="px-4 py-24">
        <div className="mx-auto max-w-xl text-center">
          <h2 className="text-3xl font-bold tracking-tight md:text-4xl">
            准备好提升团队效率了吗？
          </h2>
          <p className="mt-4 text-muted-foreground">
            加入 Lumos，立即开始构建你的 AI 工作流。
            <br />
            我们会在产品正式发布时第一时间通知你。
          </p>
          {submitted ? (
            <div className="mt-8 flex items-center justify-center gap-2 text-green-600 dark:text-green-400">
              <CheckCircle2 className="h-5 w-5" />
              <span className="font-medium">已收到，我们会尽快联系你！</span>
            </div>
          ) : (
            <form
              className="mt-8 flex gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                if (email.trim()) setSubmitted(true);
              }}
            >
              <input
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="your@email.com"
                required
                className="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring"
              />
              <Button type="submit">获取早期访问</Button>
            </form>
          )}
        </div>
      </section>

      {/* Footer */}
      <footer className="border-t px-4 py-8">
        <div className="mx-auto flex max-w-6xl items-center justify-between">
          <div className="flex items-center gap-2">
            <Sparkles className="h-4 w-4 text-primary" />
            <span className="text-xs font-semibold">Lumos</span>
          </div>
          <p className="text-xs text-muted-foreground">
            &copy; {new Date().getFullYear()} Lumos. AI Agent Workstation.
          </p>
          <div className="flex items-center gap-4">
            <a href="#" className="text-xs text-muted-foreground hover:text-foreground">隐私政策</a>
            <a href="#" className="text-xs text-muted-foreground hover:text-foreground">服务条款</a>
          </div>
        </div>
      </footer>
    </div>
  );
}
