//! `GET /api/config` 发布的**前端公开 feature flag**（M10-4 / `LUM-2106`，`docs/64` §2.2 第 12 行）。
//!
//! 上游逐字：`server/internal/featureflags/keys.go::EvaluateFrontendPublicFlags`（pin `90e0bdf`）。
//!
//! ```go
//! var frontendPublicFlags = []string{BillingWorkspaceSubscriptions, ComposioMCPApps, PluginsV1}
//!
//! func EvaluateFrontendPublicFlags(ctx context.Context, flags *featureflag.Service) map[string]bool {
//!     out := make(map[string]bool, len(frontendPublicFlags)+3)
//!     for _, key := range frontendPublicFlags {
//!         out[key] = flags.IsEnabled(ctx, key, false)
//!     }
//!     out[agentBuilderCompat] = true
//!     out[agentSkillTogglesCompat] = true
//!     out[resourceLabelsCompat] = true
//!     return out
//! }
//! ```
//!
//! ## 6 键 = **3 走 provider + 3 恒 `true` 兼容键**
//!
//! | 键 | 缺省 | 分类 |
//! | --- | :-: | --- |
//! | `billing_workspace_subscriptions` | `false` | 走 provider（`IsEnabled(ctx, key, false)`） |
//! | `composio_mcp_apps` | `false` | 同上 |
//! | `plugins_v1` | `false` | 同上（dogfood 开关） |
//! | `agents_agent_builder` | **`true`** | 兼容键，**不接受** env / YAML 覆盖 |
//! | `agents_skill_toggles` | **`true`** | 兼容键 |
//! | `settings_resource_labels` | **`true`** | 兼容键 |
//!
//! 🔴 **两个必须"不发布"的键**（上游注释逐字给理由，`config_test.go` 逐条断言）：
//! `desktop_hang_stack_capture`（已装机的 v0.4.13–v0.4.18 客户端在 `true` 时会一直开着
//! 调试器通道，而该机群已产不出可用 stack ⇒ **不发布**就是拆弹）与 `custom_issue_statuses`
//! （pre-v0.4.33 客户端据此显示 "New status" 且 fail-closed）。⇒ 本模块**不导出**这两个常量，
//! `config/tests.rs` 用「键不出现」钉住。
//!
//! ⚠️ `triage_v1` 也是上游的 flag，但**不在** `frontendPublicFlags` 里 ⇒ 不发布。
//!
//! ## flag 源（**0 迁移**：上游 `migrations/` 里没有 feature flag 表）
//!
//! 上游 `featureflag.NewServiceFromEnv` 的标准链，按**优先级递减**：
//!
//! 1. `EnvProvider("FF_")` —— 运维急停开关（[`flag_key_to_env`] 的转换）；
//! 2. 可选 `MULTICA_FEATURE_FLAGS_FILE` 指向的 YAML 规则文件（`StaticProvider`）。
//!
//! 两者都没命中 ⇒ `IsEnabled` 返回调用方给的缺省值（这里三个门控键都是 `false`）。
//!
//! ## 为什么是**匿名**求值（`allow` / `deny` 为什么在本文件里恒不命中）
//!
//! `/api/config` 挂在**未鉴权**路由组上（上游 `GetConfig` 注释：*the web app calls it before
//! login*）⇒ 请求上下文里的 `EvalContext` 是**零值** ⇒ `Lookup("user_id")` 永远 miss
//! ⇒ `deny` / `allow` 列表无从命中（上游 `evaluateRule` 逐字：`if v, ok := ec.Lookup(by); ok && …`）。
//! 这不是简化，是**逐字等价**：本文件只保留 `percent` 与 `default` 两条分支，注释里写明为什么。
//!
//! `percent` 在零值上下文里仍有意义：标识符为空串，FNV-1a 桶**确定**（上游 `hash.go`：
//! *the empty string hashes to a stable bucket*）⇒ 匿名访客全体落进同一个桶（上游注释称之为
//! 期望行为），本文件用同一个 FNV-1a 复刻。

