//! Run statistics and the recent-window p95 that drives the adaptive timeout.

use std::collections::VecDeque;
use std::time::Duration;

const RECENT_WINDOW: usize = 64;
const TIMEOUT_MULT: f64 = 4.0;
const TIMEOUT_FLOOR: Duration = Duration::from_millis(250);
const TIMEOUT_CEIL: Duration = Duration::from_millis(2000);
const TIMEOUT_INITIAL: Duration = Duration::from_millis(1000);

pub struct Stats {
    rtts: Vec<f64>, // all reply RTTs in ms, including late ones
    recent: VecDeque<f64>,
    pub sent: u64,
    pub late: u64,
    pub lost: u64,
    pub voided: u64, // probes discarded due to detected scheduler stalls
}

impl Stats {
    pub fn new() -> Self {
        Stats {
            rtts: Vec::new(),
            recent: VecDeque::with_capacity(RECENT_WINDOW),
            sent: 0,
            late: 0,
            lost: 0,
            voided: 0,
        }
    }

    pub fn record_rtt(&mut self, rtt_ms: f64) {
        self.rtts.push(rtt_ms);
        if self.recent.len() == RECENT_WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(rtt_ms);
    }

    /// A timed-out probe's reply arrived after all: it was never lost.
    pub fn lost_becomes_late(&mut self, rtt_ms: f64) {
        self.lost = self.lost.saturating_sub(1);
        self.late += 1;
        self.record_rtt(rtt_ms);
    }

    /// Adaptive timeout: TIMEOUT_MULT × p95 of the recent window, clamped.
    /// A drop on a 5 ms path shouldn't blind the stream for a fixed 2 s.
    pub fn timeout(&self) -> Duration {
        if self.recent.is_empty() {
            return TIMEOUT_INITIAL;
        }
        let p95 = percentile(&mut self.recent.iter().copied().collect::<Vec<_>>(), 95.0);
        Duration::from_secs_f64(p95 * TIMEOUT_MULT / 1000.0).clamp(TIMEOUT_FLOOR, TIMEOUT_CEIL)
    }

    pub fn replies(&self) -> u64 {
        self.rtts.len() as u64
    }

    /// (min, avg, p95, max) over all replies, in ms.
    pub fn summary(&self) -> Option<(f64, f64, f64, f64)> {
        if self.rtts.is_empty() {
            return None;
        }
        let mut v = self.rtts.clone();
        let p95 = percentile(&mut v, 95.0);
        let min = v[0];
        let max = v[v.len() - 1];
        let avg = self.rtts.iter().sum::<f64>() / self.rtts.len() as f64;
        Some((min, avg, p95, max))
    }
}

/// Nearest-rank percentile; sorts `v` in place.
fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    v[rank.clamp(1, v.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_floor_on_fast_path() {
        let mut s = Stats::new();
        for _ in 0..64 {
            s.record_rtt(0.5);
        }
        assert_eq!(s.timeout(), TIMEOUT_FLOOR);
    }

    #[test]
    fn timeout_tracks_slow_path() {
        let mut s = Stats::new();
        for _ in 0..64 {
            s.record_rtt(200.0);
        }
        assert_eq!(s.timeout(), Duration::from_millis(800));
    }

    #[test]
    fn percentile_basics() {
        let mut v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert_eq!(percentile(&mut v, 95.0), 95.0);
        let mut one = vec![7.0];
        assert_eq!(percentile(&mut one, 95.0), 7.0);
    }
}
