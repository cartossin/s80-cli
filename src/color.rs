//! RTT → color, ported from s80.me: log-scale hue wheel, green (~1 ms)
//! through yellow/orange to red (~1.5 s). HSL with s=1, l=0.5.

static COLORMAP: std::sync::OnceLock<[(u8, u8, u8); 1500]> = std::sync::OnceLock::new();
static SUBMS_MAP: std::sync::OnceLock<[(u8, u8, u8); 100]> = std::sync::OnceLock::new();

/// Map an RTT in milliseconds to an RGB color on the s80 wheel.
/// Table lookup: precomputed per integer ms above 1 ms (the web range),
/// per 10 µs below it, where the same log formula continues the wheel
/// green -> cyan -> blue. Floor at 10 µs keeps the hue from wrapping
/// back toward red.
pub fn rtt_rgb(rtt_ms: f64) -> (u8, u8, u8) {
    if rtt_ms < 1.0 {
        let map = SUBMS_MAP.get_or_init(|| {
            let mut m = [(0, 0, 0); 100];
            for (k, slot) in m.iter_mut().enumerate().skip(1) {
                *slot = compute_rgb(k as f64 * 0.01);
            }
            m[0] = m[1];
            m
        });
        map[((rtt_ms * 100.0).round() as usize).min(99)]
    } else {
        let map = COLORMAP.get_or_init(|| {
            let mut m = [(0, 0, 0); 1500];
            for (x, slot) in m.iter_mut().enumerate().skip(1) {
                *slot = compute_rgb(x as f64);
            }
            m[0] = m[1];
            m
        });
        map[(rtt_ms.round() as usize).min(1499)]
    }
}

fn compute_rgb(x: f64) -> (u8, u8, u8) {
    // web: i = round((100 - (ln(x)/6)*100) + 30); hue = i * 1.2 / 360
    // (the web clamps i to its 1..1499 ms domain; here the table bounds x)
    let i = (100.0 - (x.ln() / 6.0) * 100.0 + 30.0).round();
    let hue = i * 1.2 / 360.0;
    hsl_to_rgb(hue, 1.0, 0.5)
}

/// Nearest color in the xterm 256-color 6x6x6 cube.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let q = |v: u8| -> u8 {
        // cube levels: 0, 95, 135, 175, 215, 255
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as u16 - 35) / 40) as u8
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let (r, g, b);
    if s == 0.0 {
        r = l;
        g = l;
        b = l;
    } else {
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        r = hue_to_rgb(p, q, h + 1.0 / 3.0);
        g = hue_to_rgb(p, q, h);
        b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    }
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_ms_is_spring_green() {
        // web: x=1 → i=130 → hue 156° → rgb(0, 255, 153)
        assert_eq!(rtt_rgb(1.0), (0, 255, 153));
    }

    #[test]
    fn slow_is_red() {
        let (r, g, b) = rtt_rgb(1499.0);
        assert!(r == 255 && g < 60 && b == 0, "got {:?}", (r, g, b));
    }

    #[test]
    fn clamps() {
        assert_eq!(rtt_rgb(9999.0), rtt_rgb(1499.0));
    }

    #[test]
    fn table_quantizes_to_integer_ms_like_web() {
        assert_eq!(rtt_rgb(4.4), rtt_rgb(4.0));
        assert_eq!(rtt_rgb(4.6), rtt_rgb(5.0));
        assert_eq!(rtt_rgb(700.0), compute_rgb(700.0));
    }

    #[test]
    fn sub_ms_extends_into_blue() {
        // 60 µs LAN reply → hue ~212°, azure
        assert_eq!(rtt_rgb(0.06), (0, 117, 255));
        // 10 µs floor: everything below clamps to it, hue never wraps red
        assert_eq!(rtt_rgb(0.001), rtt_rgb(0.01));
        let (r, _, b) = rtt_rgb(0.01);
        assert!(b == 255 && r < 128, "floor color should stay blue");
        // seamless at the 1 ms boundary
        assert_eq!(rtt_rgb(0.995), rtt_rgb(1.0));
    }
}
