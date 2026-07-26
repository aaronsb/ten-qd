//! QM-571 Power Amplifier — 25 W / 25 W.
//!
//! Nine meter columns, sitting directly below the nine equaliser columns and
//! bound to the same centre frequencies. Pull the 250 Hz slider down and the
//! third column here visibly drops, because both are reading the same nine
//! bands of the same signal — the meters are fed from the post-DSP output, not
//! from the source.
//!
//! That vertical alignment is a deliberate departure from the ad, where the
//! meters run horizontally. It is worth the departure: it makes the rack
//! legible as one instrument instead of two panels that happen to be stacked.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::{BAND_TRACK, BAND_W, BAND_X, SPINE};
use crate::state::Command;
use crate::ui::hit::HitMap;
use crate::state::{Stack, BAND_LABELS};
use crate::ui::chassis;
use crate::ui::theme::Theme;

const ROWS: u16 = 5;

pub fn draw(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme, hits: &mut HitMap) {
    let inner = chassis::bay(buf, area, theme);
    let amp = &stack.amp;

    let lamp = Rect::new(inner.x, inner.y + 1, SPINE, 5);
    chassis::lamp(buf, lamp, "POWER", theme, amp.power);
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::AmpPower);

    chassis::legend(buf, inner.x, inner.y, "POWER AMP", theme);

    let sx = inner.x + BAND_X;

    for i in 0..9 {
        let slot = sx + i as u16 * BAND_W;
        // Three cells wide, centred on the same track column the EQ uses, so
        // the two panels line up column for column.
        for dx in 0..3u16 {
            chassis::led_column(
                buf,
                slot + BAND_TRACK - 1 + dx,
                inner.y + 1,
                ROWS,
                amp.levels[i],
                amp.peaks[i],
                theme,
                amp.power,
            );
        }
    }

    // Band legend repeated under the meters — the columns mean nothing without
    // it, and it is the visual tie back to the equaliser.
    for (i, label) in BAND_LABELS.iter().enumerate() {
        let slot = sx + i as u16 * BAND_W;
        let lw = label.chars().count() as u16;
        chassis::sublegend(
            buf,
            slot + BAND_W.saturating_sub(lw) / 2,
            inner.y + 1 + ROWS,
            label,
            theme,
            false,
        );
    }

    // --- right-hand side: rating, badge, power ---------------------------
    let rx = inner.x + inner.width.saturating_sub(26);
    buf.set_string(
        rx,
        inner.y,
        "25W/25W",
        Style::default()
            .fg(theme.ink_white)
            .bg(theme.chassis)
            .add_modifier(Modifier::BOLD),
    );

    // The power switch lives on the spine with every other unit's, so this
    // corner keeps only what it can report: whether the amplifier is passing
    // anything. A second POWER key beside the first was two controls for one
    // relay.
    chassis::sublegend(buf, rx, inner.y + 4, "LEVEL", theme, false);
    chassis::sublegend(
        buf,
        rx + 10,
        inner.y + 5,
        if amp.power { "IND ▰" } else { "IND ▱" },
        theme,
        false,
    );

    chassis::model_corner(buf, inner, &["POWER AMPLIFIER QM-571"], theme);
}
