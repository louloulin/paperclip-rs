//! skill 面聚合：`/api/skills*`（14 个注册键 = 12 + 2）。
//!
//! ## ⚠️ 本文件由 M6-0 anchor 冻结，M6 后续切片**不得**编辑
//!
//! 子路由的合并点在这里；各切片的文件（`crud` / `files` / `labels` / `import` / `refresh`）
//! 由**各自的写者**实作，anchor 只保证「合并点不再变」。若某个切片发现自己需要的子文件
//! 不在下面的清单里（`docs/57` §3.2 的格子有遗漏），**不要**直接加到这里 —— 记到
//! `docs/32` §9 的文件→写者表里，由集成方（M6-10）统一加。
//!
//! ## 路由账（`docs/57` §4.2 / §4.1）
//!
//! | 注册键 | 方法 | 上游 | 写者 |
//! | --- | :-: | --- | :-: |
//! | `/api/skills/`（+ 别名 `/api/skills`） | GET, POST | `router.go:2234-2235` | M6-2 |
//! | `/api/skills/search` | GET | `router.go:2236` | M6-2 |
//! | `/api/skills/:id/`（+ 别名 `/api/skills/:id`） | GET, PUT, DELETE | `router.go:2239-2241` | M6-2 |
//! | `/api/skills/:id/files` | GET, PUT | `router.go:2246-2247` | M6-2 |
//! | `/api/skills/:id/files/:fileId` | DELETE | `router.go:2248` | M6-2 |
//! | `/api/skills/:id/labels` | GET, POST | `router.go:2243-2244` | M6-2 |
//! | `/api/skills/:id/labels/:labelId` | DELETE | `router.go:2245` | M6-2 |
//! | `/api/skills/import` | POST | `router.go:2237` | M6-3 |
//! | `/api/skills/:id/refresh` | POST | `router.go:2242` | M6-3 |
//!
//! ## ⚠️ 尾斜杠双形态（`docs/57` §2.4，M6 只有这 5 个键有这条要求）
//!
//! 上游是 chi：`Route("/api/skills")` 之后 `Get("/")` / `Post("/")` ⇒ **两种形态都服务**
//! （`/api/skills` 与 `/api/skills/`）。axum 0.7 的 matchit 把二者当**不同**路径，
//! 未注册的形态返回 **404（不是 307）**。所以 M6-2 必须把下面的两组都注册，方法集合**逐字相同**：
//!
//! - `GET|POST /api/skills` + `GET|POST /api/skills/`
//! - `GET|PUT|DELETE /api/skills/:id` + `GET|PUT|DELETE /api/skills/:id/`
//!
//! 这 5 个键是 `slash_alias_audit.py` 在 M6 波里唯一点名要求双形态的地方；M6-0 已把
//! `docs/fixtures/slash-alias-allowlist.tsv` 里对应的 2 行**删掉**（豁免不再存在 ⇒
//! 漏注册就是 `MISSING_ALIAS` 硬失败，没有退路）。
//!
//! ⚠️ 路径参数必须写 `:id`（matchit 0.7 把 `{id}` 当**字面量**段：编译通过、恒 404）。

pub mod crud;
pub mod files;
pub mod helpers;
pub mod import;
pub mod labels;
pub mod refresh;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/skills*` 的聚合 router（state 由 `main.rs` 的 `with_state` 一次性注入）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(crud::router())
        .merge(files::router())
        .merge(labels::router())
        .merge(import::router())
        .merge(refresh::router())
}
