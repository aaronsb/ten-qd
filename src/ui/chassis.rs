//! Chassis primitives — the parts every bay is assembled from.
//!
//! Nothing here knows what a CD player is. These draw metal, glass, ink, lamps
//! and lit segments; the unit modules arrange them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::glyph;
use super::theme::Theme;

/// A component bay: the chassis face, seamed top and bottom. Returns the inner
/// rect the unit may draw into.
pub fn bay(buf: &mut Buffer, area: Rect, theme: &Theme) -> Rect {
    buf.set_style(area, Style::default().bg(theme.chassis));

    let seam = Style::default().fg(theme.seam).bg(theme.chassis);
    let w = area.width as usize;
    if area.height >= 2 {
        buf.set_string(area.x, area.y, "━".repeat(w), seam);
        buf.set_string(area.x, area.y + area.height - 1, "━".repeat(w), seam);
    }

    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

/// The recessed black display window, with a lit edge.
pub fn window(buf: &mut Buffer, area: Rect, theme: &Theme, edged: bool) -> Rect {
    buf.set_style(area, Style::default().bg(theme.window));

    let edge = Style::default()
        .fg(if edged { theme.led_g } else { theme.seam })
        .bg(theme.window);

    if area.width >= 2 && area.height >= 2 {
        let inner_w = area.width as usize - 2;
        buf.set_string(area.x, area.y, format!("╭{}╮", "─".repeat(inner_w)), edge);
        buf.set_string(
            area.x,
            area.y + area.height - 1,
            format!("╰{}╯", "─".repeat(inner_w)),
            edge,
        );
        for y in 1..area.height - 1 {
            buf.set_string(area.x, area.y + y, "│", edge);
            buf.set_string(area.x + area.width - 1, area.y + y, "│", edge);
        }
    }

    Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    }
}

/// Screen-printed panel legend.
pub fn legend(buf: &mut Buffer, x: u16, y: u16, text: &str, theme: &Theme) {
    buf.set_string(x, y, text, Style::default().fg(theme.ink_legend).bg(theme.chassis));
}

/// Small grey sub-legend — the "TRACK", "ELAPSED", "44.1 kHz" annotations.
pub fn sublegend(buf: &mut Buffer, x: u16, y: u16, text: &str, theme: &Theme, on_window: bool) {
    let bg = if on_window { theme.window } else { theme.chassis };
    buf.set_string(
        x,
        y,
        text,
        Style::default().fg(theme.ink_grey).bg(bg).add_modifier(Modifier::DIM),
    );
}

/// Vacuum-fluorescent numerals.
///
/// Every cell is drawn twice: once as a full `8` in the un-driven segment
/// colour, then again with the live value on top. That ghost is what makes a
/// real VFD read as a display rather than as floating numbers — you can always
/// see the segments that are not lit.
pub fn vfd(buf: &mut Buffer, x: u16, y: u16, text: &str, theme: &Theme) {
    // Separators keep their own shape in the ghost — mapping them to `8` would
    // make the un-driven layer wider than the live one and the two would drift
    // apart. A blank stays lit as a ghost `8`, which is what an unused digit
    // position on a real VFD looks like.
    let ghost_src: String = text
        .chars()
        .map(|c| if c == ':' || c == '.' { c } else { '8' })
        .collect();

    let ghost = glyph::seven_seg(&ghost_src);
    let live = glyph::seven_seg(text);

    for (r, row) in ghost.iter().enumerate() {
        buf.set_string(
            x,
            y + r as u16,
            row,
            Style::default().fg(theme.vfd_dim).bg(theme.window),
        );
    }
    for (r, row) in live.iter().enumerate() {
        // Overlay only the lit cells, so the ghost shows through the gaps.
        for (i, ch) in row.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x + i as u16, y + r as u16)) {
                cell.set_char(ch)
                    .set_style(Style::default().fg(theme.vfd).bg(theme.window));
            }
        }
    }
}

/// A boxed indicator legend — APS, M SCAN, REPEAT, DISC and friends.
pub fn boxed(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    label: &str,
    theme: &Theme,
    on: bool,
    on_window: bool,
) {
    let bg = if on_window { theme.window } else { theme.chassis };
    let fg = if on { theme.ink_red } else { theme.ink_grey };
    let style = if on {
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::DIM)
    };
    buf.set_string(x, y, glyph::boxed(label), style);
}

