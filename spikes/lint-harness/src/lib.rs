//! Test bench for the lint harness of lymaille.
//!
//! The base code is deliberately minimal and must pass every lint without a single diagnostic. Under the `canaries`
//! feature, deliberate faults prove that each lint fires (see `check.sh`).

#[cfg(feature = "canaries")]
pub mod canaries;

/// Adds two integers without silent overflow.
///
/// Returns `None` on overflow.
#[must_use]
pub const fn add(left: u32, right: u32) -> Option<u32> {
    left.checked_add(right)
}

#[cfg(test)]
mod tests {
    use super::add;

    #[test]
    fn adds_small_numbers() {
        assert_eq!(add(1, 2), Some(3), "1 + 2 must be 3");
    }

    #[test]
    fn reports_overflow() {
        assert_eq!(add(u32::MAX, 1), None, "overflow must be reported");
    }
}
