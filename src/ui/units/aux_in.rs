//! QA-581 Auxiliary Input — the cable, and whatever is on the end of it.
//!
//! This was a cassette adapter for a while: the tape-shaped shell with a wire,
//! pushed into the deck so the mechanism would spin and believe it was playing.
//! That is a lovely object and the wrong interface. A deck carrying an adapter
//! has a counter that counts nothing, two sides that do not exist and a pair of
//! reels turning against a loop of tape that is not the music — every readout
//! on the unit made false at once, to model a cable.
//!
//! So the cable became a source, which is what it always was. AUX sits beside
//! CD, TAPE and TUNER on the selector, and the cassette deck went back to being
//! a cassette deck.
//!
//! What it shows is what a real auxiliary input could not: an **input meter**,
//! because a cable that carries the wrong thing should say so on the panel
//! rather than needing to be measured with other tools. See `docs/sources.md`
//! for the two days that lesson cost.
//!
//! The transport keys are the other thing a real one could not do. They reach
//! the plugged-in player over MPRIS — the modern form of leaning over to the
//! passenger seat and pressing the button yourself.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::SPINE;
use crate::state::{Command, SourceKind, Stack, Unit};
use crate::ui::chassis;
use crate::ui::glyph::transport as tr;
use crate::ui::hit::HitMap;
use crate::ui::theme::Theme;

/// The bottom of the input meter's scale. A cable fed from a desktop mixer
/// sits near the top of it; a capture pointed at the wrong node picks up the
/// room and sits near the bottom, and the difference is meant to be obvious at
/// a glance.
const FLOOR_DB: f32 = -48.0;
const SEGMENTS: u16 = 12;

