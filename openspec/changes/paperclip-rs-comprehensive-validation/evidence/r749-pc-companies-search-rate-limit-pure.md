# R749 — pc-companies/search_rate_limit_pure 纯函数模块

## 目标

把 pc-companies/src/search_rate_limit.rs 中的核心纯函数从
InMemoryCompanySearchRateLimiter::consume 抽出到独立 search_rate_limit_pure 模块。

## 新增内容

### crates/pc-companies/src/search_rate_limit_pure.rs (9.9 KB / 24 单测)

#### 公开 API

| 函数 / 类型 | 用途 | 对齐 Node |
|---|---|---|
| retry_after_seconds_for_blocked(oldest_hit, now, window) | 算 retry-after 秒数 (向上取整, 最小 1) | Math.ceil((oldest + window - now) / 1000) |
| retry_after_min_one(secs) | retry-after 下限 1 秒 | Math.max(1, secs) |
| cutoff_for(now, window) | 窗口截止时间（None 表示无窗口约束） | now - window (saturating) |
| is_hit_in_window(hit, cutoff) | hit 是否在窗口内 | hit > cutoff |
| result_allowed(max, current_hits) | 构造 allowed result | consume 路径分支 |
| result_blocked(max, oldest, now, window) | 构造 blocked result | consume 路径分支 |
| actor_key(company_id, type, id) | 拼接 actor key | key(actor) |
| parse_window_ms(raw) | 解析环境变量 -> u64 | env parser |
| parse_max_requests(raw) | 解析环境变量 -> usize | env parser |
| prune_expired_hits(hits, cutoff) | 保留窗口内 hit | recentHits filter |
| pop_expired_front(deque, cutoff) | 弹出过期 hit | while let Some(front) |
| ResultParts struct | 纯 result 结构 | CompanySearchRateLimitResult |

#### 设计要点

1. 零 IO / 零 DB：所有函数只消费 u64 / usize / slice / 字符串。
2. retry_after 算术 1:1 对齐 Node 的 Math.ceil 行为（用 div_ceil）。
3. saturating_sub 用于避免 oldest + window - now 下溢。
4. cutoff = None 表示无窗口约束，全保留（与 Node 行为对齐）。
5. tests 全部命名 r749_*。

## 验证

cargo test -p pc-companies --lib search_rate_limit_pure
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 25 filtered out

cargo test -p pc-companies --lib
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

## 与 Node parity

| 行为 | Node | Rust | 一致 |
|---|---|---|---|
| retry-after 向上取整 | Y | Y | OK |
| retry-after 最小 1 秒 | Y | Y | OK |
| window 截止 saturating | Y | Y | OK |
| 过期 hit 淘汰 | Y | Y | OK |
| 构造 allowed/blocked result | Y | Y | OK |
| actor key 拼接 (company_id:type:id) | Y | Y | OK |
| env 解析（无效 -> None） | Y | Y | OK |

## 累计

| 项 | 之前 | R749 后 |
|---|---:|---:|
| pc-companies lib tests | 25 | 49 |
| pc-companies R749 新增 | - | +24 |
| 累计 R712-R749 新增 | 428 | +24 = 452 PASS |
| 累计新代码行数 | ~11500 | ~12000 |

## 后续

- R750 — pc-routines/activity_gate pure helper 抽取
