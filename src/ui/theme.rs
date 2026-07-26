//! Layer 1 · TOKENS
//!
//! Ported from the `:root` block of `fujitsu-ten-stack.html`. The rule there
//! was "retheme from :root alone, no unit reaches into another unit" — that
//! rule holds here too. Nothing below this module knows a hex value.
//!
//! The terminal has no alpha channel, so every `rgba(x, .08)` in the CSS
//! becomes an explicit blend against the surface it sat on. `mix()` does that.
//!
//! ## The dimmer
//!
//! Every car has a rheostat on the instrument lighting, because a dash that is
//! comfortable at noon is blinding at midnight. This has the same control, and
//! because *every* colour in the build is constructed here, it needs no more
//! than a luminance scale applied on the way out — one multiply, applied once,
//! reaching the whole panel. That is the payoff for keeping the token layer
//! honest: a global visual change costs a single function.

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

/// Instrument-lighting level. `0` is the bottom of the rheostat's travel, not
/// off — a dimmer that could extinguish the panel would be a power switch.
pub const DIM_MAX: u8 = 7;
pub const DIM_DEFAULT: u8 = DIM_MAX;

/// Luminance multiplier for a dimmer position.
///
/// The floor is 0.45, not something near zero: a dash rheostat at its minimum
/// is still meant to be read at night. Below roughly this the green ink stops
/// separating from the chassis before the emissive elements have dimmed
/// usefully far.
fn dim_factor(level: u8) -> f32 {
    0.45 + 0.55 * level.min(DIM_MAX) as f32 / DIM_MAX as f32
}

/// Scale a colour's luminance, keeping its hue. Straight per-channel
/// multiplication, which is what turning down a backlight actually does.
fn dimmed(c: (u8, u8, u8), k: f32) -> (u8, u8, u8) {
    let f = |x: u8| (x as f32 * k).round().clamp(0.0, 255.0) as u8;
    (f(c.0), f(c.1), f(c.2))
}

pub struct Theme {
    /// Kept so the colophon can draw the rheostat's position.
    pub dim: u8,

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
fn mix_raw(fg: (u8, u8, u8), bg: (u8, u8, u8), a: f32) -> (u8, u8, u8) {
    let f = |x: u8, y: u8| (x as f32 * a + y as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
    (f(fg.0, bg.0), f(fg.1, bg.1), f(fg.2, bg.2))
}

fn rgb_raw(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

impl Theme {
    /// The theme the panel is currently wearing.
    pub fn for_stack(stack: &crate::state::Stack) -> Self {
        Self::new(stack.ctrl.ill, stack.ctrl.dimmer)
    }

    pub fn new(ill: Ill, dim: u8) -> Self {
        let k = dim_factor(dim);

        // Two constructors, and which one a colour goes through *is* the model
        // of the panel. `surface` is unlit material — chassis plastic, the
        // glass of a display window, a key cap. A rheostat does not change the
        // colour of plastic. `lit` is everything the dimmer is actually wired
        // to: bulbs, phosphor, LEDs, and the legends they backlight.
        //
        // Dimming both was the first attempt and it was wrong twice over —
        // physically, and because scaling the background in step with the
        // foreground holds contrast constant, so nothing appeared to dim at
        // all beyond the whole rack fading toward black.
        let surface = rgb_raw;
        let lit = |c| rgb_raw(dimmed(c, k));
        let lit_mix = |fg, bg, a| rgb_raw(dimmed(mix_raw(fg, bg, a), k));

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
            dim,
            chassis: surface(CHASSIS),
            chassis_deep: surface(CHASSIS_DEEP),
            seam: surface(SEAM),
            window: surface(WINDOW),

            lamp: lit(lamp),
            lamp_hot: lit(lamp_hot),
            lamp_deep: lit(lamp_deep),

            ink_legend: lit(INK_GREEN),
            ink_red: lit((0xff, 0x4b, 0x22)),
            ink_grey: lit((0x7f, 0x83, 0x8c)),
            ink_white: lit((0xd6, 0xd8, 0xde)),

            vfd: lit(VFD),
            // `--vfd-dim: rgba(255,194,31,.085)` over the window black. This is
            // the unlit segment — visible as a ghost, which is exactly how a
            // real VFD looks when you can see the un-driven grid.
            vfd_dim: lit_mix(VFD, WINDOW, 0.16),
            led_r: lit(LED_R),
            led_a: lit(LED_A),
            led_g: lit(LED_G),
            led_off: lit_mix((0xff, 0x96, 0x28), PANEL, 0.12),

            cap: surface((0x1c, 0x1d, 0x23)),
            cap_slot: lit_mix(VFD, (0x1c, 0x1d, 0x23), 0.55),
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
