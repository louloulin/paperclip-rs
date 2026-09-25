//! toolkit 目录与 auth-config 解析 —— 上游 `integrations/composio/service.go` 的目录部分
//! （`ListToolkits` / `authConfigForToolkit` / `authConfigMap` / `fetchAuthConfigMap` /
//! `betterAuthConfig`）。
//!
//! **M8-0 anchor 建桩（`LUM-1797`）、M8-6 落地（`LUM-1803`）**。
//!
//! # 契约（`docs/61` §6.5 的 M8-6 行）
//!
//! **auth-config 未配置的 toolkit 不出现在目录里**（动态解析）。上游注释逐字：
//!
//! > enabling a toolkit is a dashboard action, not a redeploy
//!
//! 所以本文件把「哪些 auth config 可用」当输入，输出可见 toolkit 集；**没有任何**
//! toolkit→auth-config 的静态映射表、也不读 env。
//!
//! # 两件事，两个纯/缓存职责
//!
//! | 件 | 上游 | 性质 |
//! | --- | --- | --- |
//! | [`visible_toolkits`] | `ListToolkits` 的过滤段（`if _, canConnect := connectable[slug]; !canConnect`） | **纯函数**（无 I/O、无时钟） |
//! | [`best_auth_config_for_each_toolkit`] | `fetchAuthConfigMap` + `betterAuthConfig` 的归约段 | **纯函数** |
//! | [`AuthConfigDirectory`] | `authConfigMap` 的缓存 + 陈旧兜底 | 带 TTL 的进程内缓存（**无 Redis**） |
//!
//! 上游的 `authConfigMap` 在 `sync.Mutex` 里**跨网络调用**持锁（串行化刷新）。本仓不这样做：
//! 持锁跨越 `.await` 会踩 clippy 的 `await_holding_lock`，而且「刷新失败就发陈旧快照」这条
//! 语义不需要排他 —— 并发刷新写进同一个 map，后写者与先写者的内容**逐字相同**（同一个上游、
//! 同一个归约）⇒ 丢掉单飞不会改变可观察行为（见 [`AuthConfigDirectory::resolve`] 的注释）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use mc_core::composio::ComposioToolkit;

use crate::client::{AuthConfig, ComposioClient};
use crate::service::ComposioError;

/// auth-config 目录的默认缓存寿命：上游 `defaultAuthCacheTTL` 逐字（5 分钟）。
///
/// 上游注释逐字：「Short enough that enabling an auth config in the dashboard reflects within
/// minutes; long enough that a burst of connect/list requests does not hammer `/auth_configs`.」
pub const DEFAULT_AUTH_CACHE_TTL_SECS: i64 = 300;

/// 分页抓取的页数上限：上游 `maxAuthConfigPages` / `maxToolkitPages` 逐字（防上游游标病态自转）。
pub const MAX_LIST_PAGES: usize = 20;

/// 每页条数：上游 `listPageLimit` 逐字。
pub const LIST_PAGE_LIMIT: usize = 1000;

