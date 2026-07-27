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

use crate::browser::{Browser, Kind};
use crate::state::{Stack, Unit};
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

/// The key map. The third field marks a section heading.
///
/// Explicit, because it used to be inferred from the description being empty —
/// which quietly demoted the two headings that carry a parenthetical, so
/// TRANSPORT and THE RACK were painted as bindings while the other three were
/// not. A heading is a fact about the row, not something to deduce from it.
pub const HELP: &[(&str, &str, bool)] = &[
    ("SOURCE", "", true),
    ("c  t  u  a", "compact disc · cassette · tuner · aux", false),
    ("o", "open the disc/tape browser", false),
    ("", "", false),
    ("TRANSPORT", "(acts on the selected source)", true),
    ("SPACE  s", "play/pause · stop", false),
    ("← → / p n", "previous · next   (tuner: seek)", false),
    ("1-9", "cue track   (tuner: recall preset)", false),
    ("! @ # $ % ^", "store the current station as a preset", false),
    ("e", "eject", false),
    ("r  z", "repeat · random", false),
    ("v  y  b", "flip the tape · Dolby · auto-reverse", false),
    ("g  P", "tuner LOCAL · tuner power", false),
    ("A", "aux: pick what to send through the rack", false),
    ("1-9", "aux: plug that stream in", false),
    ("", "", false),
    ("RECORD", "(two machines sharing one button)", true),
    ("M", "mode: TRACK a list of what played · AUDIO the signal", false),
    ("R", "TRACK: log every player · AUDIO: arm, roll, stop", false),
    ("( )", "record level, ±12 dB — separate from volume and GAIN", false),
    ("", "cut a tape out of it: ten-qd --log, ten-qd --export", false),
    ("", "", false),
    ("CONTROL HEAD", "", true),
    ("↑ ↓  m", "volume · attenuator", false),
    (", .  / < >", "bass · treble", false),
    ("; '", "fader rear/front", false),
    ("i  w", "illumination colour · amplifier power", false),
    ("-  =", "instrument dimmer, down and up", false),
    ("O", "output device the rack drives", false),
    ("", "", false),
    ("THE RACK", "(a folded or removed unit still plays)", true),
    ("~", "arrange the rack — order, and what is in it", false),
    ("click POWER", "take a unit out of the signal path, or back", false),
    ("C T U X", "fold away: CD · cassette · tuner · aux", false),
    ("E W H", "fold away: equaliser · amplifier · control head", false),
    ("", "", false),
    ("EQUALISER", "", true),
    ("h l  j k", "select band · cut/boost", false),
    ("f  d  0", "front/rear bank · defeat · flat", false),
    ("{ }", "output trim — cut/boost, ±12 dB", false),
    ("", "", false),
    ("click", "any control · wheel scrolls the rack", false),
    ("q", "quit", false),
];

/// Width the key map needs in order to print every description in full.
fn help_width() -> u16 {
    let widest = HELP.iter().map(|(_, d, _)| d.chars().count()).max().unwrap_or(0) as u16;
    (widest + 17).max(52)
}

pub fn draw_help(f: &mut Frame, theme: &Theme) {
    // Sized from the table rather than pinned to a number, so a binding with a
    // longer description widens the panel instead of being quietly beheaded.
    // 14 is where the description column starts, plus a border and a margin
    // either side.
    let area = centred(f, help_width(), HELP.len() as u16 + 2);
    let inner = panel(f, area, "PANEL CONTROLS", theme);

    for (i, (key, desc, heading)) in HELP.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let style = if *heading {
            Style::default().fg(theme.ink_legend).bg(theme.window).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.vfd).bg(theme.window).add_modifier(Modifier::BOLD)
        };
        f.buffer_mut().set_string(inner.x + 1, y, key, style);
        // Clip to the panel rather than running off its right edge.
        let room = inner.width.saturating_sub(15) as usize;
        let desc: String = desc.chars().take(room).collect();
        f.buffer_mut().set_string(
            inner.x + 14,
            y,
            &desc,
            Style::default().fg(theme.ink_grey).bg(theme.window),
        );
    }
}

// ---------------------------------------------------------------------------
// The aux source picker
// ---------------------------------------------------------------------------

