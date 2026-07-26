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

use crate::state::{Command, Layout, SourceKind, Stack, Unit};
use theme::Theme;

/// Natural height of each bay, in rows. These are fixed: the panels have the
/// proportions they have.
pub const H_CD: u16 = 12;
pub const H_TAPE: u16 = 12;
pub const H_TUNER: u16 = 12;
pub const H_AUX: u16 = 12;
pub const H_EQ: u16 = 15;
pub const H_AMP: u16 = 10;
pub const H_CTRL: u16 = 12;
pub const H_COLOPHON: u16 = 2;

/// The height a unit occupies with its faceplate open.
pub fn natural_height(u: Unit) -> u16 {
    match u {
        Unit::Cd => H_CD,
        Unit::Tape => H_TAPE,
        Unit::Tuner => H_TUNER,
        Unit::Aux => H_AUX,
        Unit::Eq => H_EQ,
        Unit::Amp => H_AMP,
        Unit::Ctrl => H_CTRL,
    }
}

/// What a unit occupies as the rack is currently arranged: nothing when it has
/// been taken out, one row for its bar when folded, its full face otherwise.
pub fn unit_height(u: Unit, layout: &Layout) -> u16 {
    if layout.is_hidden(u) {
        0
    } else if layout.is_collapsed(u) {
        1
    } else {
        natural_height(u)
    }
}

/// The rack's height as arranged. No longer a constant: folding a unit away
/// genuinely makes the stack shorter, which is the point.
pub fn rack_height(layout: &Layout) -> u16 {
    layout.order.iter().map(|u| unit_height(*u, layout)).sum::<u16>() + H_COLOPHON
}
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
    let theme = Theme::for_stack(stack);

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
    let height = rack_height(&stack.layout);
    let full = Rect::new(0, 0, view.width, height);
    let mut rack = Buffer::empty(full);
    rack.set_style(full, Style::default().bg(theme.chassis_deep));

    let mut y = 0;
    let row = |y: &mut u16, h: u16| {
        let r = Rect::new(0, *y, full.width, h);
        *y += h;
        r
    };

    // Factory order is sources, then the processing chain, then the control
    // head — which is also the order signal travels through the rack. The
    // operator may have rearranged it; that is a cabinet, not a signal path.
    for u in stack.layout.order {
        let h = unit_height(u, &stack.layout);
        if h == 0 {
            continue;
        }
        let area = row(&mut y, h);
        if h > 1 {
            match u {
                Unit::Cd => units::cd::draw(&mut rack, area, stack, &theme, hits),
                Unit::Tape => units::tape::draw(&mut rack, area, stack, &theme, hits),
                Unit::Tuner => units::tuner::draw(&mut rack, area, stack, &theme, hits),
                Unit::Aux => units::aux_in::draw(&mut rack, area, stack, &theme, hits),
                Unit::Eq => units::eq::draw(&mut rack, area, stack, &theme, hits),
                Unit::Amp => units::amp::draw(&mut rack, area, stack, &theme, hits),
                Unit::Ctrl => units::ctrl::draw(&mut rack, area, stack, &theme, hits),
            }
        }
        // The bar goes on last either way: for an open bay it replaces the
        // plain top seam the unit just drew, so the fold control sits in the
        // same place whether the face below it is there or not.
        let bar = Rect::new(area.x, area.y, area.width, 1);
        chassis::seam_bar(
            &mut rack,
            bar,
            &theme,
            h == 1,
            is_live(u, stack),
            stack.blink[u.index()] > 0,
            u.plate(),
        );
        hits.add(bar.x + 2, bar.y, 3, 1, Command::UnitCollapse(u));
    }
    colophon(&mut rack, row(&mut y, H_COLOPHON), stack, &theme, hits);

    let scroll = scroll.min(height.saturating_sub(view.height));
    blit(f.buffer_mut(), view, &rack, scroll, height);
    hits.translate(scroll);
}

