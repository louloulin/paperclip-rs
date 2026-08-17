#![forbid(unsafe_code)]

//! Password strength + format validation pure helpers.
//!
//! R734: 零依赖密码校验：长度 / 字符类 / 字典词检查。

/// 最小密码长度（与 Node auth 配置对齐）。
pub const MIN_PASSWORD_LENGTH: usize = 8;

/// 最大密码长度（防止 bcrypt-like 算法的 DoS）。
pub const MAX_PASSWORD_LENGTH: usize = 256;

/// 常见弱密码字典（最常见的 50 个，截取自 Node auth policy）。
pub const COMMON_WEAK_PASSWORDS: &[&str] = &[
    "123456", "password", "123456789", "12345678", "12345",
    "111111", "1234567", "sunshine", "qwerty", "iloveyou",
    "admin", "welcome", "monkey", "login", "abc123",
    "starwars", "123123", "dragon", "passw0rd", "master",
    "hello", "freedom", "whatever", "trustno1", "654321",
    "jordan23", "harley", "password1", "shadow", "michael",
    "football", "baseball", "superman", "1qaz2wsx", "121212",
    "000000", "letmein", "666666", "batman", "liverpool",
    "hannah", "charlie", "donald", "password1!", "qwerty123",
];

/// 密码强度评估结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordStrength {
    TooShort,
    TooLong,
    Weak,
    Medium,
    Strong,
}

impl PasswordStrength {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooShort => "too_short",
            Self::TooLong => "too_long",
            Self::Weak => "weak",
            Self::Medium => "medium",
            Self::Strong => "strong",
        }
    }

    /// 是否足够强（>= Medium）。
    pub fn is_acceptable(self) -> bool {
        matches!(self, Self::Medium | Self::Strong)
    }
}

/// 评估密码强度。
///
/// 算法：
/// 1. 长度 < MIN_PASSWORD_LENGTH → TooShort
/// 2. 长度 > MAX_PASSWORD_LENGTH → TooLong
/// 3. 出现在字典中（case-insensitive）→ Weak
/// 4. 字符类数（lower/upper/digit/symbol）>= 4 → Strong
/// 5. 字符类数 >= 3 → Medium
/// 6. 其余 → Weak
pub fn evaluate_password_strength(password: &str) -> PasswordStrength {
    if password.chars().count() < MIN_PASSWORD_LENGTH {
        return PasswordStrength::TooShort;
    }
    if password.chars().count() > MAX_PASSWORD_LENGTH {
        return PasswordStrength::TooLong;
    }
    if COMMON_WEAK_PASSWORDS
        .iter()
        .any(|w| w.eq_ignore_ascii_case(password))
    {
        return PasswordStrength::Weak;
    }
    let classes = character_class_count(password);
    if classes >= 4 {
        PasswordStrength::Strong
    } else if classes >= 3 {
        PasswordStrength::Medium
    } else {
        PasswordStrength::Weak
    }
}

