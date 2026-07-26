//! QE-581 Graphic Equalizer — two banks of nine.
//!
//! The rev-2 note on the HTML prototype called this the biggest correction to
//! the original design: the photograph shows an upper row of nine caps and a
//! lower row of nine caps with the frequency legend printed between them, F
//! beside the upper and R beside the lower. Front and rear are curved
//! independently, and those two curves are two real filter banks in `dsp.rs`.
//!
//! Each slider is five rows tall and resolves nine positions by using the
//! upper and lower half of each cell — ±12 dB in 3 dB steps.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::{BAND_TRACK, BAND_W, BAND_X, MARKER_X, SPINE};
use crate::state::{Bank, Command, Stack, BAND_LABELS};
use crate::ui::hit::HitMap;
use crate::ui::chassis;
use crate::ui::theme::Theme;

/// Slider travel, in dB either side of flat.
pub const RANGE_DB: f32 = 12.0;
/// One detent.
pub const STEP_DB: f32 = 3.0;
const ROWS: u16 = 5;
/// Nine detents across five rows, using half-cells.
const POSITIONS: i32 = 9;

/// Convert a gain in dB to a detent index, 0 = top (+12) .. 8 = bottom (-12).
fn detent(db: f32) -> i32 {
    let clamped = db.clamp(-RANGE_DB, RANGE_DB);
    ((RANGE_DB - clamped) / STEP_DB).round() as i32
}

/// Snap a gain to the nearest detent — the sliders have physical stops.
pub fn snap(db: f32) -> f32 {
    (db / STEP_DB).round() * STEP_DB
}

pub fn draw(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme, hits: &mut HitMap) {
    let inner = chassis::bay(buf, area, theme);
    let eq = &stack.eq;

    let lamp = Rect::new(inner.x, inner.y + 1, SPINE, 5);
    chassis::lamp(buf, lamp, "DEFEAT", theme, eq.defeat);
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::EqDefeat);

    chassis::legend(buf, inner.x, inner.y, "GRAPHIC EQ", theme);

    let sx = inner.x + BAND_X;
    let front_y = inner.y + 1;
    let legend_y = front_y + ROWS;
    let rear_y = legend_y + 1;

    bank(buf, sx, front_y, eq.bank(Bank::Front), eq, Bank::Front, theme, hits);
    bank(buf, sx, rear_y, eq.bank(Bank::Rear), eq, Bank::Rear, theme, hits);

    // Bank markers. A single glyph rather than a boxed one: the box needed
    // three columns, and spending them here is what pushed the whole panel up
    // against the DEFEAT lamp with no gap.
    marker(buf, inner.x + MARKER_X, front_y, "F", Bank::Front, eq, theme, hits);
    marker(buf, inner.x + MARKER_X, rear_y, "R", Bank::Rear, eq, theme, hits);

    // --- frequency legend, printed between the banks ---------------------
    for (i, label) in BAND_LABELS.iter().enumerate() {
        let slot = sx + i as u16 * BAND_W;
        let lw = label.chars().count() as u16;
        let lx = slot + BAND_W.saturating_sub(lw) / 2;
        let selected = eq.cursor.1 == i;
        let style = if selected {
            Style::default().fg(theme.vfd).bg(theme.chassis).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.ink_legend).bg(theme.chassis)
        };
        buf.set_string(lx, legend_y, label, style);
        // Clicking a frequency legend selects that band in the current bank.
        hits.add_row(slot, legend_y, BAND_W, Command::EqSelect { bank: eq.cursor.0, band: i });
    }

    // --- dB scale down the right edge ------------------------------------
    let scale_x = sx + 9 * BAND_W + 1;
    if scale_x + 4 < inner.x + inner.width {
        chassis::sublegend(buf, scale_x, front_y, "+12", theme, false);
        chassis::sublegend(buf, scale_x, front_y + 2, "  0", theme, false);
        chassis::sublegend(buf, scale_x, front_y + 4, "-12", theme, false);
        chassis::sublegend(buf, scale_x, rear_y, "+12", theme, false);
        chassis::sublegend(buf, scale_x, rear_y + 2, "  0", theme, false);
        chassis::sublegend(buf, scale_x, rear_y + 4, "-12", theme, false);
    }

    // --- readout for the selected band -----------------------------------
    let (bank_sel, band_sel) = eq.cursor;
    let db = eq.bank(bank_sel)[band_sel];
    let readout = format!(
        "{} {:>4} Hz {:+.0} dB",
        if bank_sel == Bank::Front { "FRONT" } else { "REAR " },
        BAND_LABELS[band_sel],
        db
    );
    let y = inner.y + inner.height.saturating_sub(1);
    buf.set_string(
        inner.x + BAND_X,
        y,
        &readout,
        Style::default().fg(theme.vfd).bg(theme.chassis),
    );

    if eq.defeat {
        buf.set_string(
            inner.x + BAND_X + readout.len() as u16 + 3,
            y,
            "· DEFEAT — CURVE BYPASSED",
            Style::default().fg(theme.ink_red).bg(theme.chassis),
        );
    }

    chassis::model_corner(buf, inner, &["GRAPHIC EQUALIZER QE-581"], theme);
}

