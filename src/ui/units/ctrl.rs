//! Control head — volume, attenuator, tone, fader, illumination.
//!
//! On the real stack this is the block of keys to the right of the cassette
//! deck: four separate VOLUME buttons (not a rocker — the photograph shows
//! both patterns and they are different parts), ATT, BASS, TREBLE and ILL.
//!
//! ILL is kept because it earns its place: it swaps the lamp colour of every
//! illuminated button in the rack, which is exactly what it did in the car.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::SPINE;
use crate::ui::chassis;
use crate::ui::theme::Theme;
use crate::state::{Command, SourceKind, Stack};
use crate::ui::hit::HitMap;

pub fn draw(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme, hits: &mut HitMap) {
    let inner = chassis::bay(buf, area, theme);
    let c = &stack.ctrl;

    chassis::legend(buf, inner.x, inner.y, "CONTROL", theme);
    let lamp = Rect::new(inner.x, inner.y + 1, SPINE, 4);
    chassis::lamp(buf, lamp, "ILL", theme, true);
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::Ill);

    let x = inner.x + SPINE + 2;
    let row_vol = inner.y + 1;
    let row_tone = inner.y + 3;
    let row_fader = inner.y + 5;

    // --- volume ----------------------------------------------------------
    chassis::sublegend(buf, x, row_vol, "VOLUME", theme, false);
    let bar_x = x + 8;
    let bar_w = 24u16;
    chassis::ramp_bar(buf, bar_x, row_vol, bar_w, c.volume, theme);
    buf.set_string(
        bar_x + bar_w + 1,
        row_vol,
        format!("{:>3.0}", c.volume * 100.0),
        Style::default().fg(theme.vfd).bg(theme.chassis),
    );
    // Clicking anywhere along the bar sets that level outright.
    for i in 0..bar_w {
        let level = (i + 1) as f32 / bar_w as f32;
        hits.add_row(bar_x + i, row_vol, 1, Command::Volume(level));
    }

    // The attenuator's 70% / 60% steps light per the panel's own logic.
    chassis::boxed(buf, bar_x + bar_w + 6, row_vol, "ATT", theme, c.att, false);
    hits.add_row(bar_x + bar_w + 6, row_vol, 5, Command::Att);
    chassis::boxed_green(
        buf,
        bar_x + bar_w + 12,
        row_vol,
        "70%",
        theme,
        c.att && c.volume > 0.35,
        false,
    );
    chassis::boxed_green(
        buf,
        bar_x + bar_w + 18,
        row_vol,
        "60%",
        theme,
        c.att && c.volume <= 0.35,
        false,
    );

    // --- tone ------------------------------------------------------------
    tone(buf, x, row_tone, "BASS", c.bass, theme, hits, &Command::Bass);
    tone(buf, x + 26, row_tone, "TREBLE", c.treble, theme, hits, &Command::Treble);

    // --- fader -----------------------------------------------------------
    chassis::sublegend(buf, x, row_fader, "FADER", theme, false);
    fader(buf, x + 8, row_fader, c.fader, theme, hits);

    // --- illumination ----------------------------------------------------
    let ix = x + 34;
    chassis::sublegend(buf, ix, row_fader, "ILL", theme, false);
    buf.set_string(
        ix + 4,
        row_fader,
        c.ill.label(),
        Style::default()
            .fg(theme.lamp_hot)
            .bg(theme.chassis)
            .add_modifier(Modifier::BOLD),
    );
    hits.add_row(ix, row_fader, 8, Command::Ill);

    // --- source selector -------------------------------------------------
    // The head unit's most consequential control, so it gets its own row.
    let row_src = inner.y + 6;
    chassis::sublegend(buf, x, row_src, "SOURCE", theme, false);
    let mut sx = x + 8;
    for kind in [SourceKind::Cd, SourceKind::Tape, SourceKind::Tuner] {
        let on = stack.source == kind;
        chassis::boxed(buf, sx, row_src, kind.label(), theme, on, false);
        hits.add_row(sx, row_src, kind.label().len() as u16 + 2, Command::Source(kind));
        sx += kind.label().len() as u16 + 4;
    }

    chassis::model_corner(buf, inner, &["CONTROL HEAD LT-581"], theme);
}

/// The five-tick tone display. One tick lights, -2 at the bottom of the range
/// to +2 at the top, which is the resolution the real BASS/TREBLE keys had.
#[allow(clippy::too_many_arguments)]
fn tone(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    label: &str,
    value: i8,
    theme: &Theme,
    hits: &mut HitMap,
    make: &dyn Fn(i8) -> Command,
) {
    chassis::sublegend(buf, x, y, label, theme, false);
    let tx = x + 8;
    for i in 0..5i8 {
        // Ticks run +2 (left) .. -2 (right).
        let step = 2 - i;
        let on = step == value.clamp(-2, 2);
        buf.set_string(
            tx + i as u16 * 2,
            y,
            if on { "▮" } else { "▯" },
            Style::default()
                .fg(if on { theme.led_a } else { theme.led_off })
                .bg(theme.chassis),
        );
        hits.add_row(tx + i as u16 * 2, y, 2, make(step));
    }
    buf.set_string(
        tx + 11,
        y,
        format!("{value:+}"),
        Style::default().fg(theme.vfd).bg(theme.chassis),
    );
}

/// Front/rear balance. The marker slides between the two bus labels.
fn fader(buf: &mut Buffer, x: u16, y: u16, value: f32, theme: &Theme, hits: &mut HitMap) {
    const W: u16 = 17;
    buf.set_string(x, y, "R", Style::default().fg(theme.ink_legend).bg(theme.chassis));
    buf.set_string(
        x + W + 1,
        y,
        "F",
        Style::default().fg(theme.ink_legend).bg(theme.chassis),
    );

    let pos = (value.clamp(0.0, 1.0) * (W - 1) as f32).round() as u16;
    for i in 0..W {
        let centre = i == (W - 1) / 2;
        let (ch, fg) = if i == pos {
            ("▮", theme.led_a)
        } else if centre {
            ("┼", theme.ink_grey)
        } else {
            ("─", theme.seam)
        };
        buf.set_string(x + 2 + i, y, ch, Style::default().fg(fg).bg(theme.chassis));
        hits.add_row(x + 2 + i, y, 1, Command::Fader(i as f32 / (W - 1) as f32));
    }
}
