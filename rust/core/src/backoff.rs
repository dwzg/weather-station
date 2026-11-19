//! Exponential backoff with +-25 % jitter, so a device that lost the network
//! does not retry in lock-step with the outage.

#[derive(Clone, Debug)]
pub struct Backoff {
    min: u32,
    max: u32,
    failures: u32,
}

impl Backoff {
    pub const fn new(min_ms: u32, max_ms: u32) -> Self {
        Self { min: min_ms, max: max_ms, failures: 0 }
    }

    /// The next delay in ms. `random` is any 32-bit random value; the caller
    /// supplies it so this stays testable.
    pub fn next(&mut self, random: u32) -> u32 {
        let max = u64::from(self.max);
        let mut base = u64::from(self.min);
        let mut i = 0;
        while i < self.failures && base < max {
            base *= 2;
            i += 1;
        }
        base = base.min(max);
        if self.failures < 32 {
            self.failures += 1;
        }
        // jitter in [-25 %, +25 %)
        let span = base / 2;
        let jitter = if span > 0 { u64::from(random) % span } else { 0 };
        (base - base / 4 + jitter).max(1) as u32
    }

    pub fn reset(&mut self) {
        self.failures = 0;
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_and_caps() {
        let mut b = Backoff::new(1000, 8000);
        let d: Vec<u32> = (0..5).map(|_| b.next(0)).collect();
        assert_eq!(d, [750, 1500, 3000, 6000, 6000]);
        assert!(b.next(0xFFFF_FFFF) < 10_000);
        b.reset();
        assert_eq!(b.next(0), 750);
    }
}
