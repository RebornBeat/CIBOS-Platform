//! # Validation Helpers
//!
//! Small, shared validators for values that appear in more than one crate, so
//! the rules live in exactly one place. Each returns a typed
//! [`ConfigError`] describing the field and
//! the reason on failure, suitable for surfacing directly to a user editing
//! configuration.

use crate::types::error::ConfigError;

/// Inclusive lower bound for a scheduling weight.
pub const WEIGHT_MIN: u32 = 1;
/// Inclusive upper bound for a scheduling weight.
pub const WEIGHT_MAX: u32 = 100;

/// Validate that a scheduling weight is within `[WEIGHT_MIN, WEIGHT_MAX]`.
///
/// Weights are bounded so that the weighted-entropy selector cannot be handed a
/// zero (which would make a class unselectable) or an extreme value that would
/// starve other classes in spirit even where anti-starvation is compiled out.
///
/// # Errors
///
/// Returns [`ConfigError::ValidationFailed`] if `weight` is outside the range.
pub const fn validate_weight(weight: u32) -> Result<(), ConfigError> {
    if weight < WEIGHT_MIN {
        return Err(ConfigError::ValidationFailed {
            field: "weight",
            reason: "must be at least 1",
        });
    }
    if weight > WEIGHT_MAX {
        return Err(ConfigError::ValidationFailed {
            field: "weight",
            reason: "must be at most 100",
        });
    }
    Ok(())
}

/// Validate that a value is non-zero.
///
/// # Errors
///
/// Returns [`ConfigError::ValidationFailed`] tagged with `field` if `value`
/// is zero.
pub const fn validate_non_zero(value: u64, field: &'static str) -> Result<(), ConfigError> {
    if value == 0 {
        Err(ConfigError::ValidationFailed {
            field,
            reason: "must be non-zero",
        })
    } else {
        Ok(())
    }
}

/// Validate that `value` does not exceed `max`.
///
/// # Errors
///
/// Returns [`ConfigError::ValidationFailed`] tagged with `field` if
/// `value > max`.
pub const fn validate_at_most(
    value: u64,
    max: u64,
    field: &'static str,
) -> Result<(), ConfigError> {
    if value > max {
        Err(ConfigError::ValidationFailed {
            field,
            reason: "exceeds maximum",
        })
    } else {
        Ok(())
    }
}

/// Validate that a byte length fits within a fixed capacity.
///
/// # Errors
///
/// Returns [`ConfigError::ValidationFailed`] tagged with `field` if
/// `len > capacity`.
pub const fn validate_length(
    len: usize,
    capacity: usize,
    field: &'static str,
) -> Result<(), ConfigError> {
    if len > capacity {
        Err(ConfigError::ValidationFailed {
            field,
            reason: "exceeds capacity",
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_bounds() {
        assert!(validate_weight(0).is_err());
        assert!(validate_weight(1).is_ok());
        assert!(validate_weight(50).is_ok());
        assert!(validate_weight(100).is_ok());
        assert!(validate_weight(101).is_err());
    }

    #[test]
    fn non_zero_and_at_most() {
        assert!(validate_non_zero(0, "x").is_err());
        assert!(validate_non_zero(1, "x").is_ok());
        assert!(validate_at_most(5, 10, "x").is_ok());
        assert!(validate_at_most(11, 10, "x").is_err());
    }

    #[test]
    fn length_within_capacity() {
        assert!(validate_length(64, 64, "name").is_ok());
        assert!(validate_length(65, 64, "name").is_err());
    }
}