/// slug 的规范化（上游 `lowerTrim` 的语义：`ToLower` + `TrimSpace`）。
///
/// 目录、连接、state 三处都用它 ⇒ 三处的比较键**同一个**（上游为同此理由内联了 `lowerTrim`）。
pub fn normalize_slug(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// 过滤出**可见**的 toolkit 目录（上游 `ListToolkits` 的过滤段）。
///
/// # 语义（逐条，与上游对齐）
///
/// - `all` 里每个 toolkit 的 `auth_config_ids` 是**候选**（调用侧按 slug 从项目目录填进来）；
/// - `available_auth_config_ids` 是本项目**已启用**的 auth config id 集；
/// - 一个 toolkit 可见 ⇔ 它的候选集与已启用集**有交集** ⇒ 输出里 `auth_config_ids` 收窄成
///   那个交集（保持 toolkit 自己的顺序、去掉重复）；
/// - **顺序**按 `all` 原序（上游逐页追加、按 `usage` 排序 ⇒ 目录顺序是契约的一部分）；
/// - slug 重复只留**第一次**出现（上游 `seen` map 的语义：first wins）；
/// - slug 为空 / 规范化后为空 ⇒ 丢弃（上游 `if slug == "" { continue }`）。
pub fn visible_toolkits(
    all: Vec<ComposioToolkit>,
    available_auth_config_ids: &[String],
) -> Vec<ComposioToolkit> {
    let available: HashSet<&str> = available_auth_config_ids
        .iter()
        .map(String::as_str)
        .filter(|id| !id.is_empty())
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for mut toolkit in all {
        let slug = normalize_slug(&toolkit.slug);
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        let mut kept: Vec<String> = Vec::new();
        for id in &toolkit.auth_config_ids {
            if !available.contains(id.as_str()) || kept.iter().any(|other| other == id) {
                continue;
            }
            kept.push(id.clone());
        }
        if kept.is_empty() {
            continue;
        }
        toolkit.slug = slug;
        toolkit.auth_config_ids = kept;
        out.push(toolkit);
    }
    out
}

/// 把一页页的 auth config 归约成 `toolkit_slug → 选中的 auth_config_id`（上游 `fetchAuthConfigMap`）。
///
/// # 选择规则（上游 `betterAuthConfig` 逐字）
///
/// 1. **自定义（bring-your-own OAuth）胜过 Composio 托管的** —— 它是产品要的白标通路；
/// 2. 同类之间**最近更新的**胜出（上游比较的是 RFC3339 字符串 ⇒ 字典序即时间序，本片照抄）。
///
/// 跳过：`id` 为空、`status == "DISABLED"`（大小写不敏感）、`toolkit.slug` 规范化后为空。
/// **未知状态**（既非 `ENABLED` 也非 `DISABLED`）按可用处理 —— 上游只显式剔 `DISABLED`。
#[must_use]
pub fn best_auth_config_for_each_toolkit(configs: &[AuthConfig]) -> HashMap<String, String> {
    let mut best: HashMap<String, &AuthConfig> = HashMap::new();
    for config in configs {
        if config.id.is_empty() || config.status.eq_ignore_ascii_case("DISABLED") {
            continue;
        }
        let slug = normalize_slug(&config.toolkit_slug);
        if slug.is_empty() {
            continue;
        }
        match best.get(&slug) {
            Some(current) if !better_auth_config(config, current) => {}
            _ => {
                best.insert(slug, config);
            }
        }
    }
    best.into_iter()
        .map(|(slug, config)| (slug, config.id.clone()))
        .collect()
}

/// 候选 `candidate` 是否应取代当前选中的 `current`（上游 `betterAuthConfig` 逐字）。
#[must_use]
pub fn better_auth_config(candidate: &AuthConfig, current: &AuthConfig) -> bool {
    if candidate.is_composio_managed != current.is_composio_managed {
        return !candidate.is_composio_managed;
    }
    candidate.last_updated_at > current.last_updated_at
}

/// 一次缓存快照。
#[derive(Debug, Clone)]
struct Snapshot {
    /// 过期时刻（Unix 秒）。
    expires_at: i64,
    /// `toolkit_slug → auth_config_id`。
    map: Arc<HashMap<String, String>>,
}

/// 项目 auth-config 目录的**进程内**缓存（上游 `Service.authCache` + `authCacheExp` + `authCacheMu`）。
///
/// 用途两条：
/// 1. `begin_connect` 解析某个 toolkit 的 `auth_config_id`；
/// 2. `list_toolkits` 判定哪些 toolkit 可连接。
///
/// ⚠️ **单副本假设的另一面**：缓存是进程内的（无 Redis）⇒ 多副本部署时两个副本各自刷新，
/// 但两者刷的是**同一个上游目录**，所以不会出现票数不一致（登记在 `docs/32` §9.12）。
#[derive(Debug)]
pub struct AuthConfigDirectory {
    ttl_secs: i64,
    snapshot: Mutex<Option<Snapshot>>,
}

impl AuthConfigDirectory {
    /// 从 TTL（秒）构造。`ttl_secs <= 0` ⇒ 每次都重新抓（测试注入用）。
    pub fn new(ttl_secs: i64) -> Self {
        Self {
            ttl_secs,
            snapshot: Mutex::new(None),
        }
    }

    /// 当前 TTL（秒）。
    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }

    /// 拿到目录映射：命中缓存就返回它，未命中/过期就重新抓。
    ///
    /// # 「陈旧兜底」（上游 `authConfigMap` 的注释逐字：a transient `/auth_configs` blip should
    /// not make every toolkit suddenly un-connectable）
    ///
    /// 重新抓失败时，只要手里有**任何**快照就发它（哪怕已过期）；一份都没有才把错误上抛。
    ///
    /// # Errors
    ///
    /// 抓取失败且**无**陈旧快照可发 ⇒ [`ComposioError`]（上游错误 / 传输错误）。
    pub async fn resolve(
        &self,
        client: &ComposioClient,
        now_unix: i64,
    ) -> Result<Arc<HashMap<String, String>>, ComposioError> {
        let cached = {
            let guard = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
            guard.clone()
        };
        if let Some(snapshot) = &cached {
            if now_unix < snapshot.expires_at {
                return Ok(Arc::clone(&snapshot.map));
            }
        }

        // ⚠️ 刷新**不持锁**（见文件头：并发刷新写进同一个等价 map，丢掉单飞不改变可观察行为）。
        let configs = match client.list_auth_configs().await {
            Ok(configs) => configs,
            Err(error) => {
                if let Some(snapshot) = cached {
                    tracing::warn!(%error, "composio: auth config refresh failed; serving stale directory");
                    return Ok(snapshot.map);
                }
                return Err(error);
            }
        };
        let map = Arc::new(best_auth_config_for_each_toolkit(&configs));
        let mut guard = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        *guard = Some(Snapshot {
            expires_at: now_unix.saturating_add(self.ttl_secs),
            map: Arc::clone(&map),
        });
        Ok(map)
    }

    /// 某个 toolkit 的 `auth_config_id`（`None` = 项目里没有可用的 auth config ⇒ 上游
    /// `authConfigForToolkit` 返回 `""`）。
    ///
    /// # Errors
    ///
    /// 同 [`AuthConfigDirectory::resolve`]。
    pub async fn auth_config_for(
        &self,
        client: &ComposioClient,
        toolkit_slug: &str,
        now_unix: i64,
    ) -> Result<Option<String>, ComposioError> {
        let slug = normalize_slug(toolkit_slug);
        if slug.is_empty() {
            return Ok(None);
        }
        let map = self.resolve(client, now_unix).await?;
        Ok(map.get(&slug).cloned())
    }
}

