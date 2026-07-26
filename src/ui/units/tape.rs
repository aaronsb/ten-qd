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
    row.gap(2);

    // APS — Automatic Program Search, the deck's name for track skip. It finds
    // the gaps between tracks rather than knowing where they are.
    press(&mut row, 6, "◀APS", false, Command::TapeApsPrev, hits);
    press(&mut row, 6, "APS▶", false, Command::TapeApsNext, hits);
    row.gap(1);
    press(&mut row, 6, "FLIP", t.side == Side::B, Command::TapeFlip, hits);
    row.gap(2);
    press(&mut row, 8, "EJECT", t.tape.is_some(), Command::TapeEject, hits);


    let strip = match (t.tape.as_ref(), t.current()) {
        (Some(tape), Some(track)) => {
            format!("{} — {} · side {} of {}", track.artist, track.title, t.side.label(), tape.title)
        }
        (Some(tape), None) => format!("{} · {} tracks", tape.title, tape.tracks.len()),
        _ => "no tape".to_string(),
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

    chassis::model_corner(buf, inner, &["STEREO CASSETTE DECK QD-581"], theme);
}
