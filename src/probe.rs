//! What every probe engine speaks: send a numbered probe, wait for evidence
//! of a round trip.

use std::io;
use std::time::{Duration, Instant};

pub enum Recv {
    Reply {
        seq: u32,
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
    fn send(&mut self, seq: u32) -> io::Result<()>;
    fn recv(&mut self, deadline: Instant) -> io::Result<Recv>;
}

/// Errors that describe a transient local outage (route flap, interface
/// down, kernel rate-limit) rather than broken program state. The run
/// must survive these — dying mid-incident is failing at the exact
/// moment the tool exists for.
pub fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(
            libc::ENETUNREACH
                | libc::EHOSTUNREACH
                | libc::ENETDOWN
                | libc::EHOSTDOWN
                | libc::ENOBUFS
                | libc::EPERM
        )
    )
}
