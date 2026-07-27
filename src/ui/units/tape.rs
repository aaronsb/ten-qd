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

use super::SPINE;
use crate::state::{Command, Side, SourceKind, Stack, Transport, Unit};
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

    // --- what is being written down ---------------------------------------
    // REC is a switch; this lamp is a readout. A deck with its power off is
    // not appending to anything, so the switch can be left where it was and
    // the lamp still tells the truth.
    let rec = t.power && t.rec.on;
    chassis::boxed(buf, w.x + 2, w.y + 3, "REC", theme, rec, true);
    if rec {
        // Both counts are of things that already happened: entries on disk,
        // and players with an entry open that will be written when they end.
        let following = t.rec.following;
        chassis::sublegend(
            buf,
            w.x + 8,
            w.y + 3,
            &format!("{:03} LOG · {following}", t.rec.wrote.min(999)),
            theme,
            true,
        );
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

    let cw = glyph::seven_seg_width("8888");
    let right_col = 12u16;
    let cx = w.x + w.width.saturating_sub(right_col + cw + 2);
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
    press(&mut row, 6, "●REC", rec, Command::TapeRecord, hits);
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
    // TRACK mode is not on the tape, so it says so here rather than in the
    // window: what is being written is a list, and the tape in the deck — if
    // there is one — has nothing to do with it.
    if rec {
        strip.push_str(" · REC to the listening log, not to tape");
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
        let panel = render(RecState { on: true, wrote: 12, following: 2 }, true);
        assert!(panel.contains("012 LOG · 2"), "{panel}");
    }

    /// The whole invariant in one test: REC is a switch position, and the
    /// panel reads the log, not the switch. A deck with no power appends
    /// nothing, so it must not say it is recording.
    #[test]
    fn a_deck_with_no_power_does_not_claim_to_be_recording() {
        let panel = render(RecState { on: true, wrote: 12, following: 2 }, false);
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

    #[test]
    fn a_deck_that_is_not_recording_says_nothing_about_a_log() {
        let panel = render(RecState::default(), true);
        assert!(!panel.contains("LOG"), "{panel}");
        assert!(!panel.contains("listening log"), "{panel}");
    }
}
