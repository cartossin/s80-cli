# s80

Terminal-native latency/jitter visualizer. Sibling of [s80.me](https://s80.me) —
not a port, a rethink freed from the browser.

```
$ s80 1.1.1.1
s80 1.1.1.1 (1.1.1.1) — ^C for stats
!!!!!!!!!!!!!!!!!.!!!!!!!!!!!!,!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
--- s80 1.1.1.1 (1.1.1.1) ---
194 probes  193 replies  min/avg/p95/max = 10.78/15.41/15.05/100.81 ms
late 1 (0.52%)  lost 1 (0.52%)  elapsed 3.0s  rate 64/s
```

## How it works

**Self-clocked ping-pong.** One probe in flight; the next is sent the instant
the reply lands (an homage to the Cisco CMTS pinger). It can't flood by
construction — the send rate is ACK-clocked to the path — and the output rate
IS the RTT. A healthy path streams glyphs at a steady rhythm; jitter reads as
stutter your eye catches pre-attentively.

**Glyphs.**

- `!` — reply, colored on a log-scale wheel from blue (µs territory) through
  green (~1 ms) to red (~1.5 s). Above 1 ms the colors match s80.me exactly;
  below it the same formula keeps going, so a LAN comb has visible texture
  instead of uniform green
- `.` — timeout
- `,` — late: the reply arrived *after* its timeout. The `.` is repainted in
  place if still on screen, and always tallied separately. Late vs lost is the
  difference between queueing/retry disease and vanished-packet disease —
  serial probing makes the distinction unambiguous.

**Adaptive timeout.** 4 × the recent p95 RTT, clamped to 250 ms – 2 s. A drop
on a 5 ms path shouldn't blind the stream for a fixed 2 seconds.

**It doesn't lie.** Monotonic clock only. If the OS stalls the process past a
probe's deadline (scheduler, sleep), the sample is annotated and voided —
never rendered as loss. Runs are bounded by design (default 10 s, max 600):
it's a probe, not a daemon.

**It owns the socket.** ICMP echo via unprivileged datagram sockets — native
on macOS; on Linux enable with:

```
sudo sysctl -w net.ipv4.ping_group_range='0 2147483647'
```

Falls back to a raw socket (root) if dgram is unavailable. Never shells out
to `ping`.

## Usage

```
s80 [options] <target>

  -t, --secs <n>      run duration in seconds (default 10, max 600)
  -c, --count <n>     stop after n probes
  -T, --timeout <ms>  fixed probe timeout (default: adaptive)
      --color <when>  auto | always | never
      --256           force 256-color palette
```

Output is pipeable: when stdout isn't a tty, ANSI is dropped
(`s80 gw | tee incident.log`).

## Build

```
cargo build --release   # → target/release/s80
```

Two dependencies (socket2, libc). No async runtime — serial ping-pong is
naturally synchronous. No GC — a GC pause is the lie this tool exists to
refuse.

## Roadmap

- `--dash`: ratatui dashboard, multi-target panes, side-by-side combs
- ARP ping (isolate the Wi-Fi/L2 hop) — the killer demo is an ARP comb to the
  gateway next to an ICMP comb to the internet: "is it my Wi-Fi or my ISP?"
  answered visually in ten seconds
- HTTP(S) probes for parity with web s80
- Per-hop mode: mtr-style TTL probing, one comb line per hop, with ICMP
  policers detected by their signature (perfectly periodic gaps) and labeled
  instead of shown as fake loss
- IPv6

## License

MIT
