//! 报告：把「每条 fixture 恰好一行结论」汇总成可 `--check` 字节比对的产物。
//!
//! 口径全部写进产物本身（[`Report`] 的字段注释），读者不必翻文档才能解释数字；
//! 单独成文件的理由同 [`crate::requirements`]：它是**呈现层**，而 `lib.rs` 里的是
//! 回放逻辑，两者每次一起改的只有 [`to_row`] 一处。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{Bindings, Outcome, SCHEMA_VERSION};

// ---------------------------------------------------------------------------
// 报告
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub domain: String,
    pub method: String,
    pub path: String,
    pub actor: String,
    pub via: String,
    pub status_expected: u16,
    pub source: String,
    pub requires: Vec<String>,
    pub outcome: Outcome,
    pub tier: String,
    pub status_observed: Option<u16>,
    pub offline: Option<Outcome>,
    pub database: Option<Outcome>,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub fixtures: usize,
    pub pass: usize,
    pub mismatch: usize,
    pub unmounted: usize,
    pub placeholder: usize,
    pub unevaluable: usize,
    pub by_actor: BTreeMap<String, BTreeMap<String, usize>>,
    pub by_via: BTreeMap<String, BTreeMap<String, usize>>,
    pub tiers: BTreeMap<String, BTreeMap<String, usize>>,
}

/// 报告的头部：口径写进产物本身，读者不必去翻文档才能解释数字。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub golden_dir: String,
    pub bindings: BTreeMap<String, String>,
    pub totals: Totals,
    /// 契约等价率 = pass / fixtures（打不到 = 未实现，仍留在分母里）。
    pub contract_equivalence_rate: f64,
    /// 已接入路由等价率 = pass / (pass + mismatch)：只问"实现了的路由对不对"。
    pub mounted_equivalence_rate: Option<f64>,
    /// 离线可判定的 fixture 数与其中 pass 的数量（`actor.kind == "anonymous"`）。
    pub offline_decidable: OfflineSplit,
    pub fixtures: Vec<Row>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OfflineSplit {
    pub fixtures: usize,
    pub pass: usize,
}

impl Report {
    #[must_use]
    pub fn from_rows(golden_dir: &Path, bindings: &Bindings, mut rows: Vec<Row>) -> Self {
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        let mut totals = Totals {
            fixtures: rows.len(),
            ..Totals::default()
        };
        for r in &rows {
            match r.outcome {
                Outcome::Pass => totals.pass += 1,
                Outcome::Mismatch => totals.mismatch += 1,
                Outcome::Unmounted => totals.unmounted += 1,
                Outcome::Placeholder => totals.placeholder += 1,
                Outcome::Unevaluable => totals.unevaluable += 1,
            }
            *totals
                .by_actor
                .entry(r.actor.clone())
                .or_default()
                .entry(r.outcome.as_str().to_string())
                .or_insert(0) += 1;
            *totals
                .by_via
                .entry(r.via.clone())
                .or_default()
                .entry(r.outcome.as_str().to_string())
                .or_insert(0) += 1;
            *totals
                .tiers
                .entry(r.tier.clone())
                .or_default()
                .entry(r.outcome.as_str().to_string())
                .or_insert(0) += 1;
        }
        let eq = if totals.fixtures == 0 {
            0.0
        } else {
            ratio(totals.pass, totals.fixtures)
        };
        let mounted_den = totals.pass + totals.mismatch;
        let mounted = if mounted_den == 0 {
            None
        } else {
            Some(ratio(totals.pass, mounted_den))
        };
        let offline_rows: Vec<&Row> = rows.iter().filter(|r| r.actor == "anonymous").collect();
        let offline_decidable = OfflineSplit {
            fixtures: offline_rows.len(),
            pass: offline_rows
                .iter()
                .filter(|r| r.outcome == Outcome::Pass)
                .count(),
        };
        let mut binding_map = BTreeMap::new();
        binding_map.insert("user_id".into(), bindings.user_id.to_string());
        binding_map.insert("workspace_id".into(), bindings.workspace_id.to_string());
        Self {
            schema_version: SCHEMA_VERSION,
            golden_dir: golden_dir.display().to_string(),
            bindings: binding_map,
            totals,
            contract_equivalence_rate: eq,
            mounted_equivalence_rate: mounted,
            offline_decidable,
            fixtures: rows,
        }
    }

    /// 稳定的 JSON 文本（`--check` 就是拿它做字节比对）。
    pub fn to_json(&self) -> Result<String> {
        let mut s = serde_json::to_string_pretty(self)?;
        s.push('\n');
        Ok(s)
    }

    pub fn render_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "golden: {}  fixtures: {}",
            self.golden_dir, self.totals.fixtures
        );
        let _ = writeln!(
            out,
            "  pass {}  mismatch {}  unmounted {}  placeholder {}  unevaluable {}",
            self.totals.pass,
            self.totals.mismatch,
            self.totals.unmounted,
            self.totals.placeholder,
            self.totals.unevaluable
        );
        let _ = writeln!(
            out,
            "  契约等价率 = {}/{} = {:.1}%",
            self.totals.pass,
            self.totals.fixtures,
            self.contract_equivalence_rate * 100.0
        );
        match self.mounted_equivalence_rate {
            Some(r) => {
                let _ = writeln!(
                    out,
                    "  已接入路由等价率 = {}/{} = {:.1}%",
                    self.totals.pass,
                    self.totals.pass + self.totals.mismatch,
                    r * 100.0
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "  已接入路由等价率 = n/a（没有打到已实现路由的 fixture）"
                );
            }
        }
        let _ = writeln!(
            out,
            "  离线可判定（anonymous）= {}/{} pass",
            self.offline_decidable.pass, self.offline_decidable.fixtures
        );
        let needs: usize = self
            .fixtures
            .iter()
            .filter(|r| !r.requires.is_empty())
            .count();
        let _ = writeln!(
            out,
            "  声明了场景前提（requires）= {needs}/{} 条（前提凑不齐 ⇒ 那一层不判定，落 unevaluable）",
            self.fixtures.len()
        );
        let _ = writeln!(out, "  --- 非 pass ---");
        for r in &self.fixtures {
            if r.outcome != Outcome::Pass {
                let _ = writeln!(
                    out,
                    "  {:<11} {:<6} {:>3} {:<44} {}{}",
                    r.outcome.as_str(),
                    r.method,
                    r.status_expected,
                    r.path,
                    r.id,
                    if r.requires.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", r.requires.join(","))
                    }
                );
            }
        }
        out
    }

    /// 汇总一行（给 CI 日志 / issue 评论用）。
    #[must_use]
    pub fn summary_line(&self) -> String {
        format!(
            "pass {}/{} ({:.1}%) · mismatch {} · unmounted {} · placeholder {} · unevaluable {}",
            self.totals.pass,
            self.totals.fixtures,
            self.contract_equivalence_rate * 100.0,
            self.totals.mismatch,
            self.totals.unmounted,
            self.totals.placeholder,
            self.totals.unevaluable
        )
    }
}

#[allow(clippy::cast_precision_loss)] // 计数远小于 2^53，比例精度足够。
fn ratio(num: usize, den: usize) -> f64 {
    (num as f64) / (den as f64)
}