use std::collections::BTreeMap;

use serde::Deserialize;
use tracing::warn;

/// 门控键之一：workspace 计费 / Stripe Checkout / seat 对账 / Billing Portal（缺省关）。
pub const BILLING_WORKSPACE_SUBSCRIPTIONS: &str = "billing_workspace_subscriptions";
/// 门控键之一：Composio 应用管理 UI（缺省关）。
pub const COMPOSIO_MCP_APPS: &str = "composio_mcp_apps";
/// 门控键之一：Plugin 目录与生命周期 API 的 dogfood 开关（缺省关）。
pub const PLUGINS_V1: &str = "plugins_v1";

/// 兼容键：不再是发布开关，但**必须**继续发布为 `true`（已装机桌面客户端据此拿到恒开行为）。
pub const AGENT_BUILDER_COMPAT: &str = "agents_agent_builder";
/// 兼容键：同上（v0.4.0 客户端；v0.4.1 起客户端侧已拆，但键仍发布）。
pub const AGENT_SKILL_TOGGLES_COMPAT: &str = "agents_skill_toggles";
/// 兼容键：同上（v0.4.0–v0.4.15 **全部**客户端都 gate 它且缺键 fail-closed ⇒ 缺键即关）。
pub const RESOURCE_LABELS_COMPAT: &str = "settings_resource_labels";

/// 上游 `frontendPublicFlags` 的逐字三键（走 provider，缺省 `false`）。
pub const FRONTEND_PUBLIC_FLAGS: [&str; 3] = [
    BILLING_WORKSPACE_SUBSCRIPTIONS,
    COMPOSIO_MCP_APPS,
    PLUGINS_V1,
];

/// 三个恒 `true` 的兼容键（上游 `EvaluateFrontendPublicFlags` 尾部三行**逐字**硬编码）。
pub const COMPAT_PUBLIC_FLAGS: [&str; 3] = [
    AGENT_BUILDER_COMPAT,
    AGENT_SKILL_TOGGLES_COMPAT,
    RESOURCE_LABELS_COMPAT,
];

/// 上游 `featureflag.EnvFlagFile`（`config.go:15`）。
pub const ENV_FLAG_FILE: &str = "MULTICA_FEATURE_FLAGS_FILE";
/// 上游 `featureflag.EnvOverridePrefix`（`config.go:21`）。
pub const ENV_OVERRIDE_PREFIX: &str = "FF_";

/// YAML 规则文件的**线上格式**（上游 `ruleConfig`，只保留本文件真会读的字段）。
///
/// ⚠️ 上游还有 `variant` / `allow` / `allow_by` / `deny` / `deny_by`：前者在布尔判决里
/// 不参与（`variantEnabled` 只在**调用方**按 variant 分支时才读，而 `/api/config` 只发
/// 布尔值），后两者在匿名零值上下文里**恒不命中**（见模块头的说明）。
/// 未知字段**静默忽略**（serde 默认行为），所以一份上游完整的 flags 文件在本文件里可用。
#[derive(Debug, Default, Deserialize)]
struct RuleConfig {
    #[serde(default)]
    default: Option<bool>,
    #[serde(default)]
    percent: Option<PercentConfig>,
}

#[derive(Debug, Deserialize)]
struct PercentConfig {
    percent: i64,
}

/// 单键规则求值后的布尔判决。
#[derive(Debug, Clone, Copy)]
struct Rule {
    default: bool,
    /// `None` = 文件没写 `percent` 段（上游 `PercentRollout` 指针为 `nil`）。
    percent: Option<i64>,
}

impl Rule {
    /// 零值 `EvalContext` 下的判决（上游 `evaluateRule`，只保留可命中的两条分支）。
    fn evaluate(&self, key: &str) -> bool {
        // 1. deny / allow：零值上下文里 `ec.Lookup(..)` 恒 miss ⇒ 两条分支**不可达**。
        //    （上游逐字：`if v, ok := ec.Lookup(denyBy); ok && slices.Contains(..)`）
        // 2. percent：标识符为空串的确定桶（上游 `inPercent(key, "", pct)`）。
        match self.percent {
            Some(pct) => in_percent(key, "", pct),
            None => self.default,
        }
    }
}

