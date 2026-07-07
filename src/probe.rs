//! What every probe engine speaks: send a numbered probe, wait for evidence
//! of a round trip.

use std::io;
use std::time::{Duration, Instant};

pub enum Recv {
    Reply {
        seq: u16,
        at: Instant,
    },
    /// Deadline passed. `overshoot` far beyond the deadline means the OS
    /// stalled us (scheduler, sleep) — the sample is a lie, not a loss.
    TimedOut {
        overshoot: Duration,
    },
    Interrupted,
}

pub trait Prober {
    fn send(&mut self, seq: u16) -> io::Result<()>;
    fn recv(&mut self, deadline: Instant) -> io::Result<Recv>;
}