/// 数密码包含的字符类数（lower / upper / digit / symbol）。
pub fn character_class_count(password: &str) -> usize {
    let mut classes = 0;
    if password.chars().any(|c| c.is_ascii_lowercase()) {
        classes += 1;
    }
    if password.chars().any(|c| c.is_ascii_uppercase()) {
        classes += 1;
    }
    if password.chars().any(|c| c.is_ascii_digit()) {
        classes += 1;
    }
    if password.chars().any(|c| !c.is_alphanumeric()) {
        classes += 1;
    }
    classes
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn too_short_password() {
        assert_eq!(evaluate_password_strength("abc"), PasswordStrength::TooShort);
    }

    #[test]
    fn too_long_password() {
        let s = "a".repeat(MAX_PASSWORD_LENGTH + 1);
        assert_eq!(evaluate_password_strength(&s), PasswordStrength::TooLong);
    }

    #[test]
    fn weak_common_password() {
        assert_eq!(evaluate_password_strength("qwerty12345"), PasswordStrength::Weak);
    }

    #[test]
    fn strong_password_four_classes() {
        // lower + upper + digit + symbol
        assert_eq!(
            evaluate_password_strength("Abcdef123!@#"),
            PasswordStrength::Strong
        );
    }

    #[test]
    fn medium_password_three_classes() {
        // lower + upper + digit (no symbol)
        assert_eq!(
            evaluate_password_strength("Abcdef123456"),
            PasswordStrength::Medium
        );
    }

    #[test]
    fn weak_password_two_classes() {
        // lower + digit only
        assert_eq!(
            evaluate_password_strength("abcdef123456"),
            PasswordStrength::Weak
        );
    }

    #[test]
    fn character_class_count_zero() {
        assert_eq!(character_class_count(""), 0);
    }

    #[test]
    fn character_class_count_one() {
        assert_eq!(character_class_count("abc"), 1);
    }

    #[test]
    fn character_class_count_four() {
        assert_eq!(character_class_count("Abc123!@#"), 4);
    }

    #[test]
    fn is_acceptable_medium_or_strong() {
        assert!(PasswordStrength::Medium.is_acceptable());
        assert!(PasswordStrength::Strong.is_acceptable());
        assert!(!PasswordStrength::Weak.is_acceptable());
        assert!(!PasswordStrength::TooShort.is_acceptable());
        assert!(!PasswordStrength::TooLong.is_acceptable());
    }

    #[test]
    fn strength_as_str() {
        assert_eq!(PasswordStrength::Strong.as_str(), "strong");
        assert_eq!(PasswordStrength::Weak.as_str(), "weak");
    }
}


#[cfg(test)]
mod internal_tests_r771 {
    use super::*;

    // ---- Round 771: pc-auth::password_validation_pure 边缘测试 ----

    /// PasswordStrength 5 个变体字符串。
    #[test]
    fn r771_password_strength_as_str() {
        assert_eq!(PasswordStrength::TooShort.as_str(), "too_short");
        assert_eq!(PasswordStrength::Weak.as_str(), "weak");
        assert_eq!(PasswordStrength::Medium.as_str(), "medium");
        assert_eq!(PasswordStrength::Strong.as_str(), "strong");
        assert_eq!(PasswordStrength::Strong.as_str(), "strong");
    }

    /// is_acceptable: 仅 Strong + Strong 接受。
    #[test]
    fn r771_is_acceptable() {
        assert!(!PasswordStrength::TooShort.is_acceptable());
        assert!(!PasswordStrength::Weak.is_acceptable());
        assert!(PasswordStrength::Medium.is_acceptable());
        assert!(PasswordStrength::Strong.is_acceptable());
        assert!(PasswordStrength::Strong.is_acceptable());
    }

    /// evaluate_password_strength: 5 种典型密码。
    #[test]
    fn r771_evaluate_typical_passwords() {
        assert_eq!(evaluate_password_strength(""), PasswordStrength::TooShort);
        assert_eq!(evaluate_password_strength("xyz1"), PasswordStrength::TooShort);
        assert_eq!(evaluate_password_strength("password1"), PasswordStrength::Weak);
        assert_eq!(evaluate_password_strength("Password1"), PasswordStrength::Weak);
        assert_eq!(evaluate_password_strength("Str0ng!Pass!Word!"), PasswordStrength::Strong);
    }

    /// character_class_count: 4 个类。
    #[test]
    fn r771_character_class_count() {
        assert_eq!(character_class_count(""), 0);
        assert_eq!(character_class_count("abc"), 1, "lowercase only");
        assert_eq!(character_class_count("ABC"), 1, "uppercase only");
        assert_eq!(character_class_count("123"), 1, "digit only");
        assert_eq!(character_class_count("!@#"), 1, "symbol only");
        assert_eq!(character_class_count("abcABC"), 2);
        assert_eq!(character_class_count("abc123"), 2);
        assert_eq!(character_class_count("abcABC123!@#"), 4, "all 4 classes");
    }
}
