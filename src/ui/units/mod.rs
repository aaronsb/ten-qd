//! One module per component. A unit draws inside the `Rect` it is given and
//! reads only its own slice of `Stack` plus the theme — it never reaches into
//! another unit's state or another unit's area.

pub mod amp;
pub mod aux_in;
pub mod cd;
pub mod ctrl;
pub mod eq;
pub mod tape;
pub mod tuner;

/// Width of the left-hand spine bay carrying the illuminated buttons.
pub const SPINE: u16 = 8;
/// Where the nine band columns begin, measured from the bay's inner edge.
///
/// The equaliser and the amplifier meters share this so their columns line up
/// vertically down the rack — that alignment is the whole point of binding the
/// meters to the EQ's band centres, and it is why this is one constant rather
/// than two. Shifting it moves both panels together.
pub const BAND_X: u16 = SPINE + 2;

/// The column immediately left of the band grid, where the equaliser prints its
/// F / R bank markers. Leaving `SPINE`..`MARKER_X` clear gives every bay the
/// same one-column gap between its spine lamp and its contents.
pub const MARKER_X: u16 = SPINE + 1;
/// Cells per band column.
pub const BAND_W: u16 = 5;
/// Column within a band slot that carries the slider track / meter dots.
pub const BAND_TRACK: u16 = 2;
