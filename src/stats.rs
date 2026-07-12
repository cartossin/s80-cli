//! Run statistics: streaming aggregates plus a log-spaced histogram for
//! percentiles, so memory stays flat (~160 KB) no matter how many
//! billions of probes a run sends. Min/avg/max are exact; percentiles
//! are accurate to one histogram bucket (0.1%).

use std::collections::VecDeque;

const RECENT_WINDOW: usize = 64;

// histogram: log-spaced buckets from 100 ns to ~60 s, 0.1% per bucket
const HIST_MIN_MS: f64 = 0.0001;
const HIST_GROWTH: f64 = 1.001;
const HIST_BUCKETS: usize = 20_300; // ln(60_000/0.0001)/ln(1.001), rounded up

pub struct Stats {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    hist: Vec<u64>,
    recent: VecDeque<f64>,
    pub sent: u64,
    pub late: u64,
    pub lost: u64,
    pub voided: u64, // discarded: scheduler stalls or local send failures
}

impl Stats {
    pub fn new() -> Self {
        Stats {
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: 0.0,
            hist: vec![0; HIST_BUCKETS],
            recent: VecDeque::with_capacity(RECENT_WINDOW),
            sent: 0,
            late: 0,
            lost: 0,
            voided: 0,
        }
    }

    pub fn record_rtt(&mut self, rtt_ms: f64) {
        self.count += 1;
        self.sum += rtt_ms;
        self.min = self.min.min(rtt_ms);
        self.max = self.max.max(rtt_ms);
        self.hist[bucket(rtt_ms)] += 1;
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

    /// p95 of the recent reply window, in ms — feeds the timeout autotuner.
    pub fn recent_p95(&self) -> Option<f64> {
        if self.recent.is_empty() {
            return None;
        }
        let mut v: Vec<f64> = self.recent.iter().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let rank = (0.95 * v.len() as f64).ceil() as usize;
        Some(v[rank.clamp(1, v.len()) - 1])
    }

    pub fn replies(&self) -> u64 {
        self.count
    }

    /// (min, avg, p95, max) over all replies, in ms. Min/avg/max are
    /// exact; p95 comes from the histogram (one bucket = 0.1%).
    pub fn summary(&self) -> Option<(f64, f64, f64, f64)> {
        if self.count == 0 {
            return None;
        }
        let rank = ((0.95 * self.count as f64).ceil() as u64).max(1);
        let mut seen = 0u64;
        let mut p95 = self.max;
        for (i, n) in self.hist.iter().enumerate() {
            seen += n;
            if seen >= rank {
                p95 = bucket_mid(i).clamp(self.min, self.max);
                break;
            }
        }
        Some((self.min, self.sum / self.count as f64, p95, self.max))
    }
}

fn bucket(rtt_ms: f64) -> usize {
    if rtt_ms <= HIST_MIN_MS {
        return 0;
    }
    let idx = ((rtt_ms / HIST_MIN_MS).ln() / HIST_GROWTH.ln()) as usize;
    idx.min(HIST_BUCKETS - 1)
}

/// Geometric midpoint of a bucket, in ms.
fn bucket_mid(i: usize) -> f64 {
    HIST_MIN_MS * HIST_GROWTH.powf(i as f64 + 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_p95_tracks_window() {
        let mut s = Stats::new();
        assert_eq!(s.recent_p95(), None);
        for _ in 0..64 {
            s.record_rtt(200.0);
        }
        assert_eq!(s.recent_p95(), Some(200.0));
    }

    #[test]
    fn summary_is_exact_where_it_claims_to_be() {
        let mut s = Stats::new();
        for x in 1..=100 {
            s.record_rtt(x as f64);
        }
        let (min, avg, p95, max) = s.summary().unwrap();
        assert_eq!(min, 1.0);
        assert_eq!(max, 100.0);
        assert!((avg - 50.5).abs() < 1e-9);
        // p95 within one 0.1% bucket of the true value
        assert!((p95 - 95.0).abs() / 95.0 < 0.002, "p95 {p95}");
    }

    #[test]
    fn histogram_stays_flat_at_scale() {
        let mut s = Stats::new();
        for i in 0..1_000_000u64 {
            s.record_rtt(0.01 + (i % 100) as f64 * 0.001);
        }
        assert_eq!(s.replies(), 1_000_000);
        let (_, _, p95, _) = s.summary().unwrap();
        assert!((p95 - 0.105).abs() / 0.105 < 0.01, "p95 {p95}");
    }

    #[test]
    fn bucket_edges_are_sane() {
        assert_eq!(bucket(0.0), 0);
        assert_eq!(bucket(-1.0), 0);
        assert!(bucket(60_000.0) < HIST_BUCKETS);
        assert!(bucket(1e12) == HIST_BUCKETS - 1);
    }
}
