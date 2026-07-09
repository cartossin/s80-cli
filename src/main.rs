//! s80 — terminal-native latency/jitter visualizer.
//!
//! Self-clocked ping-pong (the Cisco CMTS pinger, reborn): ONE probe in
//! flight, the next sent the instant the reply lands. Can't flood by
//! construction; the output rate IS the RTT. `!` reply (colored by RTT),
//! `.` timeout, `,` late reply (repainted in place). Monotonic clock only.
//! s80 doesn't lie.

mod color;
mod icmp;
mod probe;
mod stats;
mod term;
mod udp;

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_COUNT: u64 = 1000;
const STALL_SLOP: Duration = Duration::from_millis(300);
// timeout autotuner: start at -T (or 1 s), grow 1.5x on each timeout so a
// short -T can never turn the probe into a fixed-rate blaster, re-anchor
// to TIMEOUT_MULT x recent p95 whenever replies flow
const TIMEOUT_INITIAL: Duration = Duration::from_millis(1000);
const TIMEOUT_FLOOR: Duration = Duration::from_millis(250);
const TIMEOUT_CEIL: Duration = Duration::from_millis(2000);
const TIMEOUT_MULT: f64 = 4.0;
const LATE_WINDOW: Duration = Duration::from_secs(10);
// UDP auto-pacing: grow delay ~1.5x per drop, decay 10% per clean streak
const PACE_STEP_MIN: Duration = Duration::from_millis(10);
const PACE_CAP: Duration = Duration::from_secs(2);
const PACE_DECAY_STREAK: u32 = 20;
// below this, sleeping can't hit the mark (OS timer slack) — spin instead
const SPIN_MAX: Duration = Duration::from_millis(1);

static INTR: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    if INTR.swap(true, Ordering::SeqCst) {
        // second ^C: user means it
        unsafe { libc::_exit(130) };
    }
}

extern "C" fn on_sigwinch(_: libc::c_int) {
    term::WINCH.store(true, Ordering::Relaxed);
}

struct Args {
    target: String,
    secs: Option<f64>,
    count: Option<u64>,
    delay: Duration,
    fixed_timeout: Option<Duration>,
    color: ColorChoice,
    force_256: bool,
    udp: bool,
    port: u16,
}

enum ColorChoice {
    Auto,
    Always,
    Never,
}

const USAGE: &str = "\
usage: s80 [options] <target>

  -c, --count <n>     stop after n probes (default 1000; 0 = unlimited)
  -t, --secs <n>      stop after n seconds instead (0 = unlimited)
  -d, --delay <ms>    delay between probes in milliseconds
  -T, --timeout <ms>  starting probe timeout (autotuned from there: grows
                      while probes time out, tracks the path once replies flow)
  -u, --udp           use UDP probes instead of ICMP
      --port <n>      UDP destination port (default 33434)
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
        secs: None,
        count: None,
        delay: Duration::ZERO,
        fixed_timeout: None,
        color: ColorChoice::Auto,
        force_256: false,
        udp: false,
        port: udp::DEFAULT_PORT,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .ok_or_else(|| format!("s80: {name} needs a value"))
        };
        match a.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("s80 {VERSION}");
                std::process::exit(0);
            }
            "-t" | "--secs" => {
                let secs = val("-t")?
                    .parse::<f64>()
                    .map_err(|_| "s80: -t wants a number of seconds")?;
                if secs < 0.0 || !secs.is_finite() || Duration::try_from_secs_f64(secs).is_err() {
                    return Err(
                        "s80: -t wants a non-negative, non-astronomical number of seconds".into(),
                    );
                }
                args.secs = Some(secs);
            }
            "-c" | "--count" => {
                args.count = Some(
                    val("-c")?
                        .parse::<u64>()
                        .map_err(|_| "s80: -c wants a probe count")?,
                );
            }
            "-d" | "--delay" => {
                let ms = val("-d")?
                    .parse::<f64>()
                    .map_err(|_| "s80: -d wants milliseconds (fractional ok)")?;
                if ms < 0.0 || !ms.is_finite() {
                    return Err("s80: -d wants a non-negative delay".into());
                }
                // 1 µs is the smallest honest gap: the spin-wait holds a
                // mark to ~±50 ns, but below 1 µs the gap is smaller than
                // the send syscall's own jitter — it would change nothing
                if ms > 0.0 && ms < 0.001 {
                    return Err(
                        "s80: smallest nonzero -d is 0.001 (1 µs) — below that a gap \
                         disappears into syscall jitter, and s80 doesn't pretend"
                            .into(),
                    );
                }
                args.delay = Duration::try_from_secs_f64(ms / 1000.0)
                    .map_err(|_| "s80: -d is too large to be a delay")?;
            }
            "-T" | "--timeout" => {
                let ms: u64 = val("-T")?
                    .parse()
                    .map_err(|_| "s80: -T wants milliseconds")?;
                if !(10..=10_000).contains(&ms) {
                    return Err("s80: -T must be 10..10000 ms".into());
                }
                args.fixed_timeout = Some(Duration::from_millis(ms));
            }
            "--color" => {
                args.color = match val("--color")?.as_str() {
                    "auto" => ColorChoice::Auto,
                    "always" => ColorChoice::Always,
                    "never" => ColorChoice::Never,
                    other => return Err(format!("s80: unknown --color '{other}'")),
                };
            }
            "-u" | "--udp" => args.udp = true,
            "--port" => {
                args.port = val("--port")?
                    .parse()
                    .ok()
                    .filter(|&p| p > 0)
                    .ok_or("s80: --port wants a port number, 1-65535")?;
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
        print_help();
        std::process::exit(2);
    }
    if args.count.is_none() && args.secs.is_none() {
        args.count = Some(DEFAULT_COUNT);
    }
    Ok(args)
}

