//! QD-581 Cassette Deck — a playlist transport.
//!
//! A tape is a playlist, and the two sides are the two halves of it. That is
//! not a metaphor stretched to fit: a cassette holds a fixed running time per
//! side, so `Tape::from_tracks` splits where the cumulative time crosses the
//! midpoint, exactly as anyone compiling one used to.
//!
//! The display is the reason this unit exists alongside the CD player. A deck
//! has no idea what track it is on — it shows a **linear counter**, four
//! digits, reset when you turn the tape over. Everything the CD player states
//! precisely, the deck can only approximate, and that difference is the whole
//! character of the machine.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::{aux_in, SPINE};
use crate::audio::record;
use crate::state::{Arm, Command, RecMode, Side, SourceKind, Stack, Transport, Unit};
use crate::ui::chassis;
use crate::ui::glyph::{self, transport as tr};
use crate::ui::hit::HitMap;
use crate::ui::theme::Theme;

/// The four hub-rotation phases, so a running reel visibly turns.
const REEL: [char; 4] = ['◜', '◝', '◞', '◟'];

pub fn draw(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme, hits: &mut HitMap) {
    let inner = chassis::bay(buf, area, theme);
    let t = &stack.tape;
    let live = stack.source == SourceKind::Tape;
    let running = t.transport.is_running();

    chassis::legend(buf, inner.x, inner.y, "CASSETTE", theme);
    let lamp = Rect::new(inner.x, inner.y + 1, SPINE, 5);
    chassis::lamp(buf, lamp, "POWER", theme, t.power);
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::UnitPower(Unit::Tape));



    // --- window -----------------------------------------------------------
    let win = Rect::new(
        inner.x + SPINE + 1,
        inner.y,
        inner.width.saturating_sub(SPINE + 1),
        6,
    );
    let w = chassis::window(buf, win, theme, live && running);
    if w.width < 40 {
        return;
    }

    // Reels. The phase runs off the counter, so they turn at the tape's pace.
    let phase = (t.counter * 4.0) as usize;
    let reel = |n: usize| {
        if t.transport == Transport::Play {
            REEL[(phase + n) % 4]
        } else if t.transport == Transport::Ff || t.transport == Transport::Rew {
            REEL[(phase * 3 + n) % 4]
        } else {
            '◯'
        }
    };
    let hub = Style::default()
        .fg(if running { theme.vfd } else { theme.ink_grey })
        .bg(theme.window);
    buf.set_string(w.x, w.y + 1, reel(0).to_string(), hub);

    buf.set_string(
        w.x + 2,
        w.y,
        "CASSETTE",
        Style::default().fg(theme.ink_white).bg(theme.window),
    );
    buf.set_string(
        w.x + 2,
        w.y + 1,
        "DECK QD-581",
        Style::default().fg(theme.ink_white).bg(theme.window),
    );

    // Where the counter sits. Computed before anything else on its row, since
    // it is right-aligned and everything to its left has to know where it ends.
    let cw = glyph::seven_seg_width("8888");
    let right_col = 12u16;
    let cx = w.x + w.width.saturating_sub(right_col + cw + 2);

    // --- what is being written down ---------------------------------------
    // REC is a switch; this lamp is a readout. A deck with its power off is
    // not writing anything, and neither is one whose last write failed, so
    // the switch can be left where it was and the lamp still tells the truth.
    //
    // Both modes share this row, because they share the button. What differs
    // is what the counts beside it count.
    let audio = t.rec.mode == RecMode::Audio;
    let rec = t.power
        && !t.rec.failed
        && if audio { t.rec.arm == Arm::Running } else { t.rec.on };
    let armed = audio && t.power && !t.rec.failed && t.rec.arm == Arm::Armed;

    chassis::boxed(buf, w.x + 2, w.y + 3, "REC", theme, rec, true);
    // The mode is always legible, lit or not: which machine the button drives
    // is not something to work out from what the counts happen to say.
    chassis::sublegend(buf, w.x + 2, w.y + 2, t.rec.mode.label(), theme, true);

    // Everything on this row shares it with COUNTER, which is right-aligned
    // and moves with the bay's width. Drawn only where it fits, because a
    // readout overprinting another produces a third that says neither.
    let room = cx.saturating_sub(w.x + 9) as usize;
    let counts = if audio {
        // Seconds committed and bytes on disk — both measured by the writer,
        // neither predicted. A take with a hole in it says so instead of
        // reporting a length it cannot stand behind.
        //
        // Gated on the lamp, not on the arm: a deck out of the signal path is
        // writing nothing, and the length of what it was writing before is not
        // a fact about now.
        let s = t.rec.take_seconds as u64;
        match (if rec || armed { t.rec.arm } else { Arm::Idle }, t.rec.dropped && rec) {
            (_, true) => "GAP".to_string(),
            (Arm::Armed, _) => t.rec.arm.label().to_string(),
            (Arm::Running, _) => {
                format!("{:02}:{:02} {}", s / 60, s % 60, record::size(t.rec.take_bytes))
            }
            (Arm::Idle, _) => String::new(),
        }
    } else if rec {
        format!("{:03} LOG · {}", t.rec.wrote.min(999), t.rec.following)
    } else {
        String::new()
    };
    if !counts.is_empty() && counts.chars().count() <= room {
        chassis::sublegend(buf, w.x + 8, w.y + 3, &counts, theme, true);
    }
    // `glyph::boxed` adds a rule either side, so this is eight cells, and a
    // fault warning overprinting COUNTER would be one more readout that says
    // neither thing.
    if t.rec.failed && t.power && room >= 8 {
        chassis::boxed(buf, w.x + 8, w.y + 3, if audio { "NO FILE" } else { "NO LOG" }, theme, true, true);
    }

    // --- record level -----------------------------------------------------
    // Its own gain stage, and deliberately drawn nothing like the equaliser's
    // GAIN: that one is downstream of the tap and cannot serve here, and two
    // controls that look alike is how people reach for the wrong one.
    //
    // The meter runs while armed as well as while running — that is what
    // arming is *for*. It reads after the level trim, so what it shows is what
    // the file is getting.
    if audio && (rec || armed) {
        let label = format!("LEVEL {:+3} ", t.rec.level_db);
        let mx = w.x + 2 + label.chars().count() as u16;
        let bars = cx.saturating_sub(mx + 7).min(12);
        if bars >= 4 {
            chassis::sublegend(buf, w.x + 2, w.y + 2, &label, theme, true);
            chassis::ramp_bar(buf, mx, w.y + 2, bars, aux_in::scale(t.rec.input), theme);
            chassis::sublegend(
                buf,
                mx + bars + 1,
                w.y + 2,
                &aux_in::readout(t.rec.input),
                theme,
                true,
            );
        }
    }

    // --- the counter ------------------------------------------------------
    // Four digits and no colon: a deck counts tape, not time, and cannot tell
    // you where a track begins. With an adapter in, the hubs are turning
    // against a loop of tape that is not the music — so the counter still
    // runs, and now it means nothing at all.
    let counter = if t.tape.is_some() {
        let c = (t.counter.max(0.0) as u64).min(5999);
        format!("{:02}{:02}", c / 60, c % 60)
    } else {
        "    ".to_string()
    };

    chassis::vfd(buf, cx, w.y, &counter, theme);
    chassis::sublegend(buf, cx, w.y + 3, "COUNTER", theme, true);

    // Side, and the reel at the far end of the tape path.
    let sx = cx.saturating_sub(10);
    chassis::sublegend(buf, sx, w.y, "SIDE", theme, true);
    buf.set_string(
        sx + 5,
        w.y,
        t.side.label(),
        Style::default()
            .fg(if live { theme.vfd } else { theme.ink_grey })
            .bg(theme.window)
            .add_modifier(Modifier::BOLD),
    );
    hits.add_row(sx, w.y, 7, Command::TapeFlip);
    buf.set_string(sx + 7, w.y + 1, reel(2).to_string(), hub);

    // Which side is longer is worth knowing before you commit to it.
    if let Some(tape) = &t.tape {
        let secs = tape.side_seconds(t.side) as u64;
        chassis::sublegend(
            buf,
            sx,
            w.y + 2,
            &format!("{:02}:{:02}", secs / 60, secs % 60),
            theme,
            true,
        );
        let n = tape.side_range(t.side).len();
        chassis::sublegend(buf, sx, w.y + 3, &format!("{n:02} TRK"), theme, true);
    }

    // --- window indicators ------------------------------------------------
    let rx = w.x + w.width.saturating_sub(right_col);
    chassis::boxed(buf, rx, w.y, "REW", theme, t.transport == Transport::Rew, true);
    chassis::boxed(buf, rx + 6, w.y, "FF", theme, t.transport == Transport::Ff, true);
    chassis::boxed_green(buf, rx, w.y + 1, "DOLBY", theme, t.dolby, true);
    hits.add_row(rx, w.y + 1, 7, Command::TapeDolby);
    chassis::boxed_green(buf, rx, w.y + 2, "A.REV", theme, t.auto_reverse, true);
    hits.add_row(rx, w.y + 2, 7, Command::TapeAutoReverse);
    // Metal is what you would have put a compilation on if you were serious.
    chassis::sublegend(buf, rx, w.y + 3, "MTL", theme, true);

    // --- keys -------------------------------------------------------------
    let ky = inner.y + 6;
    let kx = inner.x + SPINE + 1;
    let playing = t.transport == Transport::Play;

    let mut row = chassis::KeyRow::new(kx, ky);
    let mut press = |row: &mut chassis::KeyRow, w, label: &str, active, cmd, hits: &mut HitMap| {
        let r = row.key(buf, w, label, theme, active, true);
        hits.add(r.x, r.y, r.width, r.height, cmd);
    };
    press(&mut row, 6, tr::REW, t.transport == Transport::Rew, Command::TapeRew, hits);
    press(
        &mut row,
        6,
        if playing { tr::PAUSE } else { tr::PLAY },
        playing,
        Command::TapePlayPause,
        hits,
    );
    press(&mut row, 6, tr::FF, t.transport == Transport::Ff, Command::TapeFf, hits);
    press(&mut row, 6, tr::STOP, t.transport == Transport::Stop, Command::TapeStop, hits);
    row.gap(1);
    // Next to the transport because that is where it was, but it is not one:
    // REC writes down what every player on the desktop is doing, which carries
    // on regardless of what this deck is playing.
    press(&mut row, 6, "●REC", rec || armed, Command::TapeRecord, hits);
    press(&mut row, 6, t.rec.mode.label(), audio, Command::TapeRecMode, hits);
    row.gap(2);

    // APS — Automatic Program Search, the deck's name for track skip. It finds
    // the gaps between tracks rather than knowing where they are.
    press(&mut row, 6, "◀APS", false, Command::TapeApsPrev, hits);
    press(&mut row, 6, "APS▶", false, Command::TapeApsNext, hits);
    row.gap(1);
    press(&mut row, 6, "FLIP", t.side == Side::B, Command::TapeFlip, hits);
    row.gap(2);
    press(&mut row, 8, "EJECT", t.tape.is_some(), Command::TapeEject, hits);


    let mut strip = match (t.tape.as_ref(), t.current()) {
        (Some(tape), Some(track)) => {
            format!("{} — {} · side {} of {}", track.artist, track.title, t.side.label(), tape.title)
        }
        (Some(tape), None) => format!("{} · {} tracks", tape.title, tape.tracks.len()),
        _ => "no tape".to_string(),
    };
    // Neither mode writes to the tape in the deck, and each is not-the-tape
    // in a different way — so the strip says which, rather than one sentence
    // covering both and being wrong about one of them.
    match (t.rec.mode, rec, armed) {
        (RecMode::Track, true, _) => strip.push_str(" · REC to the listening log, not to tape"),
        (RecMode::Audio, true, _) => {
            strip.push_str(" · REC to a file, pre-EQ — volume does not touch it")
        }
        (RecMode::Audio, _, true) => {
            strip.push_str(" · REC PAUSE — meters live, nothing written yet")
        }
        _ => {}
    }
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

    chassis::model_corner(buf, inner, &["STEREO CASSETTE DECK QD-581"], theme);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RecState;

    fn audio(arm: Arm) -> RecState {
        RecState {
            mode: RecMode::Audio,
            arm,
            level_db: -3,
            input: 0.42,
            take_seconds: 154.0,
            take_bytes: 27_000_000,
            ..Default::default()
        }
    }

    /// The bay as text, so what the panel claims can be asserted on.
    fn render(rec: RecState, power: bool) -> String {
        let mut stack = Stack::default();
        stack.tape.rec = rec;
        stack.tape.power = power;
        let area = Rect::new(0, 0, 120, 12);
        let mut buf = Buffer::empty(area);
        let theme = Theme::for_stack(&stack);
        draw(&mut buf, area, &stack, &theme, &mut HitMap::new());
        (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_recording_deck_reports_what_it_has_written() {
        let panel = render(RecState { on: true, wrote: 12, following: 2, ..Default::default() }, true);
        assert!(panel.contains("012 LOG · 2"), "{panel}");
    }

    /// The whole invariant in one test: REC is a switch position, and the
    /// panel reads the log, not the switch. A deck with no power appends
    /// nothing, so it must not say it is recording.
    #[test]
    fn a_deck_with_no_power_does_not_claim_to_be_recording() {
        let panel = render(RecState { on: true, wrote: 12, following: 2, ..Default::default() }, false);
        assert!(!panel.contains("LOG ·"), "a dead deck counted entries: {panel}");
        assert!(!panel.contains("listening log"), "a dead deck claimed to record: {panel}");
    }

    /// TRACK mode writes a list, and the tape in the deck has nothing to do
    /// with it. Saying so is the difference between a readout and a wish.
    #[test]
    fn the_strip_says_the_recording_is_not_going_to_tape() {
        let panel = render(RecState { on: true, ..Default::default() }, true);
        assert!(panel.contains("REC to the listening log, not to tape"), "{panel}");
    }

    /// The status line reporting an unwritable log scrolls away in seconds;
    /// the REC lamp does not. A lamp burning over a log taking no writes is
    /// the display wishing.
    #[test]
    fn a_log_that_cannot_be_written_puts_the_rec_lamp_out() {
        let panel = render(
            RecState { on: true, wrote: 12, following: 2, failed: true, ..Default::default() },
            true,
        );
        assert!(!panel.contains("LOG ·"), "a failed log kept counting: {panel}");
        assert!(panel.contains("NO LOG"), "and must say so where the lamp was: {panel}");
        assert!(!panel.contains("listening log"), "{panel}");
    }

    /// The counts share a row with COUNTER, which is right-aligned. In a
    /// narrow terminal they used to overprint into `012COUNTER2`.
    #[test]
    fn the_counts_give_way_rather_than_overprint_the_counter() {
        // Both readouts that share the row with COUNTER: the running counts,
        // and the NO LOG warning that replaces them. The warning was the one
        // that still overprinted, and a sweep that only set `failed: false`
        // could never have seen it.
        for failed in [false, true] {
            let rec = RecState { on: true, wrote: 12, following: 2, failed, ..Default::default() };
            let mut drawn = 0;
            for width in 50..=120u16 {
                let mut stack = Stack::default();
                stack.tape.rec = rec.clone();
                let area = Rect::new(0, 0, width, 12);
                let mut buf = Buffer::empty(area);
                let theme = Theme::for_stack(&stack);
                draw(&mut buf, area, &stack, &theme, &mut HitMap::new());
                let text: String = (0..area.width).map(|x| buf[(x, 5)].symbol()).collect();
                // The closing rule, not just the word: `NO LOG` is six
                // characters inside an eight-cell box, so it survives a cell
                // of overprint and `contains("NO LOG")` would stay green over
                // a visibly garbled row. The rule is the cell COUNTER eats
                // first, which makes it the thing worth asserting.
                let shown = if failed { "NO LOG▕" } else { "012 LOG · 2" };
                if text.contains("LOG") {
                    drawn += 1;
                    assert!(text.contains("COUNTER"), "at {width} ({failed}): {text}");
                    assert!(text.contains(shown), "at {width} ({failed}): {text}");
                }
            }
            assert!(drawn > 0, "nothing drawn for failed={failed} — this proves nothing");
        }
    }

    // ---- AUDIO mode ---------------------------------------------------

    #[test]
    fn a_rolling_take_reports_its_length_and_its_cost() {
        let panel = render(audio(Arm::Running), true);
        assert!(panel.contains("02:34 27 MB"), "{panel}");
        assert!(panel.contains("LEVEL  -3"), "the level is its own control: {panel}");
        assert!(panel.contains("pre-EQ"), "and says why that matters: {panel}");
    }

    /// The whole point of arming: meters live, nothing committed. A panel that
    /// showed a running take here would be inviting the operator to believe a
    /// file was growing.
    #[test]
    fn an_armed_deck_meters_but_claims_no_take() {
        let panel = render(audio(Arm::Armed), true);
        assert!(panel.contains("PAUSE"), "{panel}");
        assert!(panel.contains("LEVEL"), "the meter is the reason to arm: {panel}");
        assert!(!panel.contains("02:34"), "nothing has been written: {panel}");
        assert!(panel.contains("nothing written yet"), "{panel}");
    }

    #[test]
    fn an_idle_deck_in_audio_mode_shows_no_meter_and_no_take() {
        let panel = render(audio(Arm::Idle), true);
        assert!(panel.contains("AUDIO"), "the mode is always legible: {panel}");
        assert!(!panel.contains("LEVEL"), "{panel}");
        assert!(!panel.contains("02:34"), "{panel}");
    }

    /// Same invariant as TRACK, different file. A deck out of the signal path
    /// is writing nothing, whichever machine the button is driving.
    #[test]
    fn a_dead_deck_claims_no_take_either() {
        let panel = render(audio(Arm::Running), false);
        assert!(!panel.contains("02:34"), "{panel}");
        assert!(!panel.contains("LEVEL"), "{panel}");
        assert!(!panel.contains("pre-EQ"), "{panel}");
    }

    /// A take the writer could not keep up with has a hole in it. Reporting
    /// its length as though it were continuous would be the worst kind of
    /// readout: precise, and wrong.
    #[test]
    fn a_take_with_a_gap_in_it_says_so_instead_of_reporting_a_length() {
        let panel = render(RecState { dropped: true, ..audio(Arm::Running) }, true);
        assert!(panel.contains("GAP"), "{panel}");
        assert!(!panel.contains("02:34"), "a torn take must not report a clean length: {panel}");
    }

    #[test]
    fn a_file_that_cannot_be_written_puts_the_rec_lamp_out_in_audio_mode_too() {
        let panel = render(RecState { failed: true, ..audio(Arm::Running) }, true);
        assert!(panel.contains("NO FILE"), "{panel}");
        assert!(!panel.contains("02:34"), "{panel}");
        assert!(!panel.contains("pre-EQ"), "{panel}");
    }

    #[test]
    fn a_deck_that_is_not_recording_says_nothing_about_a_log() {
        let panel = render(RecState::default(), true);
        assert!(!panel.contains("LOG"), "{panel}");
        assert!(!panel.contains("listening log"), "{panel}");
    }
}
