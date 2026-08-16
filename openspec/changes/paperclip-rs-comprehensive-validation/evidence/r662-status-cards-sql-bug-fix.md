# R662 - status_cards 真实 PG HTTP 测试 + SQL 续行符端口 bug 修复

## 目标
- 补齐 Node services/status-cards.ts (917 行) 的真实 HTTP 验证
- 修复 Rust 早期 port 遗留的 SQL 字符串字面量错误

## 实现

### 1. R662 真实 PG HTTP 测试
文件: crates/pc-http/tests/r662_status_cards_contract.rs (199 行)

8 步全过: list (empty), create (201), get, list (1 item), patch, updates, delete (204), 404.

### 2. 端口级 SQL bug 修复

根因：Rust port 早期用 C-style 的 \\\
 作为 SQL 字符串续行符。
在 Rust 中 \\ 是合法转义（单 backslash），newline 是普通字符，
运行时 PG 收到 \\\
，PG 不识别 \\ 续行符，syntax error。

症状：list_active 返回 error returned from database: syntax error at or near \\\


修复 (pc-repos/src/status_card.rs 等 6 文件 66 处):
- 把 5c 5c 0a 替换为 0a (仅保留 newline)
- bytes 单 \\\n (5c 0a) 是 Rust 合法 line continuation (编译时吞)，不动

### 3. 测试结果
running 1 test
R662 step1-8: PASS
test r662_status_cards_crud_full_cycle ... ok
test result: ok. 1 passed

## pc-repos 全套（修复后）
test result: ok. 634 passed

## pc-routines 全套（无回归）
test result: ok. 112 passed

## 关键发现
1. SQL 续行符 bug 真实存在但只在数据存在时触发
2. 大多数 list 因空 Vec 不触发 FromRow 解析
3. R658 realtime bridge 调用真实 list_active 碰到 status_cards 有数据失败
4. 修复后 全部测试通过
