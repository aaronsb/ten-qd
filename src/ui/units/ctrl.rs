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
    let lamp = Rect::new(inner.x, inner.y + 1, SPINE, 5);
    chassis::lamp(buf, lamp, "ILL", theme, true);
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::Ill);

    // The advertisement's top module puts VOLUME on the right as one wide
    // rocker with a chevron at each end — not a pair of buttons. Split down
    // the middle for hit testing, the same way the tuner's TUNE bar is.
    const VOL_W: u16 = 12;
    let vx = inner.x + inner.width.saturating_sub(VOL_W);
    let vol = Rect::new(vx, inner.y + 1, VOL_W, 5);
    chassis::lamp(buf, vol, "⌄ VOL ⌃", theme, true);
    hits.add(vol.x, vol.y, vol.width / 2, vol.height, Command::VolDown);
    hits.add(vol.x + vol.width / 2, vol.y, vol.width - vol.width / 2, vol.height, Command::VolUp);

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

    // The attenuator's 70% / 60% steps are readouts, not controls, so they
    // stay boxed legends — the ATT key that drives them is down in the key row.
    chassis::boxed_green(
        buf,
        bar_x + bar_w + 6,
        row_vol,
        "70%",
        theme,
        c.att && c.volume > 0.35,
        false,
    );
    chassis::boxed_green(
        buf,
        bar_x + bar_w + 12,
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

    // --- key row ----------------------------------------------------------
    // ATT and the source selector, wearing the same caps as every other
    // operable control on the rack. The attenuator's 70% / 60% marks stay up
    // on the volume row as boxed legends, because those report rather than do.
    let ky = inner.y + 7;
    chassis::sublegend(buf, x, ky, "SOURCE", theme, false);
    let mut row = chassis::KeyRow::new(x + 8, ky);

    let r = row.key(buf, 5, "ATT", theme, c.att, true);
    hits.add(r.x, r.y, r.width, r.height, Command::Att);
    row.gap(2);

    for kind in [SourceKind::Cd, SourceKind::Tape, SourceKind::Tuner] {
        let label = kind.label();
        let r = row.key(buf, label.len() as u16 + 2, label, theme, stack.source == kind, true);
        hits.add(r.x, r.y, r.width, r.height, Command::Source(kind));
    }

    // Which speakers the rack drives. A control-head concern: it is the same
    // question as the fader, one level further out.
    let oy = inner.y + 6;
    chassis::sublegend(buf, x, oy, "OUTPUT", theme, false);
    let name = stack.output.clone().unwrap_or_else(|| "system default".into());
    let name: String = name.chars().take(34).collect();
    buf.set_string(
        x + 8,
        oy,
        &name,
        Style::default().fg(theme.ink_white).bg(theme.chassis),
    );
    hits.add_row(x, oy, 44, Command::NextOutput);

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
    let ink = Style::default().fg(theme.ink_legend).bg(theme.chassis);
    // The track runs from x+2 to x+2+W-1, so F sits one clear of its end.
    // Placing it at x+W+1 put it under the track's last cell.
    buf.set_string(x, y, "R", ink);
    buf.set_string(x + W + 3, y, "F", ink);

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
