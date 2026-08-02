//! 金额值对象（cents = 最小货币单位，避免浮点误差）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Money = Money(0);
    pub fn from_cents(cents: i64) -> Self {
        Self(cents)
    }
    pub fn from_dollars(dollars: i64) -> Self {
        Self(dollars * 100)
    }
    pub fn cents(&self) -> i64 {
        self.0
    }
}

impl std::ops::Add for Money {
    type Output = Money;
    fn add(self, rhs: Self) -> Self::Output {
        Money(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Self) -> Self::Output {
        Money(self.0 - rhs.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_arithmetic() {
        let a = Money::from_cents(150);
        let b = Money::from_cents(50);
        assert_eq!((a + b).cents(), 200);
        assert_eq!((a - b).cents(), 100);
    }

    #[test]
    fn dollars_to_cents() {
        assert_eq!(Money::from_dollars(3).cents(), 300);
    }
}
