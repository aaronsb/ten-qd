//! QD-585 Compact Disc Player.
//!
//! Not in the ad — Fujitsu Ten never built one. Drawn in the same design
//! grammar as the units that surround it, which is the only thing that makes a
//! counterfactual component convincing.
//!
//! The display window shows a track number and elapsed time and nothing else.
//! That is not a limitation to work around; a 1985 player had no character
//! display, and the discipline of it is why the panel reads as period. Track
//! titles live on the shelf strip below the rack, off the panel entirely.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::SPINE;
use crate::state::{Command, Stack, Transport};
use crate::ui::hit::HitMap;
use crate::ui::chassis;
use crate::ui::glyph::{self, transport as tr};
use crate::ui::theme::Theme;

pub fn draw(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme, hits: &mut HitMap) {
    let inner = chassis::bay(buf, area, theme);
    let cd = &stack.cd;
    let running = cd.transport != Transport::Stop;

    // --- spine: EJECT ----------------------------------------------------
    let lamp = Rect::new(inner.x, inner.y, SPINE, 5);
    chassis::lamp(buf, lamp, "EJECT", theme, cd.disc.is_some());
    hits.add(lamp.x, lamp.y, lamp.width, lamp.height, Command::CdEject);

    // --- display window --------------------------------------------------
    let win_area = Rect::new(
        inner.x + SPINE + 1,
        inner.y,
        inner.width.saturating_sub(SPINE + 1),
        6,
    );
    let w = chassis::window(buf, win_area, theme, running);
    if w.width < 40 {
        return;
    }

    // Spinning disc. The glyph alternates so the disc visibly turns; the
    // transport state is the only thing driving it.
    let disc_glyph = if cd.disc.is_none() {
        ' '
    } else if cd.transport == Transport::Play {
        // Four-phase rotation off the elapsed clock.
        ['◐', '◓', '◑', '◒'][((cd.elapsed * 6.0) as usize) % 4]
    } else {
        glyph::transport::DISC.chars().next().unwrap()
    };
    buf.set_string(
        w.x,
        w.y + 1,
        disc_glyph.to_string(),
        Style::default()
            .fg(if running { theme.ink_white } else { theme.ink_grey })
            .bg(theme.window),
    );

    buf.set_string(
        w.x + 2,
        w.y,
        "COMPACT DISC",
        Style::default().fg(theme.ink_white).bg(theme.window),
    );
    buf.set_string(
        w.x + 2,
        w.y + 1,
        "PLAYER QD-585",
        Style::default().fg(theme.ink_white).bg(theme.window),
    );

    // --- music calendar --------------------------------------------------
    // The grid of track numbers every player of this era carried: a number is
    // printed if the disc has that track, lit while it is the one playing.
    // Twenty cells and an OVER lamp, which is exactly how they handled discs
    // with more tracks than the calendar had room for.
    calendar(buf, w.x + 18, w.y, stack, theme, hits);

    // --- numerals --------------------------------------------------------
    let track_text = if cd.track > 0 { format!("{:02}", cd.track.min(99)) } else { "  ".into() };
    let elapsed_text = if cd.track > 0 {
        let s = cd.elapsed.max(0.0) as u64;
        format!("{:02}:{:02}", (s / 60).min(99), s % 60)
    } else {
        "  :  ".into()
    };

    let track_w = glyph::seven_seg_width("88");
    let elapsed_w = glyph::seven_seg_width("88:88");
    let right_col = 11u16; // DISC indicator + sample rate

    // Right-align the numeral group against the indicator column so the
    // display does not shuffle as the window width changes.
    let group_w = track_w + 4 + elapsed_w;
    let tx = w.x + w.width.saturating_sub(right_col + group_w + 1);
    let ex = tx + track_w + 4;

    chassis::vfd(buf, tx, w.y, &track_text, theme);
    chassis::vfd(buf, ex, w.y, &elapsed_text, theme);
    chassis::sublegend(buf, tx, w.y + 3, "TRACK", theme, true);
    chassis::sublegend(buf, ex, w.y + 3, "ELAPSED", theme, true);

    // --- window indicators ----------------------------------------------
    let rx = w.x + w.width.saturating_sub(right_col);
    chassis::boxed_green(buf, rx, w.y, "DISC", theme, running, true);
    let rate = format!("{:.1} kHz", cd.sample_rate as f32 / 1000.0);
    chassis::sublegend(buf, rx, w.y + 2, &rate, theme, true);

    // Total track count, the way a player reports the TOC once it has read it.
    if let Some(d) = &cd.disc {
        let toc = format!("{:02} TRK", d.tracks.len().min(99));
        chassis::sublegend(buf, rx, w.y + 3, &toc, theme, true);
    }

    // --- transport keys --------------------------------------------------
    let ky = inner.y + 6;
    let kx = inner.x + SPINE + 1;
    let playing = cd.transport == Transport::Play;

    let mut row = chassis::KeyRow::new(kx, ky);
    let mut press = |row: &mut chassis::KeyRow, w, label: &str, active, cmd, hits: &mut HitMap| {
        let r = row.key(buf, w, label, theme, active, true);
        hits.add(r.x, r.y, r.width, r.height, cmd);
    };
    press(&mut row, 6, tr::PREV, false, Command::CdPrev, hits);
    press(
        &mut row,
        6,
        if playing { tr::PAUSE } else { tr::PLAY },
        playing,
        Command::CdPlayPause,
        hits,
    );
    press(&mut row, 6, tr::NEXT, false, Command::CdNext, hits);
    press(&mut row, 6, tr::STOP, cd.transport == Transport::Stop, Command::CdStop, hits);
    row.gap(2);
    press(&mut row, 6, "RPT", cd.repeat, Command::CdRepeat, hits);
    press(&mut row, 6, "RND", cd.random, Command::CdRandom, hits);

    // --- badge, model, and the shelf strip -------------------------------
    let badge_x = inner.x + inner.width.saturating_sub(24);
    chassis::badge(buf, badge_x, ky, theme);

    // The one place text is allowed: printed below the panel, not on it.
    let strip = match (cd.disc.as_ref(), cd.current()) {
        (Some(d), Some(t)) => format!("{} — {} · {}", t.artist, t.title, d.title),
        (Some(d), None) => format!("{} · {} tracks", d.title, d.tracks.len()),
        _ => "no disc".to_string(),
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

    chassis::model_corner(buf, inner, &["COMPACT DISC PLAYER QD-585"], theme);
}

/// Two rows of ten. Returns nothing — it draws into the window it is given.
fn calendar(buf: &mut Buffer, x: u16, y: u16, stack: &Stack, theme: &Theme, hits: &mut HitMap) {
    const CELLS: usize = 20;
    let cd = &stack.cd;
    let count = cd.disc.as_ref().map_or(0, |d| d.tracks.len());

    for i in 0..CELLS {
        let n = i + 1;
        let col = (i % 10) as u16 * 3;
        let row = (i / 10) as u16;

        let present = n <= count;
        let current = n == cd.track;

        let style = if current {
            Style::default().fg(theme.vfd).bg(theme.window).add_modifier(Modifier::BOLD)
        } else if present {
            Style::default().fg(theme.vfd_dim).bg(theme.window)
        } else {
            Style::default().fg(theme.window).bg(theme.window)
        };

        let label = if present { format!("{n:2}") } else { "  ".into() };
        buf.set_string(x + col, y + row, label, style);
        if present {
            hits.add_row(x + col, y + row, 2, Command::CdTrack(n - 1));
        }
    }

    // Discs longer than the calendar say so rather than silently truncating.
    chassis::boxed_green(buf, x + 30, y, "OVER", theme, count > CELLS, true);
    chassis::sublegend(buf, x, y + 2, "MUSIC CALENDAR", theme, true);
}
