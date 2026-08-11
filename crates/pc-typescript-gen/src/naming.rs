//! Identifier casing helpers for generated TypeScript output.
//!
//! JSON Schema property names follow OpenAPI conventions: usually
//! `camelCase` or `snake_case`. TypeScript conventions prefer
//! `camelCase` for properties and `PascalCase` for types.
//!
//! We do not rename aggressively — we only:
//! - PascalCase type names (`Decision`, `CompanyMember`)
//! - Preserve original property names (most JSON Schema property names
//!   are already valid TS identifiers; we escape invalid ones).

/// Convert an OpenAPI schema name (likely `PascalCase` already, e.g.
/// `CompanyMember`) into a TypeScript type identifier. If the name is
/// already a valid PascalCase / camelCase identifier, it is returned
/// unchanged. Otherwise it is normalized by stripping non-alphanumerics
/// and PascalCasing the words.
///
/// Examples:
/// - `Decision` → `Decision`
/// - `CompanyMember` → `CompanyMember`
/// - `pipeline_run` → `PipelineRun`
/// - `agent_hires` → `AgentHires`
#[must_use]
pub fn to_pascal_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut at_word_start = true;
    for ch in input.chars() {
        if ch.is_alphanumeric() {
            if at_word_start {
                out.extend(ch.to_uppercase());
                at_word_start = false;
            } else {
                out.push(ch);
            }
        } else if ch == '_' || ch == '-' || ch == ' ' {
            at_word_start = true;
        } else {
            // Skip other characters (e.g. `.`, `/`, `:`).
            at_word_start = true;
        }
    }
    out
}

/// Sanitize a property name into a valid TS identifier. Returns
/// `(safe_name, was_renamed)`. If the name is already a valid
/// identifier, `was_renamed` is false.
#[must_use]
pub fn safe_property_name(name: &str) -> (String, bool) {
    if name.is_empty() {
        return ("_".to_string(), true);
    }
    if is_valid_identifier(name) {
        return (name.to_string(), false);
    }
    let mut out = String::with_capacity(name.len());
    for (i, ch) in name.chars().enumerate() {
        if (i == 0 && ch.is_ascii_alphabetic()) || (i > 0 && (ch.is_alphanumeric() || ch == '_')) {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    (out, true)
}

fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    for ch in chars {
        if !(ch.is_alphanumeric() || ch == '_' || ch == '$') {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_already_pascal() {
        assert_eq!(to_pascal_case("Decision"), "Decision");
        assert_eq!(to_pascal_case("CompanyMember"), "CompanyMember");
    }

    #[test]
    fn pascal_from_snake() {
        assert_eq!(to_pascal_case("pipeline_run"), "PipelineRun");
        assert_eq!(to_pascal_case("agent_hires"), "AgentHires");
    }

    #[test]
    fn pascal_from_kebab() {
        assert_eq!(to_pascal_case("my-schema"), "MySchema");
    }

    #[test]
    fn safe_property_passes_valid_through() {
        assert_eq!(safe_property_name("foo"), ("foo".to_string(), false));
        assert_eq!(safe_property_name("_bar"), ("_bar".to_string(), false));
        assert_eq!(safe_property_name("$baz"), ("$baz".to_string(), false));
    }

    #[test]
    fn safe_property_replaces_invalid_chars() {
        assert_eq!(safe_property_name("foo-bar"), ("foo_bar".to_string(), true));
        assert_eq!(safe_property_name("a.b"), ("a_b".to_string(), true));
        assert_eq!(safe_property_name(""), ("_".to_string(), true));
    }
}
