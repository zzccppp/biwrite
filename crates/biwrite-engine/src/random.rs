//! Tiny non-cryptographic randomness (jitter, mock delays) without a `rand`
//! dependency: std's `RandomState` is randomly seeded per instance.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

/// A random `u64`.
pub(crate) fn random_u64() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// Uniform duration in `[min, max]` (millisecond resolution).
pub(crate) fn random_between(min: Duration, max: Duration) -> Duration {
    let span = max.saturating_sub(min).as_millis();
    if span == 0 {
        return min;
    }
    let offset = u128::from(random_u64()) % (span + 1);
    min + Duration::from_millis(u64::try_from(offset).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_in_range() {
        let (lo, hi) = (Duration::from_millis(500), Duration::from_millis(2000));
        for _ in 0..200 {
            let d = random_between(lo, hi);
            assert!(d >= lo && d <= hi);
        }
        assert_eq!(random_between(hi, lo), hi);
    }
}
