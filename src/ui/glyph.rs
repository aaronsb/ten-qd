//! Layer 2 · PRIMITIVES
//!
//! The character vocabulary. Everything the panel draws comes from here, so
//! that switching the whole stack to a different glyph set is a one-file edit.
//!
//! ## Seven-segment
//!
//! A digit is 3 cells wide by 3 cells tall, built from quadrant blocks. The
//! segment-to-quadrant mapping is:
//!
//! ```text
//!   char row 0:   a = UL+UR of all three cells   (top bar)
//!                 f = UL+LL of the left cell     (upper-left stroke)
//!                 b = UR+LR of the right cell    (upper-right stroke)
//!   char row 1:   g = LL+LR of all three cells   (middle bar, sits mid-height)
//!                 f, b continue as full strokes
//!   char row 2:   d = LL+LR of all three cells   (bottom bar)
//!                 e = UL+LL of the left cell     (lower-left stroke)
//!                 c = UR+LR of the right cell    (lower-right stroke)
//! ```
//!
//! The unions fall out as ▛ ▜ ▙ ▟ at the corners, which is why the digits
//! look mitred rather than like stacked dashes.

/// Three rows of a rendered digit. Index 0 is the top row.
pub type Seg3 = [&'static str; 3];

const D0: Seg3 = ["▛▀▜", "▌ ▐", "▙▄▟"];
const D1: Seg3 = ["  ▐", "  ▐", "  ▐"];
const D2: Seg3 = ["▀▀▜", "▄▄▟", "▙▄▄"];
const D3: Seg3 = ["▀▀▜", "▄▄▟", "▄▄▟"];
const D4: Seg3 = ["▌ ▐", "▙▄▟", "  ▐"];
const D5: Seg3 = ["▛▀▀", "▙▄▄", "▄▄▟"];
const D6: Seg3 = ["▛▀▀", "▙▄▄", "▙▄▟"];
const D7: Seg3 = ["▀▀▜", "  ▐", "  ▐"];
const D8: Seg3 = ["▛▀▜", "▙▄▟", "▙▄▟"];
const D9: Seg3 = ["▛▀▜", "▙▄▟", "▄▄▟"];
const DBLANK: Seg3 = ["   ", "   ", "   "];
const DDASH: Seg3 = ["   ", "▄▄▄", "   "];
/// Colon is one cell wide, not three — it is a separator, not a digit.
const DCOLON: Seg3 = ["▪", " ", "▪"];
/// Decimal point, likewise one cell, sitting on the baseline.
const DDOT: Seg3 = [" ", " ", "▪"];

fn glyph(c: char) -> Seg3 {
    match c {
        '0' => D0,
        '1' => D1,
        '2' => D2,
        '3' => D3,
        '4' => D4,
        '5' => D5,
        '6' => D6,
        '7' => D7,
        '8' => D8,
        '9' => D9,
        '-' => DDASH,
        ':' => DCOLON,
        '.' => DDOT,
        _ => DBLANK,
    }
}

/// Render a numeric string as three rows of seven-segment.
///
/// Digits are separated by a single blank column; colons are not, so `04:38`
/// groups the way it does on the real display. Returns three equal-length rows.
pub fn seven_seg(text: &str) -> [String; 3] {
    let mut rows = [String::new(), String::new(), String::new()];
    let chars: Vec<char> = text.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        let g = glyph(c);
        for (row, seg) in rows.iter_mut().zip(g) {
            row.push_str(seg);
        }
        // Inter-digit gap, but not around a colon and not after the last cell.
        let tight = |c: char| c == ':' || c == '.';
        let next_is_tight = chars.get(i + 1).is_some_and(|&n| tight(n));
        if i + 1 < chars.len() && !tight(c) && !next_is_tight {
            for row in rows.iter_mut() {
                row.push(' ');
            }
        }
    }
    rows
}

/// Width in cells that `seven_seg` will produce for `text`.
pub fn seven_seg_width(text: &str) -> u16 {
    seven_seg(text)[0].chars().count() as u16
}

// ---------------------------------------------------------------------------
// Bars and meters
// ---------------------------------------------------------------------------

/// Horizontal eighth-blocks, for bars that fill left-to-right. A vertical
/// eighth-block ramp reads wrong laid on its side, so bars get their own set.
pub const HRAMP: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Pick a horizontal ramp cell for `value` within the cell spanning
/// `[cell_floor, cell_floor + 1)`.
pub fn hramp_cell(value: f32, cell_floor: f32) -> char {
    let t = (value - cell_floor).clamp(0.0, 1.0);
    HRAMP[(t * 8.0).round() as usize]
}

/// The amp meter's lit dot and its unlit socket. The rev-2 note in the HTML
/// was specific that these are discrete amber dots, not a continuous ramp —
/// so the meter uses these rather than `RAMP`.
pub const DOT_ON: char = '▰';
pub const DOT_OFF: char = '▱';
/// The solid bar that rides the top of a lit column — peak indication.
pub const PEAK_BAR: char = '▬';

// ---------------------------------------------------------------------------
// Panel furniture
// ---------------------------------------------------------------------------

/// A boxed indicator legend — the small outlined APS / M SCAN / REPEAT marks.
/// Drawn with half-block brackets so the box hugs the text at one cell tall.
pub fn boxed(label: &str) -> String {
    format!("▏{label}▕")
}

/// The Fujitsu Ten roundel. The mark is 干 in a red square on the real badge.
pub const BADGE_MARK: char = '干';
pub const BADGE_TEXT: &str = "FUJITSU TEN";

/// Transport symbols. Nerd Font / Unicode media glyphs — the terminal here has
/// CaskaydiaCove NF, which covers all of these.
pub mod transport {
    pub const PLAY: &str = "▶";
    pub const PAUSE: &str = "⏸";
    pub const STOP: &str = "■";
    pub const PREV: &str = "⏮";
    pub const NEXT: &str = "⏭";
    pub const REW: &str = "◀◀";
    pub const FF: &str = "▶▶";
    pub const DISC: &str = "◍";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_three_by_three() {
        for c in "0123456789".chars() {
            let g = glyph(c);
            for row in g {
                assert_eq!(row.chars().count(), 3, "digit {c} row width");
            }
        }
    }

    #[test]
    fn seven_seg_rows_are_equal_width() {
        for s in ["04:38", "101.9", "8", "-", "12"] {
            let r = seven_seg(s);
            let w: Vec<usize> = r.iter().map(|x| x.chars().count()).collect();
            assert!(w.windows(2).all(|p| p[0] == p[1]), "{s} rows ragged: {w:?}");
        }
    }

    #[test]
    fn colon_hugs_its_neighbours() {
        // "04:38" must not put gaps either side of the colon.
        let w = seven_seg_width("04:38");
        // 4 digits * 3 + 1 colon + 2 inter-digit gaps (0|4 and 3|8) = 15
        assert_eq!(w, 15);
    }
}
