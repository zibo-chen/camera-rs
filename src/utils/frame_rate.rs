/// Phase-preserving limiter for native timestamps. Small timestamp rounding
/// errors must not turn a nominal 30 FPS stream into 15 FPS.
#[derive(Default)]
pub struct FrameRateLimiter {
    next_ns: Option<u64>,
    last_ns: u64,
}

impl FrameRateLimiter {
    pub fn accept(&mut self, timestamp_ns: u64, interval_ns: u64) -> bool {
        if interval_ns == 0 || timestamp_ns == 0 {
            return true;
        }
        if timestamp_ns < self.last_ns {
            self.next_ns = None;
        }
        self.last_ns = timestamp_ns;
        let tolerance = interval_ns / 20;
        if let Some(next) = self.next_ns {
            if timestamp_ns.saturating_add(tolerance) < next {
                return false;
            }
            self.next_ns = Some(if timestamp_ns > next.saturating_add(interval_ns) {
                timestamp_ns.saturating_add(interval_ns)
            } else {
                next.saturating_add(interval_ns)
            });
        } else {
            self.next_ns = Some(timestamp_ns.saturating_add(interval_ns));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounding_does_not_halve_frame_rate() {
        let mut limiter = FrameRateLimiter::default();
        let accepted = (1..=300)
            .filter(|i| limiter.accept(i * 33_333_000, 1_000_000_000 / 30))
            .count();
        assert_eq!(accepted, 300);
    }
    #[test]
    fn halves_sixty_fps_and_recovers_after_clock_reset_or_gap() {
        let mut limiter = FrameRateLimiter::default();
        assert_eq!(
            (1..=600)
                .filter(|i| limiter.accept(i * 16_666_667, 1_000_000_000 / 30))
                .count(),
            300
        );
        assert!(limiter.accept(100, 33_333_333));
        assert!(limiter.accept(50_000_000_000, 33_333_333));
        assert!(!limiter.accept(50_000_000_001, 33_333_333));
    }
}
