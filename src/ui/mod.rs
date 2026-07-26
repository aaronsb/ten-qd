//! Layer 3 · UNITS, and the chassis primitives they are built from.
//!
//! The rule carried over from the prototype: no unit reaches into another
//! unit. Each `units::*` module draws inside the `Rect` it is handed and reads
//! only its own slice of state plus the theme. Anything shared lives here.
//!
//! The rack is drawn into an off-screen buffer at its natural full height and
//! then blitted through the viewport, so a short terminal scrolls rather than
//! squashing the panels. A 1985 component stack does not reflow.

pub mod chassis;
pub mod hit;
pub mod overlay;
pub mod glyph;
pub mod theme;
pub mod units;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;

use crate::state::Stack;
use theme::Theme;

/// Natural height of each bay, in rows. These are fixed: the panels have the
/// proportions they have.
pub const H_CD: u16 = 12;
pub const H_TAPE: u16 = 12;
pub const H_TUNER: u16 = 12;
pub const H_EQ: u16 = 15;
pub const H_AMP: u16 = 10;
pub const H_CTRL: u16 = 11;
pub const H_COLOPHON: u16 = 2;

pub const RACK_HEIGHT: u16 =
    H_CD + H_TAPE + H_TUNER + H_EQ + H_AMP + H_CTRL + H_COLOPHON;
/// Below this the panels start losing their legends; we say so rather than
/// rendering something misleading.
pub const MIN_WIDTH: u16 = 84;

/// Draw the whole stack. `scroll` is the first rack row shown in the viewport.
///
/// `hits` is refilled every frame from the units themselves, so click targets
/// can never describe a layout other than the one just drawn.
pub fn draw(f: &mut Frame, stack: &Stack, scroll: u16, hits: &mut hit::HitMap) {
    hits.clear();
    let view = f.area();
    let theme = Theme::new(stack.ctrl.ill);

    if view.width < MIN_WIDTH {
        let msg = format!(
            "terminal is {} columns · the rack needs {MIN_WIDTH}",
            view.width
        );
        f.buffer_mut().set_string(
            0,
            view.height / 2,
            &msg,
            Style::default().fg(theme.ink_red),
        );
        return;
    }

    // Draw at full height off-screen, then window into it.
    let full = Rect::new(0, 0, view.width, RACK_HEIGHT);
    let mut rack = Buffer::empty(full);
    rack.set_style(full, Style::default().bg(theme.chassis_deep));

    let mut y = 0;
    let row = |y: &mut u16, h: u16| {
        let r = Rect::new(0, *y, full.width, h);
        *y += h;
        r
    };

    // Sources first, then the processing chain, then the control head — which
    // is also the order signal travels through the rack.
    units::cd::draw(&mut rack, row(&mut y, H_CD), stack, &theme, hits);
    units::tape::draw(&mut rack, row(&mut y, H_TAPE), stack, &theme, hits);
    units::tuner::draw(&mut rack, row(&mut y, H_TUNER), stack, &theme, hits);
    units::eq::draw(&mut rack, row(&mut y, H_EQ), stack, &theme, hits);
    units::amp::draw(&mut rack, row(&mut y, H_AMP), stack, &theme, hits);
    units::ctrl::draw(&mut rack, row(&mut y, H_CTRL), stack, &theme, hits);
    colophon(&mut rack, row(&mut y, H_COLOPHON), stack, &theme);

    let scroll = scroll.min(RACK_HEIGHT.saturating_sub(view.height));
    blit(f.buffer_mut(), view, &rack, scroll);
    hits.translate(scroll);
}

/// Copy the visible slice of the rack into the frame.
fn blit(dst: &mut Buffer, view: Rect, src: &Buffer, scroll: u16) {
    let max_scroll = RACK_HEIGHT.saturating_sub(view.height);
    let scroll = scroll.min(max_scroll);

    for vy in 0..view.height {
        let sy = vy + scroll;
        if sy >= RACK_HEIGHT {
            break;
        }
        for vx in 0..view.width.min(src.area.width) {
            if let (Some(s), Some(d)) = (
                src.cell((vx, sy)),
                dst.cell_mut((view.x + vx, view.y + vy)),
            ) {
                *d = s.clone();
            }
        }
    }
}

/// Clamp a scroll offset to what the rack and viewport allow.
pub fn clamp_scroll(scroll: i32, view_height: u16) -> u16 {
    let max = RACK_HEIGHT.saturating_sub(view_height) as i32;
    scroll.clamp(0, max.max(0)) as u16
}

fn colophon(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme) {
    let y = area.y + 1;
    let left = if stack.status.is_empty() {
        "controls emit commands · state arrives from the engine".to_string()
    } else {
        stack.status.clone()
    };
    buf.set_string(2, y, &left, Style::default().fg(theme.ink_grey));

    let right = "? KEYS   Q QUIT";
    let x = area.width.saturating_sub(right.len() as u16 + 2);
    buf.set_string(
        x,
        y,
        right,
        Style::default().fg(theme.ink_grey).add_modifier(Modifier::DIM),
    );
}
