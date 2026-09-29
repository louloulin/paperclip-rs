//! 观测与判定：把一次回放的结果和上游断言比对，得出 `Outcome`。
//!
//! 从 `lib.rs` 拆出（门 ⑩ 第 8 批）。对外符号由 crate 根 `pub use` 重导出，路径不变。

use serde::{Deserialize, Serialize};

use crate::Fixture;

// ---------------------------------------------------------------------------
// 判定
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// 与上游断言一致。
    Pass,
    /// 打到了已实现的路由，但与上游断言不符。
    Mismatch,
    /// 本仓没有这条路由。
    Unmounted,
    /// 路由在，但只是 M0 占位实现。
    Placeholder,
    /// 本仓无法构造这次请求。
    Unevaluable,
}

impl Outcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Mismatch => "mismatch",
            Self::Unmounted => "unmounted",
            Self::Placeholder => "placeholder",
            Self::Unevaluable => "unevaluable",
        }
    }

    /// 取"更强"的结论：变体声明次序即强度次序（`Pass` 最前 = 最强），
    /// 所以 "更好" 就是较小的那个。合并两层时用 `a < b` 直接比较即可。
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Mismatch => 1,
            Self::Placeholder => 2,
            Self::Unmounted => 3,
            Self::Unevaluable => 4,
        }
    }
}

/// 一次回放的原始观察。
#[derive(Debug, Clone)]
pub struct Observed {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

/// 每个 fixture 一行的结论。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureOutcome {
    pub outcome: Outcome,
    pub tier: String,
    pub detail: String,
    pub status_observed: Option<u16>,
    pub offline: Option<Outcome>,
    pub database: Option<Outcome>,
}

/// `expect.json_subset` 是响应 body 的"子集"（逐层递归，数组按下标对应）。
///
/// `Err` 里带回第一个不匹配的路径，方便直接看出是哪个字段。
pub fn json_subset(actual: &serde_json::Value, expected: &serde_json::Value) -> Result<(), String> {
    use serde_json::Value;
    fn go(actual: &Value, expected: &Value, at: &str) -> Result<(), String> {
        match (actual, expected) {
            (Value::Object(a), Value::Object(e)) => {
                for (k, ev) in e {
                    match a.get(k) {
                        Some(av) => go(av, ev, &format!("{at}.{k}"))?,
                        None => return Err(format!("{at}.{k}: missing in response")),
                    }
                }
                Ok(())
            }
            (Value::Array(a), Value::Array(e)) => {
                if a.len() < e.len() {
                    return Err(format!(
                        "{at}: response array has {} items, expected at least {}",
                        a.len(),
                        e.len()
                    ));
                }
                for (i, ev) in e.iter().enumerate() {
                    go(&a[i], ev, &format!("{at}[{i}]"))?;
                }
                Ok(())
            }
            (a, e) => {
                if a == e {
                    Ok(())
                } else {
                    Err(format!("{at}: response {a} != expected {e}"))
                }
            }
        }
    }
    go(actual, expected, "$")
}

/// 判定一次回放：状态码 + 可选 `json_subset`。
pub fn judge(fx: &Fixture, observed: &Observed) -> (Outcome, String) {
    let empty_body = observed.body.is_empty();
    let json = observed
        .content_type
        .as_deref()
        .is_some_and(|c| c.contains("application/json"));
    let expected = fx.expect.status;

    if observed.status == expected {
        if fx.expect.json_subset.is_null()
            || fx
                .expect
                .json_subset
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        {
            return (Outcome::Pass, "status matched".into());
        }
        let parsed: serde_json::Value = match serde_json::from_slice(&observed.body) {
            Ok(v) => v,
            Err(e) => {
                return (
                    Outcome::Mismatch,
                    format!("status matched but body is not json: {e}"),
                )
            }
        };
        return match json_subset(&parsed, &fx.expect.json_subset) {
            Ok(()) => (Outcome::Pass, "status + json_subset matched".into()),
            Err(e) => (Outcome::Mismatch, format!("json_subset mismatch at {e}")),
        };
    }

    // 状态码不同：先分辨"没实现"与"实现错了"。
    if observed.status == 404 && empty_body && !json {
        return (
            Outcome::Unmounted,
            format!("no route: 404 with empty body (axum fallback), expected {expected}"),
        );
    }
    if observed.status == 405 {
        return (
            Outcome::Unmounted,
            format!("path exists but method is not mounted (405), expected {expected}"),
        );
    }
    if observed.status == 501 {
        return (
            Outcome::Placeholder,
            format!("route is a declared stub (501), expected {expected}"),
        );
    }
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&observed.body) {
        if v.get("code").and_then(|c| c.as_str()) == Some("not_implemented") {
            return (
                Outcome::Placeholder,
                format!(
                    "route returns the M0 placeholder envelope (status {}), expected {expected}",
                    observed.status
                ),
            );
        }
    }
    (
        Outcome::Mismatch,
        format!("status {} != expected {expected}", observed.status),
    )
}