/// What is playing on this machine, and could be sent through the rack.
///
/// This replaced pressing a number at the cassette bay. Numbering things the
/// operator has to count is fine when the list is the disc's own tracks and
/// fixed; it is a poor way to choose between "Chromium" and "Spotify", which
/// come and go while you are looking at them.
pub fn draw_aux(
    f: &mut Frame,
    streams: &[crate::adapter::Stream],
    cursor: usize,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let rows = streams.len().clamp(3, 14) as u16;
    let area = centred(f, 66, rows + 6);
    let inner = panel(f, area, "AUX SOURCE", theme);
    if inner.height < 4 {
        return;
    }

    let caption = "what is playing — pick one to send through the rack";
    let caption: String = caption.chars().take(inner.width.saturating_sub(2) as usize).collect();
    f.buffer_mut().set_string(
        inner.x + 1,
        inner.y,
        &caption,
        Style::default().fg(theme.ink_grey).bg(theme.window),
    );

    let list_top = inner.y + 2;
    let list_h = inner.height.saturating_sub(4);

    if streams.is_empty() {
        // Not an error: nothing is playing, which is a perfectly ordinary
        // state and has an obvious remedy.
        f.buffer_mut().set_string(
            inner.x + 2,
            list_top,
            "nothing is playing",
            Style::default().fg(theme.ink_grey).bg(theme.window),
        );
        f.buffer_mut().set_string(
            inner.x + 2,
            list_top + 1,
            format!("or choose \"{}\" in the player itself", crate::adapter::DESCRIPTION),
            Style::default().fg(theme.ink_grey).bg(theme.window).add_modifier(Modifier::DIM),
        );
    }

    for row in 0..list_h {
        let idx = row as usize;
        let Some(stream) = streams.get(idx) else { break };
        let y = list_top + row;
        let selected = idx == cursor;

        let style = if selected {
            Style::default().fg(theme.window).bg(theme.vfd).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.ink_white).bg(theme.window)
        };

        let w = inner.width.saturating_sub(2) as usize;
        f.buffer_mut().set_string(inner.x + 1, y, " ".repeat(w), style);
        let label: String = stream.label().chars().take(w.saturating_sub(4)).collect();
        f.buffer_mut().set_string(inner.x + 2, y, &label, style);
        hits.add_row(inner.x + 1, y, inner.width.saturating_sub(2), Command::AuxSelect(idx));
    }

    f.buffer_mut().set_string(
        inner.x + 1,
        inner.y + inner.height - 1,
        "↑↓ move · ⏎ choose · esc close",
        Style::default().fg(theme.ink_legend).bg(theme.window),
    );
}

// ---------------------------------------------------------------------------
// The rack arranger
// ---------------------------------------------------------------------------

