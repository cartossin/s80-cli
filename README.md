# s80

Terminal-native latency/jitter visualizer. Sibling of [s80.me](https://s80.me) —
not a port, a rethink freed from the browser.

```
$ s80 1.1.1.1
s80 1.1.1.1 (1.1.1.1) — ^C for stats
!!!!!!!!!!!!!!!!!.!!!!!!!!!!!!,!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
--- s80 1.1.1.1 (1.1.1.1) ---
194 probes  193 replies  min/avg/p95/max = 10.784/15.412/15.049/100.807 ms
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
never rendered as loss. Runs are bounded by default (1000 probes): it's a
probe, not a daemon — going longer takes an explicit -c or -t (0 = unlimited).

**It owns the socket.** ICMP echo via unprivileged datagram sockets — native
on macOS, and on Linux this normally works out of the box too: systemd has
shipped `ping_group_range` wide-open by default since 2018 (Debian 13,
Ubuntu, Fedora, Arch). Never shells out to `ping`.

Where that default is absent (unprivileged containers silently drop it,
hardened kernels), either restore it or use UDP mode, which needs no
privileges anywhere:

```
sudo sysctl -w net.ipv4.ping_group_range='0 2147483647'   # containers: '0 65535'
s80 -u <target>
```

No raw sockets, no setuid, no capability bits: if the kernel won't hand us
an unprivileged socket, s80 says so and suggests `-u` — it never wants root.

**UDP mode** (`-u`) probes like traceroute does: a datagram to a closed high
port (default 33434) draws an ICMP port-unreachable from the target. On a
connected UDP socket that arrives as `ECONNREFUSED` — a round-trip proof
needing no raw socket and no privileges at all. This reaches hosts that
ignore ICMP echo. Two honest caveats: unreachables carry no sequence number,
so late detection is off (a reply is matched to the probe in flight — always
unambiguous while nothing crosses its own timeout), and many devices
rate-limit unreachables — so UDP mode watches for drops and paces itself:
each drop grows the inter-probe gap (AIMD, like TCP), clean streaks decay it
to re-probe the limit, and the footer reports where the pace settled. Drops
eaten during convergence still count as lost — pacing adapts, it doesn't
launder. `-d` sets the pace floor.

## Usage

```
s80 [options] <target>

  -c, --count <n>     stop after n probes (default 1000; 0 = unlimited)
  -t, --secs <n>      stop after n seconds instead (0 = unlimited)
  -d, --delay <ms>    gap between probes, fractional down to 0.001 (1 us)
  -T, --timeout <ms>  fixed probe timeout (default: adaptive)
  -u, --udp           UDP probes (for hosts that ignore ICMP echo)
      --port <n>      UDP destination port (default 33434)
      --color <when>  auto | always | never
      --256           force 256-color palette
```

**Spaced probes read slower — that's real.** With `-d`, RTTs rise (on some
machines several-fold): between probes the CPU downclocks, caches cool, and
the kernel's hot path goes cold, so each spaced probe pays wake-up costs a
back-to-back probe doesn't. The system's own `ping -i 1` shows it worse.
Dense probing measures your best case; spaced probing measures what
intermittent traffic actually experiences. Both are true.

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

- `--dash`: ratatui dashboard, multi-target panes, side-by-side combs — the
  killer demo is a comb to the gateway next to a comb to the internet:
  "is it my Wi-Fi or my ISP?" answered visually in ten seconds
- Per-hop mode: mtr-style TTL probing, one comb line per hop, with ICMP
  policers detected by their signature (perfectly periodic gaps) and labeled
  instead of shown as fake loss
- IPv6

## License

MIT
