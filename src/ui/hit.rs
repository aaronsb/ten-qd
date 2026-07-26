//! Click targets.
//!
//! Units register the rectangle each control occupies while they draw, so the
//! hit map is always in step with what is on screen — there is no second
//! layout description to drift out of sync with the first.
//!
//! Zones are recorded in *rack* coordinates and translated into viewport
//! coordinates once, after the rack is drawn, by `translate`. Later
//! registrations win, so a control drawn on top of a larger region (a key cap
//! over its bay) receives the click.

use ratatui::layout::Rect;

use crate::state::Command;

#[derive(Default)]
pub struct HitMap {
    zones: Vec<(Rect, Command)>,
}

impl HitMap {
    pub fn new() -> Self {
        HitMap { zones: Vec::new() }
    }

    pub fn clear(&mut self) {
        self.zones.clear();
    }

    /// Register a click target. Zero-area rectangles are dropped rather than
    /// stored as targets that can never be hit.
    pub fn add(&mut self, x: u16, y: u16, w: u16, h: u16, cmd: Command) {
        if w == 0 || h == 0 {
            return;
        }
        self.zones.push((Rect::new(x, y, w, h), cmd));
    }

    /// Convenience for the common one-row target.
    pub fn add_row(&mut self, x: u16, y: u16, w: u16, cmd: Command) {
        self.add(x, y, w, 1, cmd);
    }

    /// Shift from rack space into viewport space, discarding anything the
    /// scroll offset has moved off the top.
    pub fn translate(&mut self, scroll: u16) {
        self.zones.retain_mut(|(r, _)| match r.y.checked_sub(scroll) {
            Some(y) => {
                r.y = y;
                true
            }
            None => false,
        });
    }

    /// The topmost control under the pointer.
    pub fn hit(&self, x: u16, y: u16) -> Option<&Command> {
        self.zones
            .iter()
            .rev()
            .find(|(r, _)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
            .map(|(_, c)| c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_zones_win() {
        let mut h = HitMap::new();
        h.add(0, 0, 10, 3, Command::CdStop);
        h.add(2, 1, 2, 1, Command::CdPlayPause);
        assert!(matches!(h.hit(2, 1), Some(Command::CdPlayPause)));
        assert!(matches!(h.hit(6, 1), Some(Command::CdStop)));
        assert!(h.hit(20, 1).is_none());
    }

    #[test]
    fn translate_drops_rows_scrolled_off_the_top() {
        let mut h = HitMap::new();
        h.add_row(0, 2, 4, Command::CdStop);
        h.add_row(0, 9, 4, Command::CdNext);
        h.translate(5);
        assert!(h.hit(1, 2).is_none(), "row 2 should have scrolled away");
        assert!(matches!(h.hit(1, 4), Some(Command::CdNext)));
    }

    #[test]
    fn zero_area_zones_are_not_stored() {
        let mut h = HitMap::new();
        h.add(0, 0, 0, 5, Command::CdStop);
        assert!(h.hit(0, 0).is_none());
    }
}