/// Which boxes are in the cabinet, and in what order.
///
/// Nothing here touches the audio: a unit taken out still plays, still answers
/// its keys, and still lights its lamp. This is a cabinet, not a signal path —
/// which is why the footer says "out of the rack" rather than "off".
pub fn draw_layout(f: &mut Frame, stack: &Stack, cursor: usize, grabbed: bool, theme: &Theme) {
    const HINT: &str = "↑↓ move · space carry · enter in/out · r reset · esc done";
    let w = HINT.chars().count() as u16 + 4;
    let area = centred(f, w, Unit::ALL.len() as u16 + 6);
    let inner = panel(f, area, "ARRANGE THE RACK", theme);
    if inner.height < 4 {
        return;
    }

    f.buffer_mut().set_string(
        inner.x + 1,
        inner.y,
        "a unit taken out still plays — it is only out of sight",
        Style::default().fg(theme.ink_grey).bg(theme.window).add_modifier(Modifier::DIM),
    );

    for (i, u) in stack.layout.order.iter().enumerate() {
        let y = inner.y + 2 + i as u16;
        if y + 2 >= inner.y + inner.height {
            break;
        }
        let here = i == cursor;
        let shown = !stack.layout.is_hidden(*u);

        let style = if here && grabbed {
            Style::default().fg(theme.window).bg(theme.led_a).add_modifier(Modifier::BOLD)
        } else if here {
            Style::default().fg(theme.window).bg(theme.vfd).add_modifier(Modifier::BOLD)
        } else if shown {
            Style::default().fg(theme.ink_white).bg(theme.window)
        } else {
            Style::default().fg(theme.ink_grey).bg(theme.window).add_modifier(Modifier::DIM)
        };

        let row_w = inner.width.saturating_sub(2) as usize;
        f.buffer_mut().set_string(inner.x + 1, y, " ".repeat(row_w), style);

        // Carrying a unit is shown by the grip, not only by the highlight, so
        // the mode is legible in a screenshot as well as in motion.
        let grip = if here && grabbed { "⇕" } else if here { "▸" } else { " " };
        f.buffer_mut().set_string(inner.x + 1, y, grip, style);

        // In the rack or out of it, said the same way the rack itself says it:
        // a lamp that is lit or a lamp that is not. Strikethrough would have
        // been the obvious alternative and is not reliably drawn by terminals.
        let lamp = if shown { "●" } else { "·" };
        let lamp_style = if here {
            style
        } else {
            Style::default().fg(if shown { theme.led_a } else { theme.led_off }).bg(theme.window)
        };
        f.buffer_mut().set_string(inner.x + 3, y, lamp, lamp_style);
        f.buffer_mut().set_string(inner.x + 5, y, u.label(), style);

        let note = if !shown {
            "out of the rack"
        } else if stack.layout.is_collapsed(*u) {
            "folded"
        } else {
            ""
        };
        if !note.is_empty() {
            let nx = inner.x + inner.width.saturating_sub(note.len() as u16 + 2);
            f.buffer_mut().set_string(nx, y, note, style);
        }
    }

    f.buffer_mut().set_string(
        inner.x + 1,
        inner.y + inner.height - 1,
        HINT,
        Style::default().fg(theme.ink_legend).bg(theme.window),
    );
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

        let mark = if e.kind == Kind::Playlist { "≡ " } else { "▸ " };
        let name: String = e.name.chars().take(w.saturating_sub(20)).collect();
        f.buffer_mut().set_string(inner.x + 2, y, mark, style);
        f.buffer_mut().set_string(inner.x + 4, y, &name, style);

        // `here` is what a disc load would give you, `below` what a tape
        // would. A playlist is only ever a tape.
        let counts = match e.kind {
            Kind::Playlist => format!("{:>15} tape", e.below),
            Kind::Folder => format!("{:>4} disc {:>5} tape", e.here, e.below),
        };
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

// ---------------------------------------------------------------------------
// Output devices
// ---------------------------------------------------------------------------

/// The output picker, in the browser's clothes.
///
/// Same panel, same selection bar, same keys — because it is the same question
/// in a different domain: pick one of these, that is where it goes. A control
/// that opened a differently-shaped dialog would be teaching a second habit
/// for no reason.
pub fn draw_outputs(f: &mut Frame, stack: &Stack, cursor: usize, theme: &Theme, hits: &mut HitMap) {
    let rows = stack.outputs.len().clamp(3, 14) as u16;
    let area = centred(f, 66, rows + 6);
    let inner = panel(f, area, "OUTPUT DEVICE", theme);
    if inner.height < 4 {
        return;
    }

    // Clipped to the panel: a caption that runs off the right edge is the
    // same defect as the one the help box had.
    let caption = "where the rack sends sound, not the system default";
    let caption: String = caption.chars().take(inner.width.saturating_sub(2) as usize).collect();
    f.buffer_mut().set_string(
        inner.x + 1,
        inner.y,
        &caption,
        Style::default().fg(theme.ink_grey).bg(theme.window),
    );

    let list_top = inner.y + 2;
    let list_h = inner.height.saturating_sub(4);
    let first = cursor
        .saturating_sub(list_h.saturating_sub(1) as usize / 2)
        .min(stack.outputs.len().saturating_sub(list_h as usize));

    if stack.outputs.is_empty() {
        f.buffer_mut().set_string(
            inner.x + 2,
            list_top,
            "no output devices offered",
            Style::default().fg(theme.ink_red).bg(theme.window),
        );
    }

    for row in 0..list_h {
        let idx = first + row as usize;
        let Some(name) = stack.outputs.get(idx) else { break };
        let y = list_top + row;
        let selected = idx == cursor;
        let current = stack.output.as_deref() == Some(name.as_str());

        let style = if selected {
            Style::default().fg(theme.window).bg(theme.vfd).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.ink_white).bg(theme.window)
        };

        let w = inner.width.saturating_sub(2) as usize;
        f.buffer_mut().set_string(inner.x + 1, y, " ".repeat(w), style);
        // A mark for the one in force, so the list says which is chosen rather
        // than only which is highlighted.
        f.buffer_mut().set_string(inner.x + 2, y, if current { "▪ " } else { "  " }, style);
        let shown: String = name.chars().take(w.saturating_sub(5)).collect();
        f.buffer_mut().set_string(inner.x + 4, y, &shown, style);

        hits.add_row(inner.x + 1, y, inner.width.saturating_sub(2), Command::OutputsSelect(idx));
    }

    f.buffer_mut().set_string(
        inner.x + 1,
        inner.y + inner.height - 1,
        "↑↓ move · ⏎ choose · esc close",
        Style::default().fg(theme.ink_legend).bg(theme.window),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key map is the one place a user goes when they do not know what a
    /// control does, so a description that runs off the edge is worse there
    /// than anywhere else on the panel.
    #[test]
    fn every_binding_prints_its_description_in_full() {
        let inner = help_width() - 2;
        let room = inner.saturating_sub(15) as usize;
        for (key, desc, _) in HELP {
            assert!(
                desc.chars().count() <= room,
                "\"{desc}\" needs {} of {room} columns (key {key:?})",
                desc.chars().count()
            );
        }
    }

    /// Every heading is painted alike. This was inferred from the description
    /// being empty, which silently demoted the two headings that carry a
    /// parenthetical — TRANSPORT and THE RACK read as bindings.
    #[test]
    fn every_section_heading_is_marked_as_one() {
        let heads: Vec<&str> = HELP.iter().filter(|(_, _, h)| *h).map(|(k, _, _)| *k).collect();
        assert_eq!(
            heads,
            vec!["SOURCE", "TRANSPORT", "RECORD", "CONTROL HEAD", "THE RACK", "EQUALISER"],
            "a heading was missed, or a binding was mistaken for one"
        );
        // And nothing that answers a keypress is dressed as a heading.
        for (key, desc, heading) in HELP {
            if *heading {
                assert!(!key.is_empty(), "a heading needs a name");
            } else if !key.is_empty() {
                assert!(!desc.is_empty(), "a binding must say what it does: {key:?}");
            }
        }
    }

    #[test]
    fn keys_do_not_collide_with_their_descriptions() {
        // Keys start at inner.x + 1, descriptions at inner.x + 14.
        for (key, _, _) in HELP {
            assert!(key.chars().count() < 13, "key {key:?} would reach the description");
        }
    }
}