/// The F / R bank marker. Clicking it switches the bank the cursor is on.
#[allow(clippy::too_many_arguments)]
fn marker(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    label: &str,
    which: Bank,
    eq: &crate::state::EqState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let active = eq.cursor.0 == which;
    let style = Style::default()
        .fg(if active { theme.vfd } else { theme.ink_red })
        .bg(theme.chassis)
        .add_modifier(Modifier::BOLD);
    buf.set_string(x, y + 2, label, style);
    hits.add(x, y, 1, ROWS, Command::EqSelect { bank: which, band: eq.cursor.1 });
}

#[allow(clippy::too_many_arguments)]
fn bank(
    buf: &mut Buffer,
    sx: u16,
    y: u16,
    gains: &[f32; 9],
    eq: &crate::state::EqState,
    which: Bank,
    theme: &Theme,
    hits: &mut HitMap,
) {
    for (i, &db) in gains.iter().enumerate() {
        let slot = sx + i as u16 * BAND_W;
        let track_x = slot + BAND_TRACK;
        let selected = eq.cursor == (which, i);

        // Each row of a slider is its own target: clicking partway up the
        // travel sets the gain that point represents, which is how you would
        // actually move a physical cap.
        for r in 0..ROWS {
            let top_db = RANGE_DB - (r * 2) as f32 * STEP_DB;
            hits.add_row(
                slot,
                y + r,
                BAND_W,
                Command::EqBand { bank: which, band: i, db: top_db },
            );
        }

        // Slider track: a dim groove with a detent mark at flat.
        for r in 0..ROWS {
            let is_centre = r == ROWS / 2;
            buf.set_string(
                track_x,
                y + r,
                if is_centre { "┼" } else { "│" },
                Style::default()
                    .fg(if is_centre { theme.ink_grey } else { theme.seam })
                    .bg(theme.chassis),
            );
        }

        // Cap. The detent index maps to a row plus which half of it, which is
        // how five rows carry nine positions.
        let d = detent(db).clamp(0, POSITIONS - 1);
        let row = (d / 2) as u16;
        let upper = d % 2 == 0;
        let cap_char = if upper { "▀" } else { "▄" };

        let (fg, bold) = if eq.defeat {
            (theme.ink_grey, false)
        } else if selected {
            (theme.vfd, true)
        } else if db > 0.0 {
            (theme.led_a, false)
        } else if db < 0.0 {
            (theme.led_g, false)
        } else {
            (theme.ink_white, false)
        };

        let mut style = Style::default().fg(fg).bg(theme.chassis);
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        buf.set_string(
            track_x.saturating_sub(1),
            y + row,
            cap_char.repeat(3),
            style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detents_span_the_range_top_to_bottom() {
        assert_eq!(detent(12.0), 0);
        assert_eq!(detent(0.0), 4);
        assert_eq!(detent(-12.0), 8);
    }

    #[test]
    fn detents_stay_in_bounds_when_over_driven() {
        for db in [-99.0, -13.0, 13.0, 99.0] {
            let d = detent(db);
            assert!((0..POSITIONS).contains(&d), "{db} dB gave detent {d}");
        }
    }

    #[test]
    fn snap_lands_on_step_boundaries() {
        assert_eq!(snap(1.4), 0.0);
        assert_eq!(snap(1.6), 3.0);
        assert_eq!(snap(-4.0), -3.0);
    }
}