/// Same, but lit green — the indicators that report a mode rather than an alert.
pub fn boxed_green(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    label: &str,
    theme: &Theme,
    on: bool,
    on_window: bool,
) {
    let bg = if on_window { theme.window } else { theme.chassis };
    let style = if on {
        Style::default().fg(theme.led_g).bg(bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.ink_grey).bg(bg).add_modifier(Modifier::DIM)
    };
    buf.set_string(x, y, glyph::boxed(label), style);
}

/// A key: a dark cap with a lit slot, and the green legend printed on the
/// panel beneath it. Three rows tall, `w` wide. Returns the width used.
///
/// **Every operable control on the rack is one of these.** The cap and the
/// legend are drawn in different colours on purpose — on the real deck the
/// legend is on the panel, not on the button. Things that only *report* — DISC,
/// STEREO, REW, the attenuator steps — are `boxed` legends inside a display
/// window instead, and the distinction is the fastest way to read the panel:
/// if it has a cap, you can press it.
/// A key that may be unavailable — an empty radio preset, say. An unavailable
/// key still has a cap, because the button is physically there; its slot and
/// legend simply are not lit.
#[allow(clippy::too_many_arguments)]
pub fn key_with(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    w: u16,
    label: &str,
    theme: &Theme,
    active: bool,
    available: bool,
) -> u16 {
    let w = w.max(4);
    let cap_style = Style::default().fg(theme.cap).bg(theme.chassis);
    let slot_style = Style::default()
        .fg(if active {
            theme.vfd
        } else if available {
            theme.cap_slot
        } else {
            theme.seam
        })
        .bg(theme.cap);

    let inner = (w - 2) as usize;
    buf.set_string(x, y, format!("▗{}▖", "▄".repeat(inner)), cap_style);
    buf.set_string(x, y + 1, "▐", Style::default().fg(theme.cap).bg(theme.chassis));
    buf.set_string(x + 1, y + 1, "▬".repeat(inner), slot_style);
    buf.set_string(
        x + w - 1,
        y + 1,
        "▌",
        Style::default().fg(theme.cap).bg(theme.chassis),
    );

    // Legend, centred under the cap.
    let lw = label.chars().count() as u16;
    let lx = x + w.saturating_sub(lw) / 2;
    let style = if !available {
        Style::default().fg(theme.ink_grey).bg(theme.chassis).add_modifier(Modifier::DIM)
    } else if active {
        Style::default().fg(theme.vfd).bg(theme.chassis).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.ink_legend).bg(theme.chassis)
    };
    buf.set_string(lx, y + 2, label, style);

    w
}

/// Lay out a row of keys, returning the x each one starts at. Keeps the gap
/// between caps identical everywhere on the rack.
pub struct KeyRow {
    pub x: u16,
    pub y: u16,
}

impl KeyRow {
    pub fn new(x: u16, y: u16) -> Self {
        KeyRow { x, y }
    }

    /// Draw one key and advance. Returns the rect it occupies, for the hit map.
    #[allow(clippy::too_many_arguments)]
    pub fn key(
        &mut self,
        buf: &mut Buffer,
        w: u16,
        label: &str,
        theme: &Theme,
        active: bool,
        available: bool,
    ) -> Rect {
        let used = key_with(buf, self.x, self.y, w, label, theme, active, available);
        let r = Rect::new(self.x, self.y, used, 3);
        self.x += used + 1;
        r
    }

    /// Extra space between groups of keys.
    pub fn gap(&mut self, n: u16) {
        self.x += n;
    }
}

/// One of the big illuminated buttons down the left spine.
pub fn lamp(buf: &mut Buffer, area: Rect, label: &str, theme: &Theme, lit: bool) {
    let face = theme.lamp_face(lit);
    let style = Style::default().fg(theme.chassis_deep).bg(face);
    let w = area.width as usize;

    for y in 0..area.height {
        buf.set_string(area.x, area.y + y, " ".repeat(w), style);
    }

    let ly = area.y + area.height / 2;
    let lw = label.chars().count() as u16;
    let lx = area.x + area.width.saturating_sub(lw) / 2;
    buf.set_string(
        lx,
        ly,
        label,
        Style::default()
            .fg(theme.chassis_deep)
            .bg(face)
            .add_modifier(Modifier::BOLD),
    );
}

