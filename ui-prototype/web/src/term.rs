//! The terminal core: bytes from the shell go in, a grid of cells ready to
//! draw comes out. Nothing outside this file knows which emulator is used.
//!
//! STAND-IN: this is the `vt100` crate, not Ghostty. The Ghostty bindings
//! build for wasm32 only from an unreleased commit (see README.md), so the
//! page runs on a pure-Rust core until that build is approved. Swapping is
//! this file: `Term` keeps its methods.

use std::num::NonZeroU16;

pub type Rgb = (u8, u8, u8);

pub const FG: Rgb = (0xd8, 0xd8, 0xd8);
pub const BG: Rgb = (0x10, 0x10, 0x14);

/// One cell, colours resolved (inverse already applied).
pub struct Cell<'a> {
    /// Empty for a blank cell.
    pub text: &'a str,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Occupies this column and the next.
    pub wide: bool,
}

pub struct Term(vt100::Parser);

impl Term {
    pub fn new(cols: u16, rows: u16) -> Term {
        Term(vt100::Parser::new(size(rows), size(cols), 0))
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.0.set_size(size(rows), size(cols));
    }

    pub fn write(&mut self, bytes: &[u8]) {
        self.0.process(bytes);
    }

    /// `None` for the second column of a wide character.
    pub fn cell(&self, row: u16, col: u16) -> Option<Cell<'_>> {
        let c = self.0.screen().cell(row, col)?;
        if c.is_wide_continuation() {
            return None;
        }
        let (mut fg, mut bg) = (rgb(c.fgcolor(), FG), rgb(c.bgcolor(), BG));
        if c.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        Some(Cell {
            text: c.contents(),
            fg,
            bg,
            bold: c.bold(),
            italic: c.italic(),
            underline: c.underline(),
            wide: c.is_wide(),
        })
    }

    /// `(row, col)`, or `None` while the program has hidden the cursor.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let screen = self.0.screen();
        (!screen.hide_cursor()).then(|| screen.cursor_position())
    }

    /// The program asked for application cursor keys (DECCKM).
    pub fn application_cursor(&self) -> bool {
        self.0.screen().application_cursor()
    }
}

fn size(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).expect("a terminal has at least one cell")
}

fn rgb(color: vt100::Color, default: Rgb) -> Rgb {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => palette(i),
        vt100::Color::Rgb(r, g, b) => (r, g, b),
    }
}

/// The xterm 256-colour palette.
fn palette(i: u8) -> Rgb {
    const BASE: [Rgb; 16] = [
        (0x1d, 0x1f, 0x21),
        (0xcc, 0x66, 0x66),
        (0xb5, 0xbd, 0x68),
        (0xf0, 0xc6, 0x74),
        (0x81, 0xa2, 0xbe),
        (0xb2, 0x94, 0xbb),
        (0x8a, 0xbe, 0xb7),
        (0xc5, 0xc8, 0xc6),
        (0x66, 0x66, 0x66),
        (0xd5, 0x4e, 0x53),
        (0xb9, 0xca, 0x4a),
        (0xe7, 0xc5, 0x47),
        (0x7a, 0xa6, 0xda),
        (0xc3, 0x97, 0xd8),
        (0x70, 0xc0, 0xb1),
        (0xea, 0xea, 0xea),
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let level = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            let n = i - 16;
            (level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        232..=255 => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_attributes_reach_the_cell() {
        let mut t = Term::new(20, 4);
        t.write(b"\x1b[1;38;2;255;0;0mR\x1b[0m\x1b[7mi\x1b[0m\x1b[38;5;196mx");
        let r = t.cell(0, 0).unwrap();
        assert_eq!((r.text, r.fg, r.bold), ("R", (255, 0, 0), true));
        let i = t.cell(0, 1).unwrap();
        assert_eq!((i.fg, i.bg), (BG, FG));
        assert_eq!(t.cell(0, 2).unwrap().fg, (255, 0, 0));
        assert_eq!(t.cursor(), Some((0, 3)));
    }

    #[test]
    fn a_wide_character_takes_two_columns() {
        let mut t = Term::new(20, 4);
        t.write("漢a".as_bytes());
        assert!(t.cell(0, 0).unwrap().wide);
        assert!(t.cell(0, 1).is_none());
        assert_eq!(t.cell(0, 2).unwrap().text, "a");
    }

    #[test]
    fn resizing_keeps_what_still_fits() {
        let mut t = Term::new(20, 4);
        t.write(b"abc");
        t.resize(40, 10);
        assert_eq!(t.cell(0, 2).unwrap().text, "c");
        assert!(t.cell(9, 39).is_some());
        assert!(t.cell(10, 0).is_none());
    }

    #[test]
    fn a_hidden_cursor_is_not_reported() {
        let mut t = Term::new(20, 4);
        t.write(b"\x1b[?25l");
        assert_eq!(t.cursor(), None);
    }
}