/// Whether a unit's lamp burns steadily — it is doing something right now.
fn is_live(u: Unit, stack: &Stack) -> bool {
    match u {
        Unit::Cd => stack.source == SourceKind::Cd && stack.cd.transport.is_running(),
        Unit::Tape => stack.source == SourceKind::Tape && stack.tape.transport.is_running(),
        Unit::Tuner => stack.source == SourceKind::Tuner && stack.tuner.power,
        Unit::Aux => stack.aux.state.live,
        Unit::Eq => !stack.eq.defeat,
        Unit::Amp => stack.amp.power,
        Unit::Ctrl => true,
    }
}

/// Copy the visible slice of the rack into the frame.
fn blit(dst: &mut Buffer, view: Rect, src: &Buffer, scroll: u16, height: u16) {
    let max_scroll = height.saturating_sub(view.height);
    let scroll = scroll.min(max_scroll);

    for vy in 0..view.height {
        let sy = vy + scroll;
        if sy >= height {
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
pub fn clamp_scroll(scroll: i32, view_height: u16, layout: &Layout) -> u16 {
    let max = rack_height(layout).saturating_sub(view_height) as i32;
    scroll.clamp(0, max.max(0)) as u16
}

fn colophon(buf: &mut Buffer, area: Rect, stack: &Stack, theme: &Theme, hits: &mut hit::HitMap) {
    let y = area.y + 1;
    let left = if stack.status.is_empty() {
        "controls emit commands · state arrives from the engine".to_string()
    } else {
        stack.status.clone()
    };
    buf.set_string(2, y, &left, Style::default().fg(theme.ink_grey));

    // The dimmer sits with the other whole-panel controls rather than in a
    // bay, because that is what it is — one rheostat wired to every lamp.
    let grey = Style::default().fg(theme.ink_grey).add_modifier(Modifier::DIM);
    let tail = "? KEYS   Q QUIT";
    let bar_w = theme::DIM_MAX as u16 + 1;
    let dim_w = 5 + bar_w;
    let x = area.width.saturating_sub(tail.len() as u16 + dim_w + 5);

    buf.set_string(x, y, "DIM", grey);
    for i in 0..bar_w {
        let lit = i <= theme.dim as u16;
        buf.set_string(
            x + 4 + i,
            y,
            if lit { "▮" } else { "▯" },
            Style::default().fg(if lit { theme.led_a } else { theme.led_off }),
        );
    }
    // Left half steps down, right half steps up — one control, two directions,
    // like the rocker it stands in for.
    hits.add_row(x, y, 4 + bar_w / 2, Command::DimDown);
    hits.add_row(x + 4 + bar_w / 2, y, bar_w / 2 + 1, Command::DimUp);

    buf.set_string(area.width.saturating_sub(tail.len() as u16 + 2), y, tail, grey);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_a_unit_makes_the_rack_shorter() {
        let mut l = Layout::default();
        let full = rack_height(&l);

        l.collapsed[Unit::Tuner.index()] = true;
        let folded = rack_height(&l);
        assert_eq!(full - folded, natural_height(Unit::Tuner) - 1, "a folded unit keeps its bar");

        l.hidden[Unit::Tuner.index()] = true;
        assert_eq!(rack_height(&l), full - natural_height(Unit::Tuner), "gone is gone");
    }

    #[test]
    fn reordering_does_not_change_the_height() {
        let mut l = Layout::default();
        let before = rack_height(&l);
        l.order.swap(0, 5);
        assert_eq!(rack_height(&l), before);
    }

    #[test]
    fn an_empty_rack_is_still_a_rack() {
        // Every unit out. The colophon stays, so there is always something to
        // draw and the scroll clamp never divides by a zero-height rack.
        let l = Layout { hidden: [true; 7], ..Default::default() };
        assert_eq!(rack_height(&l), H_COLOPHON);
        assert_eq!(clamp_scroll(99, 10, &l), 0);
    }
}
