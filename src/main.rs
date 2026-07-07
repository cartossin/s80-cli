//! s80 — terminal-native latency/jitter visualizer.
//!
//! Self-clocked ping-pong (the Cisco CMTS pinger, reborn): ONE probe in
//! flight, the next sent the instant the reply lands. Can't flood by
//! construction; the output rate IS the RTT. `!` reply (colored by RTT),
//! `.` timeout, `,` late reply (repainted in place). Monotonic clock only.
//! s80 doesn't lie.

mod color;
mod icmp;
mod stats;
mod term;

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_SECS: f64 = 10.0;
const MAX_SECS: f64 = 600.0;
const STALL_SLOP: Duration = Duration::from_millis(300);
const LATE_WINDOW: Duration = Duration::from_secs(10);

static INTR: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    if INTR.swap(true, Ordering::SeqCst) {
        // second ^C: user means it
        unsafe { libc::_exit(130) };
    }
}

struct Args {
    target: String,
    secs: f64,
    count: Option<u64>,
    fixed_timeout: Option<Duration>,
    color: ColorChoice,
    force_256: bool,
}

#[derive(PartialEq)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

const USAGE: &str = "\
s80 — terminal latency/jitter visualizer (self-clocked ICMP ping-pong)

usage: s80 [options] <target>

  -t, --secs <n>      run duration in seconds (default 10, max 600)
  -c, --count <n>     stop after n probes
  -T, --timeout <ms>  fixed probe timeout (default: adaptive, 4 x recent p95)
      --color <when>  auto | always | never (default auto)
      --256           force 256-color palette (default: truecolor if COLORTERM)
  -V, --version       print version
  -h, --help          this text

glyphs:  '!' reply, colored blue (us) -> green (~1ms) -> red (slow), log scale
         '.' timeout   ',' late reply that arrived after its timeout
                           (the '.' is repainted in place when possible)";

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    match run(&args) {
        Ok(had_replies) => std::process::exit(if had_replies { 0 } else { 1 }),
        Err(e) => {
            eprintln!("s80: {e}");
            std::process::exit(1);
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        target: String::new(),
        secs: DEFAULT_SECS,
        count: None,
        fixed_timeout: None,
        color: ColorChoice::Auto,
        force_256: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .ok_or_else(|| format!("s80: {name} needs a value"))
        };
        match a.as_str() {
            "-h" | "--help" => return Err(USAGE.to_string()),
            "-V" | "--version" => return Err(format!("s80 {VERSION}")),
            "-t" | "--secs" => {
                args.secs = val("-t")?
                    .parse::<f64>()
                    .map_err(|_| "s80: -t wants a number of seconds")?;
                if !(0.1..=MAX_SECS).contains(&args.secs) {
                    return Err(format!(
                        "s80: -t must be 0.1..{MAX_SECS} seconds \
                         (bounded runs by design — it's a probe, not a daemon)"
                    ));
                }
            }
            "-c" | "--count" => {
                args.count = Some(
                    val("-c")?
                        .parse::<u64>()
                        .map_err(|_| "s80: -c wants a probe count")?,
                );
            }
            "-T" | "--timeout" => {
                let ms: u64 = val("-T")?
                    .parse()
                    .map_err(|_| "s80: -T wants milliseconds")?;
                args.fixed_timeout = Some(Duration::from_millis(ms.clamp(10, 10_000)));
            }
            "--color" => {
                args.color = match val("--color")?.as_str() {
                    "auto" => ColorChoice::Auto,
                    "always" => ColorChoice::Always,
                    "never" => ColorChoice::Never,
                    other => return Err(format!("s80: unknown --color '{other}'")),
                };
            }
            "--256" => args.force_256 = true,
            other if other.starts_with('-') => {
                return Err(format!("s80: unknown option '{other}'\n\n{USAGE}"))
            }
            other => {
                if !args.target.is_empty() {
                    return Err("s80: one target at a time (multi-target is coming)".into());
                }
                args.target = other.to_string();
            }
        }
    }
    if args.target.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(args)
}

fn resolve(target: &str) -> std::io::Result<SocketAddr> {
    let addrs = format!("{target}:0").to_socket_addrs()?;
    addrs
        .filter(|a| a.is_ipv4())
        .next()
        .ok_or_else(|| std::io::Error::other("no IPv4 address for target (IPv6: soon)"))
}

struct Probe {
    sent: Instant,
    pos: Option<term::GlyphPos>, // set once its '.' is on screen
}

