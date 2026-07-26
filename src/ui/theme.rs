//! Layer 1 · TOKENS
//!
//! Ported from the `:root` block of `fujitsu-ten-stack.html`. The rule there
//! was "retheme from :root alone, no unit reaches into another unit" — that
//! rule holds here too. Nothing below this module knows a hex value.
//!
//! The terminal has no alpha channel, so every `rgba(x, .08)` in the CSS
//! becomes an explicit blend against the surface it sat on. `mix()` does that.

use ratatui::style::Color;

/// The ILL key on the control head. On the real unit this swapped the
/// illumination colour of the whole stack; here it does the same thing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ill {
    Green,
    Orange,
}

impl Ill {
    pub fn toggle(self) -> Self {
        match self {
            Ill::Green => Ill::Orange,
            Ill::Orange => Ill::Green,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Ill::Green => "GRN",
            Ill::Orange => "ORG",
        }
    }
}

pub struct Theme {
    // chassis + panel
    pub chassis: Color,
    pub chassis_deep: Color,
    pub seam: Color,
    pub window: Color,

    // the big orange lamp-buttons down the left spine
    pub lamp: Color,
    pub lamp_hot: Color,
    pub lamp_deep: Color,

    // screen-printed panel ink
    pub ink_legend: Color,
    pub ink_red: Color,
    pub ink_grey: Color,
    pub ink_white: Color,

    // emissive: vacuum-fluorescent display and LEDs
    pub vfd: Color,
    pub vfd_dim: Color,
    pub led_r: Color,
    pub led_a: Color,
    pub led_g: Color,
    pub led_off: Color,

    // key caps
    pub cap: Color,
    pub cap_slot: Color,
}

const CHASSIS: (u8, u8, u8) = (0x13, 0x13, 0x17);
const CHASSIS_DEEP: (u8, u8, u8) = (0x09, 0x09, 0x0c);
const PANEL: (u8, u8, u8) = (0x16, 0x17, 0x1c);
const SEAM: (u8, u8, u8) = (0x2b, 0x2d, 0x34);
const WINDOW: (u8, u8, u8) = (0x0a, 0x0b, 0x0e);

const VFD: (u8, u8, u8) = (0xff, 0xc2, 0x1f);
const LED_R: (u8, u8, u8) = (0xff, 0x2e, 0x10);
const LED_A: (u8, u8, u8) = (0xff, 0xab, 0x1a);
const LED_G: (u8, u8, u8) = (0x43, 0xab, 0x5c);

const INK_GREEN: (u8, u8, u8) = (0x43, 0xab, 0x5c);

/// Blend `fg` over `bg` at `a` (0..=1) — the alpha the CSS had, resolved.
fn mix(fg: (u8, u8, u8), bg: (u8, u8, u8), a: f32) -> Color {
    let f = |x: u8, y: u8| (x as f32 * a + y as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(f(fg.0, bg.0), f(fg.1, bg.1), f(fg.2, bg.2))
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

impl Theme {
    pub fn new(ill: Ill) -> Self {
        // ILL drives the *lamp* colour only — the big illuminated buttons down
        // the left spine. Screen-printed legends and the emissive VFD/LED
        // colours do not move, because those are ink and phosphor, not bulbs.
        // (This is what `ctrl.render()` in the HTML does: it rewrites
        // --lamp / --lamp-hot / --lamp-deep and nothing else.)
        let (lamp, lamp_hot, lamp_deep) = match ill {
            Ill::Orange => ((0xe8, 0x43, 0x1a), (0xff, 0x72, 0x38), (0xa3, 0x24, 0x07)),
            Ill::Green => ((0x1f, 0x9c, 0x46), (0x43, 0xd6, 0x70), (0x0c, 0x56, 0x24)),
        };

        Theme {
            chassis: rgb(CHASSIS),
            chassis_deep: rgb(CHASSIS_DEEP),
            seam: rgb(SEAM),
            window: rgb(WINDOW),

            lamp: rgb(lamp),
            lamp_hot: rgb(lamp_hot),
            lamp_deep: rgb(lamp_deep),

            ink_legend: rgb(INK_GREEN),
            ink_red: rgb((0xff, 0x4b, 0x22)),
            ink_grey: rgb((0x7f, 0x83, 0x8c)),
            ink_white: rgb((0xd6, 0xd8, 0xde)),

            vfd: rgb(VFD),
            // `--vfd-dim: rgba(255,194,31,.085)` over the window black. This is
            // the unlit segment — visible as a ghost, which is exactly how a
            // real VFD looks when you can see the un-driven grid.
            vfd_dim: mix(VFD, WINDOW, 0.16),
            led_r: rgb(LED_R),
            led_a: rgb(LED_A),
            led_g: rgb(LED_G),
            led_off: mix((0xff, 0x96, 0x28), PANEL, 0.12),

            cap: rgb((0x1c, 0x1d, 0x23)),
            cap_slot: mix(VFD, (0x1c, 0x1d, 0x23), 0.55),
        }
    }

    /// Interpolate an LED between amber and red by how hot the column is.
    /// The rev-2 note in the HTML was emphatic that these are amber dots with
    /// a red bar riding the peak — not a green-to-red ramp. Kept.
    pub fn led_ramp(&self, t: f32) -> Color {
        if t >= 0.88 { self.led_r } else { self.led_a }
    }

    /// A lamp cap that is lit vs merely present.
    pub fn lamp_face(&self, on: bool) -> Color {
        if on { self.lamp_hot } else { self.lamp_deep }
    }
}
