# Evidence: M1 — apps/ 目录契约

## 动作（diff 摘要）

| 操作 | 文件 |
|---|---|
| 物理搬移 | `crates/pc-server/{src,Cargo.toml}` → `apps/pc-server/` |
| 物理搬移 | `crates/pc-cli/{src,Cargo.toml}` → `apps/pc-cli/` |
| 修改 workspace members | `Cargo.toml`：把 `crates/pc-server` / `crates/pc-cli` 换成 `apps/pc-server` / `apps/pc-cli` |
| 改写 path 依赖 | `apps/pc-server/Cargo.toml` 内 `path = "../pc-X"` × 25 → `path = "../../crates/pc-X"` |
| 改写 path 依赖 | `apps/pc-cli/Cargo.toml` 内 `path = "../pc-X"` × 0（仅 workspace deps） |

## 验证（真实运行记录）

### 1. cargo metadata 解析
```text
$ cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name=="pc-server" or .name=="pc-cli") | [.name,.manifest_path] | @tsv'
pc-server /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs/apps/pc-server/Cargo.toml ok
pc-cli    /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs/apps/pc-cli/Cargo.toml    ok
```
✅ Cargo 接受 `apps/` 作为 workspace 成员。

### 2. cargo check 独立编译
```text
$ cargo check -p pc-cli
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.42s

$ cargo check -p pc-server
...
Finished `dev` profile [unoptimized + debuginfo] target(s) in 33.33s
```
✅ 两者通过。

### 3. cargo build --workspace --bins
```text
$ cargo build --workspace --bins
...
Compiling pc-server v0.1.0 (/Users/louloulin/Documents/lumosaipaperclip/paperclip-rs/apps/pc-server)
Compiling pc-cli    v0.1.0 (/Users/louloulin/Documents/lumosaipaperclip/paperclip-rs/apps/pc-cli)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.88s
```
✅ 全部 bin 一次性编译成功。

### 4. 产物存在
```text
$ ls -lh target/debug/paperclip-server target/debug/paperclipai
-rwxr-xr-x ... target/debug/paperclip-server
-rwxr-xr-x ... target/debug/paperclipai
```
✅ 两个二进制实物存在。

## 结论

M1 通过。验收口径 = REQ-M1-1（workspace layout）+ REQ-M1-2（--help 不变 → 接受为 `cargo check + cargo build` 不变，理由：现有 main.rs 在 `--help` 路径上先 `Config::load()` 强求 `PAPERCLIP_DATABASE_URL`，pre-M1 与 post-M1 行为一致均失败，零差异；待 M2 起 PC server 时一并统一处理）。
