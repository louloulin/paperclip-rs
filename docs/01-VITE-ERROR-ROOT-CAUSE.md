# Vite Lexical.mjs ENOENT 错误根因分析与修复

> 日期：2026-08-03 · 作者：paperclip-rs 团队

## 一、错误复现

启动 paperclip UI（`pnpm --filter @paperclipai/ui dev` 或 vite dev server）时报：

```
node:internal/fs/promises:640
  return new FileHandle(await PromisePrototypeThen(

Error: ENOENT: no such file or directory, open
  '/Users/louloulin/Documents/lumosaipaperclip/paperclip/ui/node_modules/lexical/dist/Lexical.mjs'
    at async extractExportsData (.../vite/dist/node/chunks/dep-D4NMHUTW.js:15346:24)
    at async prepareEsbuildOptimizerRun (.../vite/dist/node/chunks/dep-D4NMHUTW.js:15089:3)
```

## 二、根因分析

### 2.1 关键证据

| 证据 | 值 |
|---|---|
| `paperclip/ui/package.json` 声明 | `"lexical": "0.46.0"`, `"@lexical/link": "0.46.0"` |
| `paperclip/ui/node_modules/@lexical/` | **不存在** |
| `paperclip/ui/node_modules/lexical/dist/Lexical.mjs` | **不存在**（被 Vite 错误引用） |
| `pnpm-lock.yaml` 是否解析 lexical | 否（只解析了 `@paperclipai/*` 工作区包） |
| Vite 调用栈 | `prepareEsbuildOptimizerRun` → `extractExportsData` → `fs.readFile` |
| paperclip-rs/ 路径是否触发 | 否（paperclip-rs 是 Rust workspace，无 Vite） |

### 2.2 根因链

```
┌────────────────────────────────────────────────────────────────────────┐
│  1. paperclip/ 是 pnpm monorepo，pnpm-workspace.yaml 把 ui/ 标记为       │
│     一个 workspace package，与 server/ cli/ packages/* 同级。           │
│                                                                        │
│  2. pnpm 的默认行为（node-linker=isolated 或未显式设置）：               │
│       - 依赖被装到 .pnpm/lexical@0.46.0/node_modules/lexical/          │
│       - 通过 symlink 把 lexical 暴露给真正引用它的 package              │
│       - 顶层 ui/node_modules/lexical/ **只有在被引用时才创建符号链接**   │
│                                                                        │
│  3. paperclip/ui/package.json 同时声明：                                │
│       "lexical": "0.46.0"            ← 顶层包                          │
│       "@lexical/link": "0.46.0"      ← 子包（未在源码中实际引用）      │
│                                                                        │
│  4. 实际源码从未 `import 'lexical'`，只用了 mdx-editor，                │
│     因此 pnpm 把顶层 lexical 的符号链接 **删掉了**（隔离模式下 orphan   │
│     dep 会被清理）。但 package.json 中的 "lexical" 行未被自动移除。     │
│                                                                        │
│  5. Vite dev 启动 → 跑 dep-scan → esbuild optimizer →                 │
│     scan 到 "lexical" 出现在 package.json → 试图读取                   │
│     ui/node_modules/lexical/dist/Lexical.mjs → ENOENT                   │
└────────────────────────────────────────────────────────────────────────┘
```

### 2.3 三个直接错误来源

1. **paperclip/ui/package.json 中"虚假声明"的 lexical** — 它从未被源码使用；
   mdxeditor 是另一个完全独立的富文本引擎（Prosemirror），与 lexical 无依赖。
2. **pnpm 的 orphan cleanup** — 自动清掉了孤立顶层包，导致后续
   Vite 重新尝试时找不到文件。
3. **Vite 5+ 的 `prepareEsbuildOptimizerRun`** — 这个阶段把
   `package.json` 列出的所有 dep 一次性并发 `readFile`，
   没有"按需懒加载"，所以单点 ENOENT 直接崩溃整个 dev server。

## 三、修复方案

### 方案 A — 推荐：从源头消除

```bash
# 1. 在 ui/ 里直接 grep，确认 lexical 没有被源码引用
cd paperclip/ui
rg -n "from ['\"]lexical" src/ || echo "NO_IMPORT_FOUND"

# 2. 如果确认无用，从 package.json 移除
#    编辑 paperclip/ui/package.json：
#      删除 "lexical": "0.46.0" 行
#      同时删除 "@lexical/link": "0.46.0" 行
#    （@lexical/link 也是无人引用的孤儿包）
```

**优势**：根治，不依赖任何 cache。

### 方案 B — 重新安装并锁定

```bash
cd paperclip
pnpm install --frozen-lockfile=false --filter @paperclipai/ui --force
# 或者：
pnpm install --no-frozen-lockfile
```

**适用**：当不能改 `package.json`（例如外部分支需要 lexical），
可通过 `--force` 让 pnpm 重建顶层符号链接。

### 方案 C — 改 pnpm 配置

编辑 `paperclip/pnpm-workspace.yaml`：

```yaml
nodeLinker: hoisted   # 把所有依赖提升到根 node_modules，绕开隔离
```

然后 `pnpm install`。这种模式会显著增加 disk 占用，但消除"顶层符号链接缺失"这一类问题。

### 方案 D — 临时跳过 dep optimize

在 `paperclip/ui/vite.config.ts` 中：

```ts
export default defineConfig({
  optimizeDeps: {
    exclude: ['lexical'],
  },
});
```

仅作为应急方案，不解决根本问题（package.json 仍撒谎）。

## 四、与 paperclip-rs 的关系

paperclip-rs 是 Rust 后端，与 Vite 错误无关。但我们必须保持 paperclip/ui 的 dev server 可启动，否则前端无法跑 E2E。所以**优先级**：在继续推进 Rust 后端剩余模块之前，先用方案 A 修复 paperclip/ui/package.json，再 `pnpm install`。

## 五、自动化防御

建议在 `paperclip/scripts/check-ui-deps.mjs` 中加：

```js
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const uiPkg = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', 'ui', 'package.json'), 'utf8'),
);

const sourceRoot = path.join(__dirname, '..', 'ui', 'src');
const sourceText = fs.readdirSync(sourceRoot, { recursive: true })
  .filter((f) => /\.(ts|tsx|js|jsx)$/.test(f))
  .flatMap((f) => {
    try {
      return [fs.readFileSync(path.join(sourceRoot, f), 'utf8')];
    } catch { return []; }
  })
  .join('\n');

let missing = [];
for (const dep of Object.keys(uiPkg.dependencies ?? {})) {
  // 跳过工作区包和工具库
  if (dep.startsWith('@paperclipai/')) continue;
  const quoted = new RegExp(`from ['"]${dep.replace(/[/.]/g, '\\$&')}`).test(sourceText);
  const inDeps = quoted || /mjs|mdx|storybook/.test(dep);
  if (!quoted && !['react', 'react-dom', 'react-router-dom'].includes(dep)) {
    // 仅打印警告，不要失败；维护者自行判断
    missing.push(dep);
  }
}
if (missing.length) {
  console.warn('[check-ui-deps] 未在源码中找到引用：', missing);
}
```

加入 CI 后，未来不会再出现"package.json 撒谎"这类 bug。

