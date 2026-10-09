//! Time as the router sees it.
//!
//! The core never reads a clock. Callers pass an [`Instant`] into every call
//! that needs one, which keeps the crate portable across embassy, ESP-IDF,
//! tokio and plain loops, and makes tests deterministic.

use core::ops::Add;
use core::time::Duration;

/// Milliseconds since a monotonic epoch chosen by the caller, such as boot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    pub const fn from_millis(ms: u64) -> Self {
        Self(ms)
    }

    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Time elapsed since `earlier`, or zero if `earlier` is later than `self`.
    pub fn saturating_duration_since(self, earlier: Instant) -> Duration {
        Duration::from_millis(self.0.saturating_sub(earlier.0))
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, rhs: Duration) -> Instant {
        let ms = u64::try_from(rhs.as_millis()).unwrap_or(u64::MAX);
        Instant(self.0.saturating_add(ms))
    }
}