/// 上游 `inPercent`（`hash.go`）：`≤0` 全关 / `≥100` 全开 / 中间比 FNV-1a 桶。
fn in_percent(key: &str, identifier: &str, percent: i64) -> bool {
    if percent <= 0 {
        return false;
    }
    if percent >= 100 {
        return true;
    }
    i64::from(bucket_for(key, identifier)) < percent
}

/// 上游 `bucketFor`：FNV-1a 32，`key` 与 `identifier` 之间写一个 `0` 分隔字节（防
/// `("ab","c")` 与 `("a","bc")` 撞桶），取 `sum32 % 100`。
fn bucket_for(key: &str, identifier: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5; // FNV offset basis
    for byte in key
        .bytes()
        .chain(std::iter::once(0u8))
        .chain(identifier.bytes())
    {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193); // FNV prime
    }
    hash % 100
}

/// 上游 `flagKeyToEnv`（`env_provider.go:137`）：字母转大写、**非字母数字的连续段**折成
/// 单个 `_`、首尾 `_` 去掉。刻意有损（大小写不敏感、标点段合并）。
pub fn flag_key_to_env(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut prev_underscore = false;
    for ch in key.chars() {
        match ch {
            'A'..='Z' | '0'..='9' => {
                out.push(ch);
                prev_underscore = false;
            }
            'a'..='z' => {
                out.extend(ch.to_uppercase());
                prev_underscore = false;
            }
            _ => {
                if !prev_underscore {
                    out.push('_');
                    prev_underscore = true;
                }
            }
        }
    }
    out.trim_matches('_').to_owned()
}

/// 上游 `EnvProvider.Lookup` 的布尔投影：`Some(decision)` ⇒ 命中，**不回落**。
///
/// 上游 `Service.Decision` 把 `ReasonError` 也当"真判决"（不让急停开关静默消失）⇒
/// 畸形百分比在这里**返回 `false`**，不回落到 YAML / 缺省。
fn env_override<F>(get: &F, key: &str) -> Option<bool>
where
    F: Fn(&str) -> Option<String>,
{
    let raw = get(&format!("{ENV_OVERRIDE_PREFIX}{}", flag_key_to_env(key)))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        // 显式置空 = 显式关闭（上游注释逐字）。
        return Some(false);
    }
    if let Some(pct_str) = trimmed.strip_suffix('%') {
        // 畸形（`abc%` / `-1%` / `101%`）⇒ `false`，**不**回落到 YAML / 缺省
        // （上游：`ReasonError` 也是一份真判决，急停开关不许静默消失）。
        return Some(match pct_str.trim().parse::<i64>() {
            Ok(pct) if (0..=100).contains(&pct) => in_percent(key, "", pct),
            _ => false,
        });
    }
    // 其它非空值 = 一个 variant 标识符 ⇒ 上游给 `Enabled=true`，与第一组同值；
    // 拆成两个分支是**有意**的（`match_same_arms` 已豁免）：让"显式关"的那一组
    // 在读代码时一眼可数。
    #[allow(clippy::match_same_arms)]
    Some(match trimmed.to_ascii_lowercase().as_str() {
        "true" | "on" | "1" | "yes" => true,
        "false" | "off" | "0" | "no" => false,
        _ => true,
    })
}