/// Peak level (0.0–1.0) as a position on the meter's scale (0.0–1.0).
fn scale(level: f32) -> f32 {
    if !level.is_finite() || level <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * level.min(1.0).log10();
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

/// What the meter prints beside the ladder. Silence gets dashes rather than a
/// very large negative number, because "nothing" and "very quiet" are
/// different answers and the panel should not blur them.
fn readout(level: f32) -> String {
    if !level.is_finite() || level <= 0.0 {
        return " --dB".into();
    }
    let db = (20.0 * level.min(1.0).log10()).max(-99.0);
    format!("{db:>3.0}dB")
}

pub fn draw(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme, hits: &mut HitMap) {
    let inner = chassis::bay(buf, area, theme);
    let a = &stack.aux;
    let live = stack.source == SourceKind::Aux;

    chassis::legend(buf, inner.x, inner.y, "AUX", theme);
    let lamp = Rect::new(inner.x, inner.y + 1, SPINE, 5);
    chassis::lamp(buf, lamp, "POWER", theme, a.power);
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::UnitPower(Unit::Aux));



    // --- window -----------------------------------------------------------
    let win = Rect::new(
        inner.x + SPINE + 1,
        inner.y,
        inner.width.saturating_sub(SPINE + 1),
        6,
    );
    let w = chassis::window(buf, win, theme, live && a.state.live);
    if w.width < 40 {
        return;
    }

    buf.set_string(
        w.x,
        w.y,
        "AUXILIARY INPUT",
        Style::default().fg(theme.ink_white).bg(theme.window),
    );

    // What is on the other end. An unplugged cable says so plainly rather than
    // leaving the field blank, because blank reads as broken.
    let what = a
        .state
        .source
        .clone()
        .unwrap_or_else(|| format!("send audio to \"{}\"", crate::adapter::DESCRIPTION));
    let what: String = what.chars().take(w.width.saturating_sub(2) as usize).collect();
    buf.set_string(
        w.x,
        w.y + 1,
        &what,
        Style::default()
            .fg(if a.state.live { theme.vfd } else { theme.ink_grey })
            .bg(theme.window),
    );

    // --- input meter ------------------------------------------------------
    // Read off the cable, before the equaliser and whichever source is
    // selected, so a cable carrying the wrong thing says so here instead of
    // needing to be chased down with external tools.
    chassis::sublegend(buf, w.x, w.y + 3, "INPUT", theme, true);
    let pos = scale(a.input);
    for i in 0..SEGMENTS {
        // Strictly greater, so a dead cable lights nothing at all rather than
        // resting on its first segment.
        let lit = pos * SEGMENTS as f32 > i as f32;
        buf.set_string(
            w.x + 6 + i,
            w.y + 3,
            if lit { "▰" } else { "▱" },
            Style::default()
                .fg(if lit { theme.led_ramp(i as f32 / SEGMENTS as f32) } else { theme.led_off })
                .bg(theme.window),
        );
    }
    chassis::sublegend(buf, w.x + 7 + SEGMENTS, w.y + 3, &readout(a.input), theme, true);

    // --- what the player says ---------------------------------------------
    let right = w.x + w.width.saturating_sub(22);
    if right > w.x + 26 {
        chassis::sublegend(buf, right, w.y, "SOURCE", theme, true);
        let player = a.state.player.clone().unwrap_or_else(|| "—".into());
        let player: String = player.chars().take(20).collect();
        buf.set_string(
            right,
            w.y + 1,
            &player,
            Style::default()
                .fg(if a.state.player.is_some() { theme.vfd } else { theme.ink_grey })
                .bg(theme.window)
                .add_modifier(Modifier::BOLD),
        );
        chassis::boxed_green(buf, right, w.y + 3, "MPRIS", theme, a.state.player.is_some(), true);
    }

    // --- keys -------------------------------------------------------------
    let ky = inner.y + 6;
    let kx = inner.x + SPINE + 1;
    let playing = a.state.playing();

    let mut row = chassis::KeyRow::new(kx, ky);
    let mut press = |row: &mut chassis::KeyRow, w, label: &str, active, cmd, hits: &mut HitMap| {
        let r = row.key(buf, w, label, theme, active, true);
        hits.add(r.x, r.y, r.width, r.height, cmd);
    };
    press(&mut row, 6, tr::REW, false, Command::AuxPrev, hits);
    press(
        &mut row,
        6,
        if playing { tr::PAUSE } else { tr::PLAY },
        playing,
        Command::AuxPlayPause,
        hits,
    );
    press(&mut row, 6, tr::FF, false, Command::AuxNext, hits);
    press(&mut row, 6, tr::STOP, false, Command::AuxStop, hits);
    row.gap(2);
    press(&mut row, 8, "SELECT", a.state.source.is_some(), Command::AuxOpen, hits);

    // --- shelf strip ------------------------------------------------------
    // "via aux" is a claim about routing, so it is only made when something is
    // actually plugged in. MPRIS will happily report a player that is playing
    // straight to the speakers, and saying that came through the rack would be
    // the panel describing a signal path that does not exist.
    let strip = match (&a.state.source, a.state.title.is_empty()) {
        (Some(src), true) => format!("{src} · via aux"),
        (Some(_), false) if a.state.artist.is_empty() => {
            format!("{} · via aux", a.state.title)
        }
        (Some(_), false) => format!("{} — {} · via aux", a.state.artist, a.state.title),
        (None, _) => {
            format!("nothing plugged in — send audio to \"{}\"", crate::adapter::DESCRIPTION)
        }
    };
    let max = inner.width.saturating_sub(SPINE + 2) as usize;
    let strip: String = strip.chars().take(max).collect();
    buf.set_string(
        inner.x + SPINE + 1,
        ky + 3,
        &strip,
        Style::default()
            .fg(theme.ink_grey)
            .bg(theme.chassis)
            .add_modifier(Modifier::ITALIC),
    );

    chassis::model_corner(buf, inner, &["AUXILIARY INPUT QA-581"], theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dead_cable_lights_nothing() {
        assert_eq!(scale(0.0), 0.0);
        // Nothing at all reads differently from very quiet: dashes, not a
        // number, because "no signal" is not a level.
        assert_eq!(readout(0.0), " --dB");
    }

    #[test]
    fn full_scale_fills_the_meter() {
        assert_eq!(scale(1.0), 1.0);
        assert_eq!(readout(1.0), "  0dB");
    }

    #[test]
    fn a_capture_picking_up_the_room_sits_near_the_bottom() {
        // The bug this meter exists to make visible: -45 dBFS of room noise.
        let room = 10f32.powf(-45.0 / 20.0);
        assert!(scale(room) < 0.1, "room noise must read as nearly nothing");
        assert_eq!(readout(room), "-45dB");
    }

    #[test]
    fn a_healthy_cable_sits_near_the_top() {
        let hot = 10f32.powf(-6.0 / 20.0);
        assert!(scale(hot) > 0.85, "a -6 dBFS cable should be plainly lit");
    }

    #[test]
    fn the_scale_stays_in_bounds_and_never_panics_on_junk() {
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 2.0, 1e-30] {
            let s = scale(v);
            assert!((0.0..=1.0).contains(&s), "{v} produced {s}");
            readout(v);
        }
    }
}