impl Default for AuthConfigDirectory {
    fn default() -> Self {
        Self::new(DEFAULT_AUTH_CACHE_TTL_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolkit(slug: &str, configs: &[&str]) -> ComposioToolkit {
        ComposioToolkit {
            slug: slug.to_string(),
            name: slug.to_uppercase(),
            auth_config_ids: configs.iter().map(|id| (*id).to_string()).collect(),
            logo_url: None,
        }
    }

    fn auth_config(id: &str, slug: &str, managed: bool, updated: &str) -> AuthConfig {
        AuthConfig {
            id: id.to_string(),
            toolkit_slug: slug.to_string(),
            is_composio_managed: managed,
            status: "ENABLED".to_string(),
            last_updated_at: updated.to_string(),
        }
    }

    #[test]
    fn unconfigured_toolkits_are_dropped_entirely() {
        let all = vec![
            toolkit("notion", &["ac_notion"]),
            toolkit("github", &["ac_github"]),
            toolkit("slack", &[]),
        ];
        let visible = visible_toolkits(all, &["ac_notion".to_string()]);
        assert_eq!(visible.len(), 1, "只有 notion 有可用的 auth config");
        assert_eq!(visible[0].slug, "notion");
        assert_eq!(visible[0].auth_config_ids, vec!["ac_notion".to_string()]);
        assert_eq!(visible[0].name, "NOTION", "其他字段原样带出");
    }

    #[test]
    fn narrowing_keeps_order_dedupes_and_lowercases_slugs() {
        let all = vec![
            toolkit("  Notion  ", &["ac_2", "ac_1", "ac_1", "ac_dead"]),
            toolkit("notion", &["ac_1"]),
            toolkit("", &["ac_1"]),
        ];
        let visible = visible_toolkits(
            all,
            &[
                "ac_1".to_string(),
                "ac_2".to_string(),
                "ac_unused".to_string(),
            ],
        );
        assert_eq!(visible.len(), 1, "重复 slug 只留第一次；空 slug 丢弃");
        assert_eq!(visible[0].slug, "notion");
        assert_eq!(
            visible[0].auth_config_ids,
            vec!["ac_2".to_string(), "ac_1".to_string()],
            "保持 toolkit 自己的顺序、去掉重复与不可用项"
        );
    }

    #[test]
    fn empty_available_set_yields_an_empty_catalog() {
        let visible = visible_toolkits(vec![toolkit("notion", &["ac_notion"])], &[]);
        assert!(visible.is_empty(), "上游：没有可用 auth config ⇒ 目录空");
        // 空 id 也不算可用。
        let visible = visible_toolkits(vec![toolkit("notion", &[""])], &[String::new()]);
        assert!(visible.is_empty());
    }

    #[test]
    fn a_toolkit_with_no_candidates_is_invisible_even_if_ids_are_available() {
        let all = vec![toolkit("notion", &[])];
        assert!(visible_toolkits(all, &["ac_notion".to_string()]).is_empty());
    }

    #[test]
    fn custom_auth_config_beats_managed_and_newer_beats_older() {
        let custom_old = auth_config("ac_custom_old", "notion", false, "2026-01-01T00:00:00Z");
        let managed_new = auth_config("ac_managed_new", "notion", true, "2026-09-01T00:00:00Z");
        let custom_new = auth_config("ac_custom_new", "notion", false, "2026-05-01T00:00:00Z");
        let map = best_auth_config_for_each_toolkit(&[
            managed_new.clone(),
            custom_old.clone(),
            custom_new.clone(),
        ]);
        assert_eq!(
            map.get("notion").map(String::as_str),
            Some("ac_custom_new"),
            "自定义胜过托管；同类间最近更新者胜"
        );

        // 只有托管的两个 ⇒ 最近更新的那个。
        let map = best_auth_config_for_each_toolkit(&[
            auth_config("ac_m1", "notion", true, "2026-01-01T00:00:00Z"),
            auth_config("ac_m2", "notion", true, "2026-02-01T00:00:00Z"),
        ]);
        assert_eq!(map.get("notion").map(String::as_str), Some("ac_m2"));
    }

    #[test]
    fn disabled_and_malformed_configs_are_skipped() {
        let mut disabled = auth_config("ac_disabled", "notion", false, "2026-09-01T00:00:00Z");
        disabled.status = "disabled".to_string();
        let mut empty_id = auth_config("", "notion", false, "2026-09-02T00:00:00Z");
        empty_id.id = String::new();
        let map = best_auth_config_for_each_toolkit(&[
            disabled,
            empty_id,
            auth_config("ac_ok", "  Notion ", true, "2026-01-01T00:00:00Z"),
        ]);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("notion").map(String::as_str), Some("ac_ok"));
    }

    #[test]
    fn unknown_status_is_treated_as_available() {
        let mut weird = auth_config("ac_weird", "notion", true, "");
        weird.status = "PENDING".to_string();
        let map = best_auth_config_for_each_toolkit(&[weird]);
        assert_eq!(map.get("notion").map(String::as_str), Some("ac_weird"));
    }

    #[test]
    fn normalize_slug_trims_and_lowercases() {
        assert_eq!(normalize_slug("  NoTioN \t"), "notion");
        assert_eq!(normalize_slug("   "), "");
    }

    #[test]
    fn directory_defaults_to_the_upstream_ttl() {
        assert_eq!(
            AuthConfigDirectory::default().ttl_secs(),
            DEFAULT_AUTH_CACHE_TTL_SECS
        );
        assert_eq!(AuthConfigDirectory::new(1).ttl_secs(), 1);
        assert_eq!(MAX_LIST_PAGES, 20);
        assert_eq!(LIST_PAGE_LIMIT, 1000);
    }
}
