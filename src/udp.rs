//! UDP probing, traceroute-style: a datagram to a (presumably closed) high
//! port draws an ICMP port-unreachable from the target. On a *connected*
//! UDP socket the kernel hands us that as ECONNREFUSED on the next recv —
//! a round-trip proof with no raw socket and no privileges at all.
//!
//! For hosts that ignore ICMP echo but still run an IP stack (plenty of
//! routers), this is the honest fallback. Caveats, honestly stated: the
//! reply carries no sequence number, so it's attributed to the probe in
//! flight (unambiguous while serial, but a reply that crosses its own
//! timeout boundary lands on the next probe), and many devices rate-limit
//! unreachables (Linux default: ~1/s) — a sparse comb against such a box
//! is the policer talking, not the path.

use crate::probe::{Prober, Recv};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io;
use std::net::SocketAddr;
use std::time::Instant;

pub const DEFAULT_PORT: u16 = 33434; // traceroute's classic base port

pub struct UdpProber {
    sock: Socket,
    last_seq: u16,
    buf: [std::mem::MaybeUninit<u8>; 2048],
}

impl UdpProber {
    pub fn new(dest: SocketAddr, port: u16) -> io::Result<Self> {
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        let mut dest = dest;
        dest.set_port(port);
        sock.connect(&SockAddr::from(dest))?;
        Ok(UdpProber {
            sock,
            last_seq: 0,
            buf: unsafe { std::mem::MaybeUninit::uninit().assume_init() },
        })
    }
}

impl Prober for UdpProber {
    fn send(&mut self, seq: u16) -> io::Result<()> {
        self.last_seq = seq;
        let mut pkt = [0u8; 26];
        pkt[..2].copy_from_slice(&seq.to_be_bytes());
        pkt[2..].copy_from_slice(b"s80!s80!s80!s80!s80!s80!");
        match self.sock.send(&pkt) {
            Ok(_) => Ok(()),
            // a stale unreachable (from an earlier probe) can surface as a
            // pending error on send; it's cleared by surfacing — retry once
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                self.sock.send(&pkt).map(|_| ())
            }
            Err(e) => Err(e),
        }
    }

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
                // port unreachable came back: round trip proven
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                    return Ok(Recv::Reply {
                        seq: self.last_seq,
                        at: Instant::now(),
                    });
                }
                // actual data back means the port is open and something
                // answered — also a round trip
                Ok(_) => {
                    return Ok(Recv::Reply {
                        seq: self.last_seq,
                        at: Instant::now(),
                    });
                }
                Err(e)
                    if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(Recv::Interrupted),
                // other async errors (host/net unreachable from a mid-path
                // router) aren't a round trip to the target; keep waiting
                Err(e) if e.raw_os_error() == Some(libc::EHOSTUNREACH) => {}
                Err(e) if e.raw_os_error() == Some(libc::ENETUNREACH) => {}
                Err(e) => return Err(e),
            }
        }
    }

}
