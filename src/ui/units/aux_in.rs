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
pub fn scale(level: f32) -> f32 {
    if !level.is_finite() || level <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * level.min(1.0).log10();
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

/// What the meter prints beside the ladder. Silence gets dashes rather than a
/// very large negative number, because "nothing" and "very quiet" are
/// different answers and the panel should not blur them.
pub fn readout(level: f32) -> String {
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

        // LINK: is the stream we plugged in still on our sink?
        //
        // Green because a seated plug is a mode, red because a plug that has
        // come out is an alert — the panel's existing grammar, and the reason
        // there are two `boxed` helpers. It blinks rather than sitting lit,
        // because the signal is not going where the panel says it is, and that
        // is not a thing to notice only if you happen to be looking.
        let lx = right + 8;
        if lx + 6 <= w.x + w.width {
            if a.state.link.astray() {
                chassis::boxed(buf, lx, w.y + 3, "LINK", theme, chassis::blinking(stack.frame), true);
            } else {
                let seated = matches!(a.state.link, crate::adapter::Link::Seated);
                chassis::boxed_green(buf, lx, w.y + 3, "LINK", theme, seated, true);
            }
        }
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
    //
    // A plug that has come out outranks all of it: everything below this line
    // describes what is coming through the rack, and when the answer is
    // "nothing" that is the only sentence worth the row.
    //
    // The two ways of saying it are not interchangeable. A stream sitting on
    // some other sink is *located*, because most of the time nothing did it on
    // purpose — a virtual sink's owner quits and WirePlumber tips the orphans
    // onto the default, and the device they land on did nothing. Only a stream
    // that would not stay put after being pushed back in gets somebody named,
    // and by then the naming is evidence rather than a guess.
    let strip = match (&a.state.link, &a.state.source) {
        (crate::adapter::Link::Adrift(on), Some(src)) => {
            format!("{src} — went to \"{on}\", not through the rack")
        }
        (crate::adapter::Link::Contested(who), Some(src)) => {
            format!("{src} — held on \"{who}\"; the rack cannot get it back")
        }
        _ => shelf(a),
    };
    let max = inner.width.saturating_sub(SPINE + 2) as usize;
    let strip: String = strip.chars().take(max).collect();
    buf.set_string(
        inner.x + SPINE + 1,
        ky + 3,
        &strip,
        Style::default()
            .fg(if a.state.link.astray() { theme.ink_red } else { theme.ink_grey })
            .bg(theme.chassis)
            .add_modifier(Modifier::ITALIC),
    );

    chassis::model_corner(buf, inner, &["AUXILIARY INPUT QA-581"], theme);
}

/// What the shelf strip says while the cable is behaving.
fn shelf(a: &crate::state::Aux) -> String {
    match (&a.state.source, a.state.title.is_empty()) {
        (Some(src), true) => format!("{src} · via aux"),
        (Some(_), false) if a.state.artist.is_empty() => {
            format!("{} · via aux", a.state.title)
        }
        (Some(_), false) => format!("{} — {} · via aux", a.state.artist, a.state.title),
        (None, _) => {
            format!("nothing plugged in — send audio to \"{}\"", crate::adapter::DESCRIPTION)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Link;

    /// The bay as text, so what the panel claims can be asserted on.
    fn render(link: Link, source: Option<&str>, frame: u64) -> String {
        let mut stack = Stack::default();
        stack.aux.state.source = source.map(str::to_string);
        stack.aux.state.link = link;
        stack.frame = frame;
        let area = Rect::new(0, 0, 120, 12);
        let mut buf = Buffer::empty(area);
        let theme = Theme::for_stack(&stack);
        draw(&mut buf, area, &stack, &theme, &mut HitMap::new());
        (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The failure this unit was extended for. A stream that is not on our
    /// sink must not go on being described as a signal path.
    #[test]
    fn a_stream_that_is_not_on_our_sink_stops_claiming_a_path() {
        let panel = render(Link::Adrift("EasyEffects Sink".into()), Some("Google Chrome"), 0);
        assert!(panel.contains("not through the rack"), "{panel}");
        assert!(!panel.contains("via aux"), "the rack must not claim a path it has not got:\n{panel}");
    }

    /// A sink a stream merely landed on is a bystander. When EasyEffects quits,
    /// its sink goes with it and WirePlumber tips the orphans onto the default
    /// output — the headphones they land on did nothing, and saying they "took"
    /// the stream sends the operator to argue with the wrong program.
    #[test]
    fn a_bystander_sink_is_located_not_blamed() {
        let panel = render(Link::Adrift("Muh Chickin Waffles".into()), Some("Google Chrome"), 0);
        assert!(panel.contains("went to \"Muh Chickin Waffles\""), "{panel}");
        for accusation in ["taken", "took", "holding", "held"] {
            assert!(!panel.contains(accusation), "accused a bystander of {accusation}:\n{panel}");
        }
    }

    /// Once the rack has pushed the plug back in and been overruled, naming the
    /// culprit is evidence rather than a guess — and the panel says plainly
    /// that it has stopped trying, so the operator knows to go and fix it.
    #[test]
    fn a_stream_something_is_holding_names_it() {
        let panel = render(Link::Contested("EasyEffects Sink".into()), Some("Google Chrome"), 0);
        assert!(panel.contains("held on \"EasyEffects Sink\""), "{panel}");
        assert!(panel.contains("cannot get it back"), "{panel}");
    }

    /// The complement, and the one that would catch a warning stuck on: a plug
    /// that held says so, in the ordinary language of the shelf.
    #[test]
    fn a_plug_that_held_still_reads_as_a_signal_path() {
        let panel = render(Link::Seated, Some("Google Chrome"), 0);
        assert!(panel.contains("via aux"), "{panel}");
        assert!(!panel.contains("not through the rack"), "{panel}");
    }

    /// Whether the LINK segment is lit on this frame.
    ///
    /// Read off the style, not the text: an unlit segment is still drawn, the
    /// same way an unlit segment of a real VFD is still a segment you can see.
    /// Asking whether the panel *contains* "LINK" answers a question about the
    /// glyph, not about the lamp.
    fn link_lit(link: Link, frame: u64) -> bool {
        let mut stack = Stack::default();
        stack.aux.state.source = Some("Google Chrome".into());
        stack.aux.state.link = link;
        stack.frame = frame;
        let area = Rect::new(0, 0, 120, 12);
        let mut buf = Buffer::empty(area);
        let theme = Theme::for_stack(&stack);
        draw(&mut buf, area, &stack, &theme, &mut HitMap::new());
        for y in 0..area.height {
            for x in 0..area.width.saturating_sub(4) {
                if ["L", "I", "N", "K"].iter().enumerate().all(|(i, c)| {
                    buf[(x + i as u16, y)].symbol() == *c
                }) {
                    return buf[(x, y)].style().add_modifier.contains(Modifier::BOLD);
                }
            }
        }
        panic!("the bay drew no LINK segment at all");
    }

    /// A blinking warning is only a warning if it comes back. Half a second
    /// lit, half a second dark, and lit again — asserted across the wrap, so a
    /// phase that flashes once and sticks cannot pass.
    #[test]
    fn the_warning_blinks_rather_than_flashing_once() {
        let pulled = Link::Contested("EasyEffects Sink".into());
        assert!(link_lit(pulled.clone(), 0), "must be lit on the frame the fault appears");
        assert!(!link_lit(pulled.clone(), 20), "must go dark between flashes");
        assert!(link_lit(pulled, 35), "and must come back — a single flash can be missed");
    }

    /// A seated plug is a steady lamp, not a blinking one: it must read the
    /// same on every frame, or it would look like a fault.
    #[test]
    fn a_seated_plug_does_not_blink() {
        for f in [0, 20, 35, 100] {
            assert!(link_lit(Link::Seated, f), "seated must stay lit, frame {f}");
        }
    }

    /// `Gone` is a stream that ended, which is what stopping the music looks
    /// like from here. Lighting a fault for it would train the operator to
    /// ignore the lamp.
    #[test]
    fn a_stream_that_ended_raises_no_alarm() {
        let panel = render(Link::Gone, Some("Google Chrome"), 0);
        assert!(!panel.contains("taken back"), "{panel}");
    }

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
