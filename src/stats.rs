//! Run statistics and the recent-window p95 that drives the adaptive timeout.

use std::collections::VecDeque;

const RECENT_WINDOW: usize = 64;

pub struct Stats {
    rtts: Vec<f64>, // all reply RTTs in ms, including late ones
    recent: VecDeque<f64>,
    pub sent: u64,
    pub late: u64,
    pub lost: u64,
    pub voided: u64, // discarded: scheduler stalls or local send failures
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

    /// p95 of the recent reply window, in ms — feeds the timeout autotuner.
    pub fn recent_p95(&self) -> Option<f64> {
        if self.recent.is_empty() {
            return None;
        }
        Some(percentile(
            &mut self.recent.iter().copied().collect::<Vec<_>>(),
            95.0,
        ))
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
    fn recent_p95_tracks_window() {
        let mut s = Stats::new();
        assert_eq!(s.recent_p95(), None);
        for _ in 0..64 {
            s.record_rtt(200.0);
        }
        assert_eq!(s.recent_p95(), Some(200.0));
    }

    #[test]
    fn percentile_basics() {
        let mut v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert_eq!(percentile(&mut v, 95.0), 95.0);
        let mut one = vec![7.0];
        assert_eq!(percentile(&mut one, 95.0), 7.0);
    }
}
