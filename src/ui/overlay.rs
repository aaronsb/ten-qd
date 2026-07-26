//! Modal overlays — the key map and the disc/tape browser.
//!
//! Both are drawn straight into the frame after the rack, so they are in
//! viewport coordinates already and their click targets need no scroll
//! translation. The browser registers its rows through the same `HitMap` the
//! panels use, so a row is clickable for the same reason a key cap is.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear};
use ratatui::Frame;

use crate::browser::Browser;
use crate::state::Command;
use crate::ui::hit::HitMap;
use crate::ui::theme::Theme;

fn centred(f: &Frame, w: u16, h: u16) -> Rect {
    let area = f.area();
    Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

fn panel(f: &mut Frame, area: Rect, title: &str, theme: &Theme) -> Rect {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .style(Style::default().bg(theme.window).fg(theme.seam));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

// ---------------------------------------------------------------------------
// Key map
// ---------------------------------------------------------------------------

pub const HELP: &[(&str, &str)] = &[
    ("SOURCE", ""),
    ("c  t  u", "compact disc · cassette · tuner"),
    ("o", "open the disc/tape browser"),
    ("", ""),
    ("TRANSPORT", "(acts on the selected source)"),
    ("SPACE  s", "play/pause · stop"),
    ("← → / p n", "previous · next   (tuner: seek)"),
    ("1-9", "cue track   (tuner: recall preset)"),
    ("! @ # $ % ^", "store the current station as a preset"),
    ("e", "eject"),
    ("r  z", "repeat · random"),
    ("v  y  a", "flip the tape · Dolby · auto-reverse"),
    ("g", "tuner LOCAL (raise the seek squelch)"),
    ("", ""),
    ("CONTROL HEAD", ""),
    ("↑ ↓  m", "volume · attenuator"),
    (", .  / < >", "bass · treble"),
    ("; '", "fader rear/front"),
    ("i  w", "illumination · amplifier power"),
    ("", ""),
    ("EQUALISER", ""),
    ("h l  j k", "select band · cut/boost"),
    ("f  d  0", "front/rear bank · defeat · flat"),
    ("", ""),
    ("click", "any control · wheel scrolls the rack"),
    ("q", "quit"),
];

pub fn draw_help(f: &mut Frame, theme: &Theme) {
    let area = centred(f, 52, HELP.len() as u16 + 2);
    let inner = panel(f, area, "PANEL CONTROLS", theme);

    for (i, (key, desc)) in HELP.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        // A row with no description is a section heading, not a binding.
        let style = if desc.is_empty() && !key.is_empty() {
            Style::default().fg(theme.ink_legend).bg(theme.window).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.vfd).bg(theme.window).add_modifier(Modifier::BOLD)
        };
        f.buffer_mut().set_string(inner.x + 1, y, key, style);
        f.buffer_mut().set_string(
            inner.x + 14,
            y,
            desc,
            Style::default().fg(theme.ink_grey).bg(theme.window),
        );
    }
}

// ---------------------------------------------------------------------------
// Browser
// ---------------------------------------------------------------------------

pub fn draw_browser(f: &mut Frame, b: &Browser, theme: &Theme, hits: &mut HitMap) {
    let rows = b.entries.len().clamp(6, 18) as u16;
    let area = centred(f, 74, rows + 6);
    let inner = panel(f, area, "SELECT DISC OR TAPE", theme);
    if inner.height < 4 {
        return;
    }

    // Path bar, tail-truncated: the end of a path identifies it, the start
    // rarely does.
    let shown = b.cwd.display().to_string();
    let max = inner.width.saturating_sub(2) as usize;
    let shown = if shown.chars().count() > max {
        let skip = shown.chars().count() - max + 1;
        format!("…{}", shown.chars().skip(skip).collect::<String>())
    } else {
        shown
    };
    f.buffer_mut().set_string(
        inner.x + 1,
        inner.y,
        &shown,
        Style::default().fg(theme.ink_white).bg(theme.window),
    );

    let list_top = inner.y + 2;
    let list_h = inner.height.saturating_sub(4);

    // Scroll the list so the cursor stays visible.
    let first = b.cursor.saturating_sub(list_h.saturating_sub(1) as usize / 2);
    let first = first.min(b.entries.len().saturating_sub(list_h as usize));

    if b.entries.is_empty() {
        f.buffer_mut().set_string(
            inner.x + 2,
            list_top,
            "no folders here",
            Style::default().fg(theme.ink_grey).bg(theme.window),
        );
    }

    for row in 0..list_h {
        let idx = first + row as usize;
        let Some(e) = b.entries.get(idx) else { break };
        let y = list_top + row;
        let selected = idx == b.cursor;

        let style = if !e.playable() {
            Style::default().fg(theme.ink_grey).bg(theme.window).add_modifier(Modifier::DIM)
        } else if selected {
            Style::default().fg(theme.window).bg(theme.vfd).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.ink_white).bg(theme.window)
        };

        // Blank the whole row first so the selection bar spans the panel.
        let w = inner.width.saturating_sub(2) as usize;
        f.buffer_mut().set_string(inner.x + 1, y, " ".repeat(w), style);

        let name: String = e.name.chars().take(w.saturating_sub(18)).collect();
        f.buffer_mut().set_string(inner.x + 2, y, &name, style);

        // `here` is what a disc load would give you, `below` what a tape would.
        let counts = format!("{:>4} disc {:>5} tape", e.here, e.below);
        let cx = inner.x + inner.width.saturating_sub(counts.len() as u16 + 2);
        f.buffer_mut().set_string(cx, y, &counts, style);

        hits.add_row(inner.x + 1, y, inner.width.saturating_sub(2), Command::BrowserSelect(idx));
    }

    // Footer: the two ways to load, and the error if the last attempt failed.
    let fy = inner.y + inner.height - 1;
    let footer: String = match &b.error {
        Some(e) => e.clone(),
        // Kept short enough to survive the panel's inner width — a clipped
        // hint is worse than a terse one.
        None => "↑↓ move · → enter · ← back · d load DISC · t load TAPE · esc close".to_string(),
    };
    let style = if b.error.is_some() {
        Style::default().fg(theme.ink_red).bg(theme.window)
    } else {
        Style::default().fg(theme.ink_legend).bg(theme.window)
    };
    let footer: String = footer.chars().take(inner.width.saturating_sub(2) as usize).collect();
    f.buffer_mut().set_string(inner.x + 1, fy, &footer, style);
}
