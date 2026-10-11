//! Time source for speed and remaining-time figures.

use std::time::{Duration, Instant};

use blocksource::sim::VirtualClock;

/// A monotonic clock. Tests use the simulated [`VirtualClock`] so HDD-scale timings can be
/// estimated without waiting.
pub trait Clock: Send + Sync {
    /// Time since an arbitrary fixed origin.
    fn now(&self) -> Duration;
}

/// Real time.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> Duration {
        self.elapsed()
    }
}
