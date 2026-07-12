//! Own the socket. ICMP echo via unprivileged datagram sockets —
//! native on macOS, gated by net.ipv4.ping_group_range on Linux
//! (open by default on systemd distros since 2018). No raw sockets,
//! no privileges, never shells out to ping.

use crate::probe::{Prober, Recv};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const FILL: &[u8] = b"s80!s80!s80!s80!s80!"; // pad after the 2 epoch bytes

pub struct Pinger {
    sock: Socket,
    ident: u16,
    buf: [std::mem::MaybeUninit<u8>; 2048],
}

impl Pinger {
    pub fn new(dest: SocketAddr) -> io::Result<Self> {
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "cannot create unprivileged ICMP socket: {e}\n\
                     on Linux this is gated by a sysctl (open by default on \
                     modern distros, often closed in containers):\n  \
                     sudo sysctl -w net.ipv4.ping_group_range='0 65535'\n\
                     UDP probes need no privileges at all: s80 -u <target>"
                ),
            )
        })?;
        sock.connect(&SockAddr::from(dest))?;
        Ok(Pinger {
            sock,
            ident: (std::process::id() & 0xffff) as u16,
            buf: unsafe { std::mem::MaybeUninit::uninit().assume_init() },
        })
    }
}

impl Prober for Pinger {
    fn send(&mut self, seq: u32) -> io::Result<()> {
        // the header seq field is 16-bit; the upper half of our virtual
        // sequence rides in the payload (echoed back verbatim), so replies
        // reconstruct a u32 — no ambiguity when the wire seq wraps at
        // high probe rates within the late window
        let mut pkt = [0u8; 10 + FILL.len()];
        pkt[0] = 8; // echo request
        pkt[4..6].copy_from_slice(&self.ident.to_be_bytes());
        pkt[6..8].copy_from_slice(&((seq & 0xffff) as u16).to_be_bytes());
        pkt[8..10].copy_from_slice(&((seq >> 16) as u16).to_be_bytes());
        pkt[10..].copy_from_slice(FILL);
        let ck = checksum(&pkt);
        pkt[2..4].copy_from_slice(&ck.to_be_bytes());
        self.sock.send(&pkt)?;
        Ok(())
    }

    /// Block until an echo reply arrives or `deadline` passes.
    fn recv(&mut self, deadline: Instant) -> io::Result<Recv> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(Recv::TimedOut {
                    overshoot: now - deadline,
                });
            }
            // clamp: a sub-µs remainder truncates to a zero timeval, and a
            // zero SO_RCVTIMEO means "block forever" — the deadline check
            // above re-arms, so rounding up is safe; rounding down hangs
            self.sock
                .set_read_timeout(Some((deadline - now).max(Duration::from_micros(1))))?;
            match self.sock.recv(&mut self.buf) {
                Ok(n) => {
                    let raw =
                        unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const u8, n) };
                    if let Some(seq) = parse_echo_reply(raw) {
                        return Ok(Recv::Reply {
                            seq,
                            at: Instant::now(),
                        });
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(Recv::Interrupted),
                // transient local outage: wait it out until the deadline
                Err(e) if crate::probe::is_transient(&e) => {}
                Err(e) => return Err(e),
            }
        }
    }
}

/// Returns the sequence number if `raw` is an ICMP echo reply.
///
/// The kernel may hand us the packet with its IPv4 header still attached
/// (macOS does; Linux dgram doesn't). An IPv4 header starts with version
/// nibble 4; an echo reply starts with type byte 0 — so the first byte
/// disambiguates.
///
/// We match on sequence number, not identifier: Linux dgram sockets rewrite
/// the id (and demux replies to us by it), and the socket is connect()ed,
/// so what arrives here is already ours.
fn parse_echo_reply(raw: &[u8]) -> Option<u32> {
    let icmp = if raw.first()? >> 4 == 4 {
        let ihl = ((raw[0] & 0xf) as usize) * 4;
        raw.get(ihl..)?
    } else {
        raw
    };
    if icmp.len() >= 10 && icmp[0] == 0 && icmp[1] == 0 {
        let low = u16::from_be_bytes([icmp[6], icmp[7]]) as u32;
        let high = u16::from_be_bytes([icmp[8], icmp[9]]) as u32;
        Some((high << 16) | low)
    } else {
        None
    }
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in data.chunks(2) {
        let word = if chunk.len() == 2 {
            u16::from_be_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], 0])
        };
        sum += word as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_verifies() {
        // a packet checksummed over itself (with the sum in place) folds to 0
        let mut pkt = [0u8; 16];
        pkt[0] = 8;
        pkt[6] = 0x12;
        pkt[7] = 0x34;
        let ck = checksum(&pkt);
        pkt[2..4].copy_from_slice(&ck.to_be_bytes());
        assert_eq!(checksum(&pkt), 0);
    }

    #[test]
    fn parses_bare_and_ip_wrapped_replies() {
        let mut icmp = [0u8; 32];
        icmp[6] = 0xab;
        icmp[7] = 0xcd;
        icmp[8] = 0x01;
        icmp[9] = 0x02;
        assert_eq!(parse_echo_reply(&icmp), Some(0x0102_abcd));

        let mut wrapped = vec![0u8; 20 + 32];
        wrapped[0] = 0x45;
        wrapped[20..].copy_from_slice(&icmp);
        assert_eq!(parse_echo_reply(&wrapped), Some(0x0102_abcd));
    }

    #[test]
    fn rejects_replies_too_short_for_the_epoch() {
        let icmp = [0u8; 9]; // valid echo reply header but no epoch bytes
        assert_eq!(parse_echo_reply(&icmp), None);
    }

    #[test]
    fn rejects_non_replies() {
        let mut req = [0u8; 32];
        req[0] = 8; // echo request, not reply
        assert_eq!(parse_echo_reply(&req), None);
        assert_eq!(parse_echo_reply(&[]), None);
    }
}