fn resolve(target: &str) -> std::io::Result<SocketAddr> {
    format!("{target}:0")
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
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
        let mut sw: libc::sigaction = std::mem::zeroed();
        sw.sa_sigaction = on_sigwinch as usize;
        libc::sigaction(libc::SIGWINCH, &sw, std::ptr::null_mut());
    }

    let dest = resolve(&args.target)?;
    let mut prober: Box<dyn probe::Prober> = if args.udp {
        Box::new(udp::UdpProber::new(dest, args.port)?)
    } else {
        Box::new(icmp::Pinger::new(dest)?)
    };

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
    let mut strip = term::Strip::new(ansi, mode);
    let color_mode = mode;
    let mut stats = stats::Stats::new();
    let mut probes: HashMap<u32, Probe> = HashMap::new();

    let banner_mode = if args.udp {
        format!(" [udp:{}]", args.port)
    } else {
        String::new()
    };
    println!(
        "s80 {} ({}){} — ^C for stats",
        args.target,
        dest.ip(),
        banner_mode
    );

    let start = Instant::now();
    // 0 for either bound means unlimited
    let count = args.count.filter(|&c| c > 0);
    let end = args
        .secs
        .filter(|&s| s > 0.0)
        .map(|s| start + Duration::from_secs_f64(s));
    let mut seq: u32 = 0;

    // timeout autotuner: -T is only the starting value. Growth on timeouts
    // keeps a short -T from turning the self-clock into a fixed-rate
    // blaster (can't-flood-by-construction survives any flag values).
    let start_timeout = args.fixed_timeout.unwrap_or(TIMEOUT_INITIAL);
    let tune_floor = TIMEOUT_FLOOR.min(start_timeout);
    let tune_ceil = TIMEOUT_CEIL.max(start_timeout);
    let mut cur_timeout = start_timeout;

    // UDP auto-pacing state (see wait_gap / USAGE)
    let mut auto_delay = Duration::ZERO;
    let mut pace_peak = Duration::ZERO;
    let mut clean_streak: u32 = 0;
    let mut pace_noted = false;
    let mut send_err_noted = false;

    enum Outcome {
        Replied,
        Lost,
        Voided,
    }

    'run: while !INTR.load(Ordering::SeqCst)
        && end.is_none_or(|e| Instant::now() < e)
        && count.is_none_or(|c| stats.sent < c)
    {
        // stamp BEFORE send, like classic ping: on loopback and same-host
        // virtual networks the whole round trip can complete inside the
        // send syscall — stamping after would hide real transit and report
        // impossibly small RTTs. Never understate.
        let sent_at = Instant::now();
        if let Err(e) = prober.send(seq) {
            if !probe::is_transient(&e) {
                return Err(e);
            }
            // the local stack can't send (route flap, interface down,
            // rate-limit): that says nothing about the path. Void it,
            // note once per outage, and wait before retrying — the run
            // must survive the incident it exists to document.
            stats.sent += 1;
            stats.voided += 1;
            if !send_err_noted {
                strip.note(&format!("[send error: {e} — pausing]"));
                send_err_noted = true;
            }
            wait_gap(prober.as_mut(), cur_timeout, &mut probes, &mut stats, &mut strip)?;
            seq = seq.wrapping_add(1);
            continue;
        }
        send_err_noted = false;
        stats.sent += 1;
        probes.insert(
            seq,
            Probe {
                sent: sent_at,
                pos: None,
            },
        );
        let deadline = sent_at + cur_timeout;

        let outcome = loop {
            match prober.recv(deadline)? {
                probe::Recv::Reply { seq: rseq, at } => {
                    if rseq == seq {
                        let rtt = (at - sent_at).as_secs_f64() * 1000.0;
                        stats.record_rtt(rtt);
                        strip.put('!', Some(color::rtt_rgb(rtt)));
                        probes.remove(&seq);
                        break Outcome::Replied; // reply landed: next probe NOW
                    }
                    handle_late(rseq, at, &mut probes, &mut stats, &mut strip);
                }
                probe::Recv::TimedOut { overshoot } => {
                    if overshoot > STALL_SLOP {
                        // the OS held us past the deadline — this sample
                        // is compromised. Annotate; never render it as loss.
                        stats.voided += 1;
                        probes.remove(&seq);
                        strip.note(&format!(
                            "[stall {}ms — sample voided]",
                            overshoot.as_millis()
                        ));
                        break Outcome::Voided;
                    }
                    stats.lost += 1;
                    let pos = strip.put('.', None);
                    if let Some(p) = probes.get_mut(&seq) {
                        p.pos = Some(pos);
                    }
                    break Outcome::Lost;
                }
                probe::Recv::Interrupted => {
                    if INTR.load(Ordering::SeqCst) {
                        break 'run;
                    }
                }
            }
        };

        // tune the timeout: replies re-anchor it to the path, timeouts grow
        // it (a lost probe means we don't know the path — get more patient)
        match outcome {
            Outcome::Replied => {
                if let Some(p95) = stats.recent_p95() {
                    cur_timeout = Duration::from_secs_f64(p95 * TIMEOUT_MULT / 1000.0)
                        .clamp(tune_floor, tune_ceil);
                }
            }
            Outcome::Lost => cur_timeout = (cur_timeout * 3 / 2).min(tune_ceil),
            Outcome::Voided => {}
        }

        // UDP auto-pacing: devices rate-limit unreachables, so drops mean
        // "slower" more often than "lost". Grow the gap on each drop until
        // replies hold; decay it on clean streaks to re-probe the limit.
        // (Voided samples say nothing about the path — they don't steer.)
        if args.udp {
            match outcome {
                Outcome::Replied => {
                    clean_streak += 1;
                    if clean_streak >= PACE_DECAY_STREAK {
                        auto_delay = auto_delay * 9 / 10;
                        clean_streak = 0;
                    }
                }
                Outcome::Lost => {
                    clean_streak = 0;
                    auto_delay = (auto_delay + (auto_delay / 2).max(PACE_STEP_MIN)).min(PACE_CAP);
                    if !pace_noted {
                        strip.note("[drops on udp — auto-pacing engaged]");
                        pace_noted = true;
                    }
                }
                Outcome::Voided => {}
            }
            pace_peak = pace_peak.max(auto_delay);
        }

        // retire timed-out probes past the late window: they stay lost
        probes.retain(|_, p| p.sent.elapsed() < LATE_WINDOW);
        seq = seq.wrapping_add(1);

        let gap = args.delay.max(auto_delay);
        let more = !INTR.load(Ordering::SeqCst)
            && end.is_none_or(|e| Instant::now() < e)
            && count.is_none_or(|c| stats.sent < c);
        if !gap.is_zero() && more {
            wait_gap(prober.as_mut(), gap, &mut probes, &mut stats, &mut strip)?;
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let pace = if pace_peak > Duration::ZERO {
        Some((auto_delay, pace_peak))
    } else {
        None
    };
    print_footer(args, dest, &stats, elapsed, pace, color_mode, cur_timeout);
    Ok(stats.replies() > 0)
}

/// A reply to an older, timed-out probe: late, not lost.
fn handle_late(
    rseq: u32,
    at: Instant,
    probes: &mut HashMap<u32, Probe>,
    stats: &mut stats::Stats,
    strip: &mut term::Strip,
) {
    if let Some(p) = probes.remove(&rseq) {
        let rtt = (at - p.sent).as_secs_f64() * 1000.0;
        stats.lost_becomes_late(rtt);
        if let Some(pos) = p.pos {
            strip.repaint(pos, ',', Some(color::rtt_rgb(rtt)));
        }
    }
    // unknown seq: not ours to interpret
}

/// Hold the inter-probe gap. Short gaps spin (the OS can't wake a sleeper
/// on a microsecond mark); longer gaps keep listening on the socket, so
/// late replies are timestamped on arrival and ^C stays responsive.
fn wait_gap(
    prober: &mut dyn probe::Prober,
    gap: Duration,
    probes: &mut HashMap<u32, Probe>,
    stats: &mut stats::Stats,
    strip: &mut term::Strip,
) -> std::io::Result<()> {
    let until = Instant::now() + gap;
    // listen until SPIN_MAX before the mark (socket wakeups carry ~1 ms of
    // timer slack), then spin the final stretch to land on the microsecond
    if gap >= SPIN_MAX {
        let listen_until = until - SPIN_MAX;
        loop {
            if INTR.load(Ordering::SeqCst) {
                return Ok(());
            }
            match prober.recv(listen_until)? {
                probe::Recv::Reply { seq: rseq, at } => handle_late(rseq, at, probes, stats, strip),
                probe::Recv::TimedOut { .. } => break,
                probe::Recv::Interrupted => {}
            }
        }
    }
    while Instant::now() < until {
        std::hint::spin_loop();
    }
    Ok(())
}

/// Help gets the banner: ascii "s80" plus ticks swept 0 -> 1500 ms through
/// the actual colormap (log-spaced so the whole wheel shows). Colored only
/// when stdout is a tty that can take it.
fn print_help() {
    const ART: &str = r" ____    ___    ___
/ ___|  ( _ )  / _ \
\___ \  / _ \ | | | |
 ___) || (_) || |_| |
|____/  \___/  \___/";
    let is_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1;
    let truecolor = std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false);
    let paint = |glyph: char, (r, g, b): (u8, u8, u8)| -> String {
        if !is_tty {
            glyph.to_string()
        } else if truecolor {
            format!("\x1b[38;2;{};{};{}m{}\x1b[0m", r, g, b, glyph)
        } else {
            format!("\x1b[38;5;{}m{}\x1b[0m", color::rgb_to_256(r, g, b), glyph)
        }
    };
    println!("{ART}");
    const SWEEP: usize = 63;
    let mut strip = String::new();
    for i in 0..SWEEP {
        // log-spaced from the 10 µs floor to 1500 ms
        let ms = 0.01 * (1500.0_f64 / 0.01).powf(i as f64 / (SWEEP - 1) as f64);
        strip.push_str(&paint('!', color::rtt_rgb(ms)));
    }
    println!("{strip}  0 -> 1500 ms\n");
    println!("{USAGE}");
}