/// The Fujitsu Ten badge.
pub fn badge(buf: &mut Buffer, x: u16, y: u16, theme: &Theme) -> u16 {
    let mark = Style::default()
        .fg(theme.ink_white)
        .bg(theme.lamp)
        .add_modifier(Modifier::BOLD);
    buf.set_string(x, y, glyph::BADGE_MARK.to_string(), mark);
    buf.set_string(
        x + 2,
        y,
        glyph::BADGE_TEXT,
        Style::default().fg(theme.ink_white).bg(theme.chassis),
    );
    2 + glyph::BADGE_TEXT.len() as u16
}

/// Model numbers, right-aligned into the bottom-right corner of a bay.
///
/// Every unit calls this rather than positioning its own, so the whole rack
/// carries its type plate in the same place — which is what makes a stack of
/// separate components read as one product line.
pub fn model_corner(buf: &mut Buffer, inner: Rect, lines: &[&str], theme: &Theme) {
    let n = lines.len() as u16;
    if inner.height < n {
        return;
    }
    let top = inner.y + inner.height - n;
    for (i, line) in lines.iter().enumerate() {
        let w = line.chars().count() as u16;
        if w > inner.width {
            continue;
        }
        model(buf, inner.x + inner.width - w, top + i as u16, line, theme);
    }
}

/// A model-number legend, set in the small grey face type.
pub fn model(buf: &mut Buffer, x: u16, y: u16, text: &str, theme: &Theme) {
    buf.set_string(
        x,
        y,
        text,
        Style::default().fg(theme.ink_white).bg(theme.chassis).add_modifier(Modifier::DIM),
    );
}

/// A vertical LED column of discrete dots, drawn bottom-up, with a solid bar
/// riding the peak.
///
/// The argument list is long because a meter column genuinely has this many
/// independent inputs; bundling them into a struct would only move the same
/// eight values one level out.
#[allow(clippy::too_many_arguments)]
///
/// `level` and `peak` are 0..=1. Columns are amber; only the bar at the top of
/// the lit run goes red, which is what the ad's meters actually show.
pub fn led_column(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    height: u16,
    level: f32,
    peak: f32,
    theme: &Theme,
    powered: bool,
) {
    let h = height as f32;
    let lit = if powered { (level.clamp(0.0, 1.0) * h).round() as u16 } else { 0 };
    let peak_row = if powered && peak > 0.01 {
        Some(height.saturating_sub(((peak.clamp(0.0, 1.0) * h).ceil() as u16).max(1)))
    } else {
        None
    };

    for row in 0..height {
        // row 0 is the top of the column.
        let from_bottom = height - row;
        let is_lit = from_bottom <= lit;
        let t = from_bottom as f32 / h;

        let (ch, fg) = if Some(row) == peak_row {
            (glyph::PEAK_BAR, theme.led_r)
        } else if is_lit {
            (glyph::DOT_ON, theme.led_ramp(t))
        } else {
            (glyph::DOT_OFF, theme.led_off)
        };

        buf.set_string(
            x,
            y + row,
            ch.to_string(),
            Style::default().fg(fg).bg(theme.chassis),
        );
    }
}

/// A horizontal bar of ramp blocks — used for the volume readout, where a
/// smooth fill reads better than discrete dots.
pub fn ramp_bar(buf: &mut Buffer, x: u16, y: u16, width: u16, level: f32, theme: &Theme) {
    let filled = level.clamp(0.0, 1.0) * width as f32;
    for i in 0..width {
        let ch = glyph::hramp_cell(filled, i as f32);
        let lit = filled > i as f32;
        buf.set_string(
            x + i,
            y,
            ch.to_string(),
            Style::default()
                .fg(if lit { theme.led_a } else { theme.led_off })
                .bg(theme.chassis),
        );
    }
}