fn run(args: &Args) -> std::io::Result<bool> {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigint as usize;
        // sa_flags deliberately 0: no SA_RESTART, so a blocking recv
        // returns EINTR and the footer prints immediately on ^C
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }

    let dest = resolve(&args.target)?;
    let mut pinger = icmp::Pinger::new(dest)?;

    let is_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1;
    let ansi = match args.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => is_tty,
    };
    let truecolor = std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false);
    let mode = if !ansi {
        term::ColorMode::None
    } else if args.force_256 || !truecolor {
        term::ColorMode::C256
    } else {
        term::ColorMode::Truecolor
    };
    let mut comb = term::Comb::new(ansi, mode);
    let mut stats = stats::Stats::new();
    let mut probes: HashMap<u16, Probe> = HashMap::new();

    println!("s80 {} ({}) — ^C for stats", args.target, dest.ip());

    let start = Instant::now();
    let end = start + Duration::from_secs_f64(args.secs);
    let mut seq: u16 = 0;

    'run: while !INTR.load(Ordering::SeqCst)
        && Instant::now() < end
        && args.count.map_or(true, |c| stats.sent < c)
    {
        pinger.send(seq)?;
        let sent_at = Instant::now();
        stats.sent += 1;
        probes.insert(seq, Probe { sent: sent_at, pos: None });
        let timeout = args.fixed_timeout.unwrap_or_else(|| stats.timeout());
        let deadline = sent_at + timeout;

        loop {
            match pinger.recv(deadline)? {
                icmp::Recv::Reply { seq: rseq, at } => {
                    if rseq == seq {
                        let rtt = (at - sent_at).as_secs_f64() * 1000.0;
                        stats.record_rtt(rtt);
                        comb.put('!', Some(color::rtt_rgb(rtt)));
                        probes.remove(&seq);
                        break; // reply landed: send the next probe NOW
                    }
                    // a reply to an older, timed-out probe: late, not lost
                    if let Some(p) = probes.remove(&rseq) {
                        let rtt = (at - p.sent).as_secs_f64() * 1000.0;
                        stats.lost_becomes_late(rtt);
                        if let Some(pos) = p.pos {
                            comb.repaint(pos, ',', Some(color::rtt_rgb(rtt)));
                        }
                    }
                    // unknown seq: not ours to interpret; keep waiting
                }
                icmp::Recv::TimedOut { overshoot } => {
                    if overshoot > STALL_SLOP {
                        // the OS held us past the deadline — this sample
                        // is compromised. Annotate; never render it as loss.
                        stats.voided += 1;
                        probes.remove(&seq);
                        comb.note(&format!(
                            "[stall {}ms — sample voided]",
                            overshoot.as_millis()
                        ));
                    } else {
                        stats.lost += 1;
                        let pos = comb.put('.', None);
                        if let Some(p) = probes.get_mut(&seq) {
                            p.pos = Some(pos);
                        }
                    }
                    break;
                }
                icmp::Recv::Interrupted => {
                    if INTR.load(Ordering::SeqCst) {
                        break 'run;
                    }
                }
            }
        }

        // retire timed-out probes past the late window: they stay lost
        probes.retain(|_, p| p.sent.elapsed() < LATE_WINDOW);
        seq = seq.wrapping_add(1);
    }

    let elapsed = start.elapsed().as_secs_f64();
    print_footer(args, dest, &stats, elapsed);
    Ok(stats.replies() > 0)
}

/// Milliseconds with microsecond resolution below 1 ms.
fn fmt_ms(v: f64) -> String {
    if v < 1.0 {
        format!("{:.3}", v)
    } else {
        format!("{:.2}", v)
    }
}

fn print_footer(args: &Args, dest: SocketAddr, stats: &stats::Stats, elapsed: f64) {
    let completed = stats.replies() + stats.lost;
    let pct = |n: u64| {
        if completed == 0 {
            0.0
        } else {
            n as f64 * 100.0 / completed as f64
        }
    };
    println!("\n--- s80 {} ({}) ---", args.target, dest.ip());
    match stats.summary() {
        Some((min, avg, p95, max)) => println!(
            "{} probes  {} replies  min/avg/p95/max = {}/{}/{}/{} ms",
            stats.sent,
            stats.replies(),
            fmt_ms(min),
            fmt_ms(avg),
            fmt_ms(p95),
            fmt_ms(max)
        ),
        None => println!("{} probes  0 replies", stats.sent),
    }
    let voided = if stats.voided > 0 {
        format!("  voided {}", stats.voided)
    } else {
        String::new()
    };
    println!(
        "late {} ({:.2}%)  lost {} ({:.2}%){}  elapsed {:.1}s  rate {:.0}/s",
        stats.late,
        pct(stats.late),
        stats.lost,
        pct(stats.lost),
        voided,
        elapsed,
        stats.sent as f64 / elapsed.max(0.001)
    );
}