/// Milliseconds, always at the microsecond precision the tool aims for.
fn fmt_ms(v: f64) -> String {
    format!("{:.3}", v)
}

fn print_footer(
    args: &Args,
    dest: SocketAddr,
    stats: &stats::Stats,
    elapsed: f64,
    pace: Option<(Duration, Duration)>,
    mode: term::ColorMode,
    timeout: Duration,
) {
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
        Some((min, avg, p95, max)) => {
            // each stat wears the color its RTT would get as a '!'
            let c = |v: f64| term::paint(&fmt_ms(v), color::rtt_rgb(v), mode);
            println!(
                "{} probes  {} replies  min/avg/p95/max = {}/{}/{}/{} ms",
                stats.sent,
                stats.replies(),
                c(min),
                c(avg),
                c(p95),
                c(max)
            )
        }
        None => println!("{} probes  0 replies", stats.sent),
    }
    let voided = if stats.voided > 0 {
        format!("  voided {}", stats.voided)
    } else {
        String::new()
    };
    println!(
        "late {} ({:.2}%)  lost {} ({:.2}%){}  elapsed {:.3}s  rate {:.0}/s  timeout {}ms (autotuned)",
        stats.late,
        pct(stats.late),
        stats.lost,
        pct(stats.lost),
        voided,
        elapsed,
        stats.sent as f64 / elapsed.max(0.001),
        fmt_ms(timeout.as_secs_f64() * 1000.0)
    );
    if let Some((current, peak)) = pace {
        println!(
            "auto-pace settled at {}ms (peak {}ms) — drops triggered pacing; \
             they still count as lost above",
            fmt_ms(current.as_secs_f64() * 1000.0),
            fmt_ms(peak.as_secs_f64() * 1000.0)
        );
    }
}
