//! Validation hook + reusable field-named bounds-check helpers.
//!
//! The [`Validate`] trait is the consumer-defined entry point; [`load`] does
//! not invoke it automatically — consumers call `cfg.validate()` themselves
//! after loading. The helpers below ([`ensure_range`], [`ensure_non_zero`],
//! …) are field-named building blocks consumers can compose into their
//! `Validate` impl. Each returns [`ValidationError`] on violation, which
//! carries the field name and a human-readable message.
//!
//! ```no_run
//! # use hjkl_config::{Validate, ValidationError, ensure_range, ensure_non_zero};
//! struct Config { tab_width: u8, max_lines: u32 }
//!
//! impl Validate for Config {
//!     type Error = ValidationError;
//!     fn validate(&self) -> Result<(), Self::Error> {
//!         ensure_range(self.tab_width, 1, 16, "tab_width")?;
//!         ensure_non_zero(self.max_lines, "max_lines")?;
//!         Ok(())
//!     }
//! }
//! ```
//!
//! [`load`]: crate::load

use std::fmt::Display;

/// Optional consumer-defined validation hook.
///
/// Decoupled from loading — implementing this trait does **not** make
/// `load()` invoke it automatically. Consumers call `cfg.validate()`
/// themselves after `load()` if validation is desired. This keeps the
/// loader's surface narrow and lets apps decide when (and whether) to
/// validate.
pub trait Validate {
    type Error: std::error::Error + Send + Sync + 'static;

    fn validate(&self) -> Result<(), Self::Error>;
}

/// Standard validation error carrying a field name and a human-readable
/// message. Consumers can use this directly as their `Validate::Error` or
/// wrap it in their own error type.
#[derive(Debug, thiserror::Error)]
#[error("config field `{field}` invalid: {message}")]
pub struct ValidationError {
    pub field: &'static str,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

/// `min ≤ value ≤ max`, else `Err`. Inclusive on both ends — the common
/// shape for config bounds (e.g. `tab_width` in `1..=16`).
pub fn ensure_range<T>(value: T, min: T, max: T, field: &'static str) -> Result<(), ValidationError>
where
    T: PartialOrd + Display + Copy,
{
    if value < min || value > max {
        return Err(ValidationError::new(
            field,
            format!("value {value} not in {min}..={max}"),
        ));
    }
    Ok(())
}

/// `value != T::default()`, else `Err`. For numeric types `T::default()`
/// is `0`, so this rejects zero. Generic over `Default` rather than a
/// numeric trait to keep the dep footprint at zero.
pub fn ensure_non_zero<T>(value: T, field: &'static str) -> Result<(), ValidationError>
where
    T: PartialEq + Default + Display,
{
    if value == T::default() {
        return Err(ValidationError::new(field, "must not be zero"));
    }
    Ok(())
}

/// `value ∈ allowed`, else `Err` with the allowed list in the message.
/// Use for enum-shaped string fields (theme names, channels, etc).
pub fn ensure_one_of<T>(
    value: &T,
    allowed: &[T],
    field: &'static str,
) -> Result<(), ValidationError>
where
    T: PartialEq + Display,
{
    if !allowed.iter().any(|a| a == value) {
        let listed: Vec<String> = allowed.iter().map(|x| format!("\"{x}\"")).collect();
        return Err(ValidationError::new(
            field,
            format!("value \"{value}\" not in [{}]", listed.join(", ")),
        ));
    }
    Ok(())
}

/// `!value.is_empty()`, else `Err`.
pub fn ensure_non_empty_str(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::new(field, "must not be empty"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_range_inclusive_boundaries_ok() {
        assert!(ensure_range(1u8, 1, 16, "tab_width").is_ok());
        assert!(ensure_range(16u8, 1, 16, "tab_width").is_ok());
        assert!(ensure_range(8u8, 1, 16, "tab_width").is_ok());
    }

    #[test]
    fn ensure_range_below_min_errs() {
        let err = ensure_range(0u8, 1, 16, "tab_width").unwrap_err();
        assert_eq!(err.field, "tab_width");
        assert!(err.message.contains("0"));
        assert!(err.message.contains("1..=16"));
    }

    #[test]
    fn ensure_range_above_max_errs() {
        let err = ensure_range(64u8, 1, 16, "tab_width").unwrap_err();
        assert_eq!(err.field, "tab_width");
        assert!(err.message.contains("64"));
    }

    #[test]
    fn ensure_non_zero_rejects_zero() {
        assert!(ensure_non_zero(0u32, "x").is_err());
        assert!(ensure_non_zero(0i64, "x").is_err());
    }

    #[test]
    fn ensure_non_zero_accepts_nonzero() {
        assert!(ensure_non_zero(1u32, "x").is_ok());
        assert!(ensure_non_zero(42u64, "x").is_ok());
    }

    #[test]
    fn ensure_one_of_finds_match() {
        let allowed = ["dark".to_string(), "light".to_string()];
        assert!(ensure_one_of(&"dark".to_string(), &allowed, "theme").is_ok());
    }

    #[test]
    fn ensure_one_of_rejects_unknown() {
        let allowed = ["dark".to_string(), "light".to_string()];
        let err = ensure_one_of(&"solarized".to_string(), &allowed, "theme").unwrap_err();
        assert_eq!(err.field, "theme");
        assert!(err.message.contains("solarized"));
        assert!(err.message.contains("dark"));
        assert!(err.message.contains("light"));
    }

    #[test]
    fn ensure_non_empty_str_works() {
        assert!(ensure_non_empty_str("x", "name").is_ok());
        let err = ensure_non_empty_str("", "name").unwrap_err();
        assert_eq!(err.field, "name");
    }

    #[test]
    fn validation_error_display_includes_field_and_message() {
        let err = ValidationError::new("editor.tab_width", "value 0 not in 1..=16");
        let s = err.to_string();
        assert!(s.contains("editor.tab_width"));
        assert!(s.contains("value 0 not in 1..=16"));
    }
}
