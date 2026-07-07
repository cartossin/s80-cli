//! RTT → color, ported from s80.me: log-scale hue wheel, green (~1 ms)
//! through yellow/orange to red (~1.5 s). HSL with s=1, l=0.5.

/// Map an RTT in milliseconds to an RGB color on the s80 wheel.
/// Input is clamped to [1, 1499] ms like the web version.
pub fn rtt_rgb(rtt_ms: f64) -> (u8, u8, u8) {
    let x = rtt_ms.clamp(1.0, 1499.0);
    // web: i = round((100 - (ln(x)/6)*100) + 30); hue = i * 1.2 / 360
    let i = (100.0 - (x.ln() / 6.0) * 100.0 + 30.0).clamp(0.0, 130.0);
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
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
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
        assert_eq!(rtt_rgb(0.01), rtt_rgb(1.0));
        assert_eq!(rtt_rgb(9999.0), rtt_rgb(1499.0));
    }
}