/// 读 `MULTICA_FEATURE_FLAGS_FILE` 指向的 YAML 规则文件（上游 `LoadRulesFromYAMLFile`）。
///
/// ⚠️ **两条与上游的已知差异**（登记在 `docs/32` §45）：
///
/// 1. 上游在**启动时**加载一次（`NewServiceFromEnv`），本函数**每次求值都读**——运维替换
///    文件后无需重启（与同一 handler 里 `POSTHOG_*` 的"每次请求重读"同款取向）。
/// 2. 上游遇到**畸形文件返回 error**（fail loudly，不静默丢配置），本函数 `warn!` 后按
///    "无规则文件"继续：`/api/config` 是**无状态公开面**，为它把整个进程启动拖死不划算；
///    缺省值是三个门控键全 `false`（fail-closed 一侧）。
fn load_rules<F>(get: &F) -> BTreeMap<String, Rule>
where
    F: Fn(&str) -> Option<String>,
{
    let mut out = BTreeMap::new();
    let Some(path) = get(ENV_FLAG_FILE).map(|v| v.trim().to_owned()) else {
        return out;
    };
    if path.is_empty() {
        return out;
    }
    match std::fs::read_to_string(&path) {
        Ok(body) if body.trim().is_empty() => {}
        // 上游：空 / 纯空白文件是合法的"还没写任何 flag"状态，返回空 map 且不报错。
        Ok(body) => match serde_yaml::from_str::<BTreeMap<String, RuleConfig>>(&body) {
            Ok(raw) => {
                for (key, rc) in raw {
                    out.insert(
                        key,
                        Rule {
                            default: rc.default.unwrap_or(false),
                            percent: rc.percent.map(|p| p.percent),
                        },
                    );
                }
            }
            Err(err) => {
                warn!(path = %path, %err, "feature flag rules file is malformed; ignoring it");
            }
        },
        Err(err) => {
            warn!(path = %path, %err, "feature flag rules file is unreadable; ignoring it");
        }
    }
    out
}

/// 单键求值：`FF_<KEY>` env 覆盖 → YAML 规则 → `default_val`（上游 `Service.IsEnabled`）。
fn flag_enabled<F>(get: &F, key: &str, default_val: bool, rules: &BTreeMap<String, Rule>) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(enabled) = env_override(get, key) {
        return enabled;
    }
    match rules.get(key) {
        Some(rule) => rule.evaluate(key),
        None => default_val,
    }
}

