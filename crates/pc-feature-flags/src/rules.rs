//! Rollout strategies.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RolloutStrategy {
    /// True for X% of identifiers (deterministic via hash).
    Percentage { pct: u8 },
    /// Allow-list of identifiers (company / user / agent id).
    AllowList { ids: Vec<uuid::Uuid> },
    /// Deny-list overrides allow-list.
    DenyList { ids: Vec<uuid::Uuid> },
    /// Opt-in only — none included unless explicitly allowed.
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RolloutRule {
    pub strategy: RolloutStrategy,
}

impl Default for RolloutRule {
    fn default() -> Self {
        Self {
            strategy: RolloutStrategy::Off,
        }
    }
}

impl RolloutRule {
    /// Pure check: does this rule include the given identifier?
    #[must_use]
    pub fn includes(&self, id: uuid::Uuid) -> bool {
        match &self.strategy {
            RolloutStrategy::Percentage { pct } => {
                if *pct == 0 {
                    return false;
                }
                if *pct >= 100 {
                    return true;
                }
                // Deterministic hash -> bucket
                let h = id.as_u128();
                let bucket = (h % 100) as u8;
                bucket < *pct
            }
            RolloutStrategy::AllowList { ids } => ids.contains(&id),
            RolloutStrategy::DenyList { ids } => !ids.contains(&id),
            RolloutStrategy::Off => false,
        }
    }

    #[must_use]
    pub fn percentage_all() -> Self {
        Self {
            strategy: RolloutStrategy::Percentage { pct: 100 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn percentage_zero_includes_none() {
        let r = RolloutRule {
            strategy: RolloutStrategy::Percentage { pct: 0 },
        };
        assert!(!r.includes(Uuid::new_v4()));
    }

    #[test]
    fn percentage_full_includes_all() {
        let r = RolloutRule {
            strategy: RolloutStrategy::Percentage { pct: 100 },
        };
        assert!(r.includes(Uuid::new_v4()));
    }

    #[test]
    fn percentage_50_includes_about_half() {
        let r = RolloutRule {
            strategy: RolloutStrategy::Percentage { pct: 50 },
        };
        let n = 1000;
        let hits = (0..n)
            .map(|_| Uuid::new_v4())
            .filter(|id| r.includes(*id))
            .count();
        // Within rough bounds 30-70%
        assert!(hits > 300 && hits < 700, "unexpected spread: {hits}/1000");
    }

    #[test]
    fn allowlist_includes_listed_only() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let r = RolloutRule {
            strategy: RolloutStrategy::AllowList { ids: vec![a] },
        };
        assert!(r.includes(a));
        assert!(!r.includes(b));
    }

    #[test]
    fn denylist_excludes_listed_only() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let r = RolloutRule {
            strategy: RolloutStrategy::DenyList { ids: vec![a] },
        };
        assert!(!r.includes(a));
        assert!(r.includes(b));
    }

    #[test]
    fn off_includes_nothing() {
        let r = RolloutRule::default();
        assert!(!r.includes(Uuid::new_v4()));
    }
}
