//! LT-581 AM/FM Tuner — driven by an RTL-SDR.
//!
//! Two indicators on this panel are the reason the demodulator is ours rather
//! than a pipe from `rtl_fm`: the signal meter reads the mean IQ magnitude off
//! the dongle, and STEREO lights only when a 19 kHz pilot is actually present
//! in the multiplex. Neither can be faked from a mono audio stream.
//!
//! TUNE is a single rocker with a chevron at each end, not three buttons — the
//! rev-2 note on the HTML prototype was specific about that, and the
//! photograph shows both patterns on the same rack.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::SPINE;
use crate::state::{Command, SourceKind, Stack};
use crate::ui::chassis;
use crate::ui::glyph;
use crate::ui::hit::HitMap;
use crate::ui::theme::Theme;

pub fn draw(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme, hits: &mut HitMap) {
    let inner = chassis::bay(buf, area, theme);
    let t = &stack.tuner;
    let live = stack.source == SourceKind::Tuner;
    let ok = matches!(t.device, Some(ref d) if !d.is_empty());

    chassis::legend(buf, inner.x, inner.y, "TUNER", theme);
    let lamp = Rect::new(inner.x, inner.y + 1, SPINE, 4);
    chassis::lamp(buf, lamp, "AM/FM", theme, live);
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::TunerBand);

    // --- window -----------------------------------------------------------
    let win = Rect::new(
        inner.x + SPINE + 1,
        inner.y,
        inner.width.saturating_sub(SPINE + 1),
        6,
    );
    let w = chassis::window(buf, win, theme, live && ok);
    if w.width < 40 {
        return;
    }

    chassis::boxed_green(buf, w.x, w.y, "FM", theme, true, true);
    chassis::boxed(buf, w.x, w.y + 1, "AM", theme, false, true);
    chassis::sublegend(buf, w.x, w.y + 3, "MHz", theme, true);

    // --- frequency --------------------------------------------------------
    // Always five cells: the hundreds digit is blank below 100 MHz, exactly as
    // it is on a real dial, and the field does not shift when it lights.
    let text = if ok {
        format!("{:>5.1}", t.freq)
    } else {
        "  . ".to_string()
    };
    let fw = glyph::seven_seg_width("888.8");
    let fx = w.x + 8;
    chassis::vfd(buf, fx, w.y, &text, theme);

    // --- indicators -------------------------------------------------------
    let ix = fx + fw + 4;
    chassis::boxed(buf, ix, w.y, "STEREO", theme, t.stereo, true);
    chassis::boxed_green(buf, ix, w.y + 1, "LOCAL", theme, t.local, true);
    hits.add_row(ix, w.y + 1, 8, Command::TunerLocal);
    if t.seeking {
        chassis::boxed(buf, ix, w.y + 2, "SEEK", theme, true, true);
    }

    // --- signal meter -----------------------------------------------------
    let mx = ix + 10;
    chassis::sublegend(buf, mx, w.y + 3, "SIGNAL", theme, true);
    let bars = 10u16;
    for i in 0..bars {
        let lit = t.rssi * bars as f32 > i as f32;
        // The meter is on glass, not on the panel, so it sits on the window
        // colour rather than the chassis.
        buf.set_string(
            mx + i,
            w.y + 1,
            if lit { "▰" } else { "▱" },
            Style::default()
                .fg(if lit { theme.led_ramp(i as f32 / bars as f32) } else { theme.led_off })
                .bg(theme.window),
        );
    }

    // What the radio actually is, or why there is not one.
    let note = match &t.device {
        Some(d) if !d.is_empty() => d.clone(),
        Some(_) | None => "no radio".to_string(),
    };
    let note: String = note.chars().take(w.width.saturating_sub(2) as usize).collect();
    chassis::sublegend(buf, w.x + 8, w.y + 3, &note, theme, true);

    // --- keys -------------------------------------------------------------
    let ky = inner.y + 6;
    let mut kx = inner.x + SPINE + 1;

    // TUNE is one rocker: a chevron at each end of a single bar.
    let rw = chassis::key(buf, kx, ky, 9, "⌄ TUNE ⌃", theme, false);
    hits.add(kx, ky, rw / 2, 3, Command::TunerStepDown);
    hits.add(kx + rw / 2, ky, rw - rw / 2, 3, Command::TunerStepUp);
    kx += rw + 2;

    let sw = chassis::key(buf, kx, ky, 9, "◀ SEEK ▶", theme, t.seeking);
    hits.add(kx, ky, sw / 2, 3, Command::TunerSeekDown);
    hits.add(kx + sw / 2, ky, sw - sw / 2, 3, Command::TunerSeekUp);
    kx += sw + 3;

    // --- presets ----------------------------------------------------------
    chassis::sublegend(buf, kx, ky, "PRESET", theme, false);
    for i in 0..6u16 {
        let px = kx + i * 4;
        let stored = t.presets[i as usize].is_some();
        let active = t.preset == Some(i as usize);
        let style = if active {
            Style::default().fg(theme.vfd).bg(theme.chassis).add_modifier(Modifier::BOLD)
        } else if stored {
            Style::default().fg(theme.ink_legend).bg(theme.chassis)
        } else {
            Style::default().fg(theme.ink_grey).bg(theme.chassis).add_modifier(Modifier::DIM)
        };
        buf.set_string(px + 1, ky + 1, format!("{}", i + 1), style);
        buf.set_string(
            px,
            ky + 2,
            if stored { "▬▬▬" } else { "───" },
            Style::default()
                .fg(if stored { theme.cap_slot } else { theme.seam })
                .bg(theme.chassis),
        );
        hits.add(px, ky + 1, 3, 2, Command::TunerPreset(i as usize));
    }

    let badge_x = inner.x + inner.width.saturating_sub(24);
    chassis::badge(buf, badge_x, ky, theme);

    // The shelf strip: what is tuned, in words rather than numerals.
    let strip = if !ok {
        "no radio — see README for RTL-SDR setup".to_string()
    } else if let Some(p) = t.preset {
        format!("FM {:.1} MHz · preset {} · {}", t.freq, p + 1, if t.stereo { "stereo" } else { "mono" })
    } else {
        format!("FM {:.1} MHz · {}", t.freq, if t.stereo { "stereo" } else { "mono" })
    };
    buf.set_string(
        inner.x + SPINE + 1,
        ky + 3,
        &strip,
        Style::default()
            .fg(theme.ink_grey)
            .bg(theme.chassis)
            .add_modifier(Modifier::ITALIC),
    );

    chassis::model_corner(buf, inner, &["AM/FM STEREO TUNER LT-581"], theme);
}
