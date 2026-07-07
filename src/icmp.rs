//! Own the socket. ICMP echo via unprivileged datagram sockets
//! (macOS: native; Linux: needs net.ipv4.ping_group_range), with a
//! raw-socket fallback. Never shells out to ping.

use crate::probe::{Prober, Recv};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io;
use std::net::SocketAddr;
use std::time::Instant;

const PAYLOAD: &[u8] = b"s80!s80!s80!s80!s80!s80!"; // 24 bytes

pub struct Pinger {
    sock: Socket,
    ident: u16,
    buf: [std::mem::MaybeUninit<u8>; 2048],
}

impl Pinger {
    pub fn new(dest: SocketAddr) -> io::Result<Self> {
        let sock = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
            Ok(s) => s,
            Err(_) => Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4)).map_err(
                |e| {
                    io::Error::new(
                        e.kind(),
                        format!(
                            "cannot create ICMP socket: {e}\n\
                             on Linux, allow unprivileged ping:\n  \
                             sudo sysctl -w net.ipv4.ping_group_range='0 2147483647'"
                        ),
                    )
                },
            )?,
        };
        sock.connect(&SockAddr::from(dest))?;
        Ok(Pinger {
            sock,
            ident: (std::process::id() & 0xffff) as u16,
            buf: unsafe { std::mem::MaybeUninit::uninit().assume_init() },
        })
    }

}

impl Prober for Pinger {
    fn send(&mut self, seq: u16) -> io::Result<()> {
        let mut pkt = [0u8; 8 + PAYLOAD.len()];
        pkt[0] = 8; // echo request
        pkt[4..6].copy_from_slice(&self.ident.to_be_bytes());
        pkt[6..8].copy_from_slice(&seq.to_be_bytes());
        pkt[8..].copy_from_slice(PAYLOAD);
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
            self.sock.set_read_timeout(Some(deadline - now))?;
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
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(Recv::Interrupted),
                Err(e) => return Err(e),
            }
        }
    }

}

/// Returns the sequence number if `raw` is an ICMP echo reply.
///
/// Depending on platform and socket type the kernel may hand us the packet
/// with its IPv4 header still attached (macOS: always; Linux: raw only).
/// An IPv4 header starts with version nibble 4; an echo reply starts with
/// type byte 0 — so the first byte disambiguates.
///
/// We match on sequence number, not identifier: Linux dgram sockets rewrite
/// the id (and demux replies to us by it), and the socket is connect()ed,
/// so what arrives here is already ours.
fn parse_echo_reply(raw: &[u8]) -> Option<u16> {
    let icmp = if raw.first()? >> 4 == 4 {
        let ihl = ((raw[0] & 0xf) as usize) * 4;
        raw.get(ihl..)?
    } else {
        raw
    };
    if icmp.len() >= 8 && icmp[0] == 0 && icmp[1] == 0 {
        Some(u16::from_be_bytes([icmp[6], icmp[7]]))
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
        assert_eq!(parse_echo_reply(&icmp), Some(0xabcd));

        let mut wrapped = vec![0u8; 20 + 32];
        wrapped[0] = 0x45;
        wrapped[20..].copy_from_slice(&icmp);
        assert_eq!(parse_echo_reply(&wrapped), Some(0xabcd));
    }

    #[test]
    fn rejects_non_replies() {
        let mut req = [0u8; 32];
        req[0] = 8; // echo request, not reply
        assert_eq!(parse_echo_reply(&req), None);
        assert_eq!(parse_echo_reply(&[]), None);
    }
}