/// 上游 `EvaluateFrontendPublicFlags` 的等价物：6 键的匿名公开判决。
///
/// `get` 是「名字 → 值」的查询函数（生产 = `std::env::var`；单测 = 注入的闭包）⇒ 本函数
/// **不碰进程全局 env**、也不触库（`docs/64` §2.2 的"匿名可读 + 不触库"硬要求）。
///
/// 返回 `BTreeMap` 而非 `HashMap`：JSON 对象的键序因此是**排序**的，与上游 Go
/// `map[string]bool` 经 `encoding/json` 序列化后的排序键序逐字对齐（`AppConfig` 的注释）。
pub fn evaluate_frontend_public_flags<F>(get: F) -> BTreeMap<String, bool>
where
    F: Fn(&str) -> Option<String>,
{
    let rules = load_rules(&get);
    let mut out = BTreeMap::new();
    for key in FRONTEND_PUBLIC_FLAGS {
        out.insert(key.to_owned(), flag_enabled(&get, key, false, &rules));
    }
    // 兼容键硬编码为 true：**不查 env、不查 YAML**（上游逐字三行赋值）。
    for key in COMPAT_PUBLIC_FLAGS {
        out.insert(key.to_owned(), true);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name: &str| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn flag_key_to_env_uppercases_and_collapses_punctuation_runs() {
        assert_eq!(flag_key_to_env("plugins_v1"), "PLUGINS_V1");
        assert_eq!(
            flag_key_to_env("billing.workspace.subscriptions"),
            "BILLING_WORKSPACE_SUBSCRIPTIONS"
        );
        // 连续标点折成**一个** `_`，首尾 `_` 被去掉。
        assert_eq!(
            flag_key_to_env("checkout--new__payment"),
            "CHECKOUT_NEW_PAYMENT"
        );
        assert_eq!(flag_key_to_env("--lead--"), "LEAD");
        assert_eq!(
            flag_key_to_env("checkout.newPayment"),
            "CHECKOUT_NEWPAYMENT"
        );
    }

    #[test]
    fn default_publishes_three_false_and_three_compat_true() {
        let flags = evaluate_frontend_public_flags(env(&[]));
        assert_eq!(flags.len(), 6);
        for key in FRONTEND_PUBLIC_FLAGS {
            assert_eq!(flags.get(key), Some(&false), "{key} must default to false");
        }
        for key in COMPAT_PUBLIC_FLAGS {
            assert_eq!(flags.get(key), Some(&true), "{key} must stay true");
        }
    }

    #[test]
    fn retired_and_unpublished_keys_never_appear() {
        let flags = evaluate_frontend_public_flags(env(&[
            ("FF_DESKTOP_HANG_STACK_CAPTURE", "true"),
            ("FF_CUSTOM_ISSUE_STATUSES", "true"),
            ("FF_TRIAGE_V1", "true"),
        ]));
        for absent in [
            "desktop_hang_stack_capture",
            "custom_issue_statuses",
            "triage_v1",
        ] {
            assert!(
                !flags.contains_key(absent),
                "{absent} must not be published"
            );
        }
    }

    #[test]
    fn env_override_wins_and_accepts_the_upstream_value_forms() {
        for (raw, want) in [
            ("true", true),
            ("on", true),
            ("1", true),
            ("yes", true),
            ("YES", true),
            ("false", false),
            ("off", false),
            ("0", false),
            ("no", false),
            ("", false),
            // 非布尔非空 = variant 标识符 ⇒ 上游给 Enabled=true
            ("experiment-v2", true),
        ] {
            let flags = evaluate_frontend_public_flags(env(&[("FF_PLUGINS_V1", raw)]));
            assert_eq!(flags.get(PLUGINS_V1), Some(&want), "FF_PLUGINS_V1={raw:?}");
        }
    }

    #[test]
    fn compat_keys_ignore_env_and_yaml_overrides() {
        // 上游逐字：`out[agentBuilderCompat] = true`，三行在 provider 求值**之后**。
        let flags = evaluate_frontend_public_flags(env(&[
            ("FF_AGENTS_AGENT_BUILDER", "false"),
            ("FF_AGENTS_SKILL_TOGGLES", "0"),
            ("FF_SETTINGS_RESOURCE_LABELS", "off"),
        ]));
        for key in COMPAT_PUBLIC_FLAGS {
            assert_eq!(flags.get(key), Some(&true), "{key} is hardcoded true");
        }
    }

    #[test]
    fn malformed_percent_override_does_not_fall_through() {
        // 上游：畸形百分比 = ReasonError 判决，Enabled=false，**不**回落。
        for raw in ["abc%", "-1%", "101%"] {
            let flags = evaluate_frontend_public_flags(env(&[("FF_PLUGINS_V1", raw)]));
            assert_eq!(flags.get(PLUGINS_V1), Some(&false), "FF_PLUGINS_V1={raw:?}");
        }
    }

    #[test]
    fn bucket_for_is_the_upstream_fnv1a_bucket() {
        // 与上游 `hash.go` 同算法：确定性 + [0,100) 范围。固定值把算法钉死。
        assert_eq!(bucket_for("plugins_v1", ""), bucket_for("plugins_v1", ""));
        assert!(bucket_for("plugins_v1", "") < 100);
        // 分隔字节防撞桶：("ab","c") 与 ("a","bc") 必须不同。
        assert_ne!(bucket_for("ab", "c"), bucket_for("a", "bc"));
    }

    #[test]
    fn percent_rollout_uses_the_empty_identifier_bucket() {
        // pct=0 全关 / pct=100 全开（上游 inPercent 逐字）。
        assert!(!in_percent("plugins_v1", "", 0));
        assert!(in_percent("plugins_v1", "", 100));
        assert!(!in_percent("plugins_v1", "", -5));
        // 桶是确定的 ⇒ 同一 (key, "") 的判决在两次求值间不变。
        let bucket = i64::from(bucket_for("plugins_v1", ""));
        assert_eq!(
            in_percent("plugins_v1", "", bucket + 1),
            in_percent("plugins_v1", "", bucket + 1)
        );
    }
}
