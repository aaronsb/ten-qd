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
    // A radio that is switched off shows nothing, whatever the hardware says.
    let ok = t.power && matches!(t.device, Some(ref d) if !d.is_empty());

    chassis::legend(buf, inner.x, inner.y, "TUNER", theme);
    let lamp = Rect::new(inner.x, inner.y + 1, SPINE, 5);
    chassis::lamp(buf, lamp, "POWER", theme, t.power);
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::TunerPower);

    // --- band ------------------------------------------------------------
    // On the right, where the advertisement puts the second key. The display
    // window makes room for it rather than running under it.
    const POWER_W: u16 = 10;
    let px = inner.x + inner.width.saturating_sub(POWER_W);
    let plamp = Rect::new(px, inner.y + 1, POWER_W, 5);
    // Always lit: the band selector is a control that is simply there. (AM is
    // not implemented — an RTL-SDR will not receive it — and the panel says so
    // in the shelf strip rather than by darkening a key that still works.)
    chassis::lamp(buf, plamp, "AM/FM", theme, true);
    hits.add(plamp.x, plamp.y, plamp.width, plamp.height, Command::TunerBand);

    // --- window -----------------------------------------------------------
    let win = Rect::new(
        inner.x + SPINE + 1,
        inner.y,
        inner.width.saturating_sub(SPINE + 2 + POWER_W),
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
    // Five cells either way. The blank keeps its decimal point, because a
    // space ghosts as a full `8` (three cells) while `.` ghosts as itself
    // (one) — an all-blank string is wider than a real reading and runs into
    // the indicators beside it.
    let text = if ok {
        format!("{:>5.1}", t.freq)
    } else {
        "   . ".to_string()
    };
    let fw = glyph::seven_seg_width("888.8");
    let fx = w.x + 8;
    chassis::vfd(buf, fx, w.y, &text, theme);

    // --- indicators -------------------------------------------------------
    let ix = fx + fw + 4;
    chassis::boxed(buf, ix, w.y, "STEREO", theme, ok && t.stereo, true);
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
        let lit = ok && t.rssi * bars as f32 > i as f32;
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
    let note = match (&t.device, t.power) {
        (_, false) => "off".to_string(),
        (Some(d), _) if !d.is_empty() => d.clone(),
        _ => "no radio".to_string(),
    };
    let note: String = note.chars().take(w.width.saturating_sub(2) as usize).collect();
    chassis::sublegend(buf, w.x + 8, w.y + 3, &note, theme, true);

    // --- keys -------------------------------------------------------------
    let ky = inner.y + 6;
    let kx = inner.x + SPINE + 1;

    let mut row = chassis::KeyRow::new(kx, ky);

    // TUNE is one rocker: a chevron at each end of a single bar, so the cap is
    // split down the middle for hit testing rather than being two keys.
    let r = row.key(buf, 9, "⌄ TUNE ⌃", theme, false, true);
    hits.add(r.x, r.y, r.width / 2, r.height, Command::TunerStepDown);
    hits.add(r.x + r.width / 2, r.y, r.width - r.width / 2, r.height, Command::TunerStepUp);
    row.gap(1);

    let r = row.key(buf, 9, "◀ SEEK ▶", theme, t.seeking, true);
    hits.add(r.x, r.y, r.width / 2, r.height, Command::TunerSeekDown);
    hits.add(r.x + r.width / 2, r.y, r.width - r.width / 2, r.height, Command::TunerSeekUp);
    row.gap(2);

    // --- presets ----------------------------------------------------------
    // Same cap as every other button; six numbered caps under a radio need no
    // caption. An unstored preset keeps its cap — the button is physically
    // there — but its slot and number stay dark.
    for i in 0..6usize {
        let stored = t.presets[i].is_some();
        let r = row.key(buf, 4, &format!("{}", i + 1), theme, t.preset == Some(i), stored);
        hits.add(r.x, r.y, r.width, r.height, Command::TunerPreset(i));
    }


    // The shelf strip: what is tuned, in words rather than numerals.
    let strip = if !t.power {
        "tuner off".to_string()
    } else if !ok {
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
