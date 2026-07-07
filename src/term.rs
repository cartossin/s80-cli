//! The tick strip: glyph stream with wrap tracking, so a late reply can
//! retroactively repaint its `.` into a `,` via cursor escapes.

use std::io::{self, Write};

#[derive(Clone, Copy, PartialEq)]
pub enum ColorMode {
    Truecolor,
    C256,
    None,
}

#[derive(Clone, Copy)]
pub struct GlyphPos {
    row: u64,
    col: u16,
}

/// Wrap `text` in the foreground escape for `rgb` under `mode`
/// (plain text when color is off).
pub fn paint(text: &str, rgb: (u8, u8, u8), mode: ColorMode) -> String {
    match mode {
        ColorMode::Truecolor => {
            format!("\x1b[38;2;{};{};{}m{}\x1b[0m", rgb.0, rgb.1, rgb.2, text)
        }
        ColorMode::C256 => format!(
            "\x1b[38;5;{}m{}\x1b[0m",
            crate::color::rgb_to_256(rgb.0, rgb.1, rgb.2),
            text
        ),
        ColorMode::None => text.to_string(),
    }
}

pub struct Strip {
    out: io::Stdout,
    ansi: bool,
    mode: ColorMode,
    width: u16,
    height: u16,
    col: u16,
    row: u64, // absolute row index of the line the cursor is on
}

impl Strip {
    pub fn new(ansi: bool, mode: ColorMode) -> Self {
        let (width, height) = winsize().unwrap_or((80, 24));
        Strip {
            out: io::stdout(),
            ansi,
            mode: if ansi { mode } else { ColorMode::None },
            width: width.max(10),
            height: height.max(2),
            col: 0,
            row: 0,
        }
    }

    /// Print one glyph at the cursor, advancing (and wrapping) the strip.
    /// Returns where it landed so it can be repainted later.
    pub fn put(&mut self, glyph: char, rgb: Option<(u8, u8, u8)>) -> GlyphPos {
        let pos = GlyphPos {
            row: self.row,
            col: self.col,
        };
        let mut buf = String::with_capacity(24);
        self.push_colored(&mut buf, glyph, rgb);
        self.col += 1;
        if self.col >= self.width {
            buf.push('\n');
            self.col = 0;
            self.row += 1;
        }
        let _ = self.out.write_all(buf.as_bytes());
        let _ = self.out.flush();
        pos
    }

    /// Repaint an earlier glyph in place (late reply: `.` becomes `,`).
    /// Silently skips if the position has scrolled off the visible screen
    /// or we're not talking to a terminal — the tally still counts it.
    pub fn repaint(&mut self, pos: GlyphPos, glyph: char, rgb: Option<(u8, u8, u8)>) {
        if !self.ansi {
            return;
        }
        let dy = self.row - pos.row;
        if dy >= self.height as u64 {
            return;
        }
        let mut buf = String::with_capacity(32);
        buf.push_str("\x1b7"); // save cursor
        if dy > 0 {
            buf.push_str(&format!("\x1b[{}A", dy));
        }
        buf.push_str(&format!("\x1b[{}G", pos.col + 1));
        self.push_colored(&mut buf, glyph, rgb);
        buf.push_str("\x1b8"); // restore cursor
        let _ = self.out.write_all(buf.as_bytes());
        let _ = self.out.flush();
    }

    /// Out-of-band annotation (e.g. a detected stall). Breaks the strip.
    pub fn note(&mut self, text: &str) {
        let mut buf = String::new();
        if self.col > 0 {
            buf.push('\n');
            self.row += 1;
            self.col = 0;
        }
        buf.push_str(text);
        buf.push('\n');
        self.row += 1;
        let _ = self.out.write_all(buf.as_bytes());
        let _ = self.out.flush();
    }

    fn push_colored(&self, buf: &mut String, glyph: char, rgb: Option<(u8, u8, u8)>) {
        match (self.mode, rgb) {
            (ColorMode::Truecolor, Some((r, g, b))) => {
                buf.push_str(&format!("\x1b[38;2;{};{};{}m{}\x1b[0m", r, g, b, glyph));
            }
            (ColorMode::C256, Some((r, g, b))) => {
                let n = crate::color::rgb_to_256(r, g, b);
                buf.push_str(&format!("\x1b[38;5;{}m{}\x1b[0m", n, glyph));
            }
            _ => buf.push(glyph),
        }
    }
}

fn winsize() -> Option<(u16, u16)> {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            Some((ws.ws_col, ws.ws_row))
        } else {
            None
        }
    }
}
