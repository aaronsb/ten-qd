//! Unit state and the command bus.
//!
//! This is the typed form of the contract the HTML prototype established:
//!
//! ```text
//!   Command   — a control was operated. Travels UI -> engine. Never mutates state.
//!   Patch     — the backend reports truth. Travels engine -> UI. The only mutator.
//! ```
//!
//! Press a key and nothing on the panel moves until the audio engine says so.
//! That is deliberate: the display is a readout, not a wish. A `PLAY` press
//! that fails to open a file leaves the transport reading STOP, which is what
//! the real unit would do.

use std::path::PathBuf;

pub const BAND_LABELS: [&str; 9] = ["60", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
pub const BAND_HZ: [f32; 9] = [60.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];

/// The two independently-curved output buses. The rev-2 note on the HTML calls
/// this out as the biggest correction to the design: the QE-581 is two banks of
/// nine, not one bank of nine, and they map onto two buses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bank {
    Front,
    Rear,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transport {
    Stop,
    Play,
    Pause,
    /// Tape only. The CD never enters these.
    Rew,
    Ff,
}

impl Transport {
    pub fn is_running(self) -> bool {
        !matches!(self, Transport::Stop)
    }
}

/// Which component is feeding the amplifier. Exactly one at a time — this is a
/// head unit, not a mixer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SourceKind {
    #[default]
    Cd,
    Tape,
    Tuner,
    /// Whatever is coming in over the cable. A source in its own right, not a
    /// tape that is pretending — which is what it was, and what made the
    /// deck's counter, sides and reels all mean nothing at once.
    Aux,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Cd => "CD",
            SourceKind::Tape => "TAPE",
            SourceKind::Tuner => "TUNER",
            SourceKind::Aux => "AUX",
        }
    }
}

/// Which side of the cassette is facing the heads.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Side {
    #[default]
    A,
    B,
}

impl Side {
    pub fn flip(self) -> Self {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Side::A => "A",
            Side::B => "B",
        }
    }
}

// ---------------------------------------------------------------------------
// Commands — UI to engine
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Command {
    // QD-585 compact disc
    CdPlayPause,
    CdStop,
    CdPrev,
    CdNext,
    CdTrack(usize),
    CdEject,
    CdRepeat,
    CdRandom,

    // QD-581 cassette deck. A tape is a playlist; the two sides are the two
    // halves of it, split where the running time crosses the midpoint.
    TapePlayPause,
    TapeStop,
    /// Automatic Program Search — the deck's name for track skip.
    TapeApsNext,
    TapeApsPrev,
    TapeRew,
    TapeFf,
    TapeFlip,
    TapeEject,
    TapeDolby,
    TapeAutoReverse,
    /// TRACK mode. Not a transport: it starts and stops the listening log,
    /// which runs independently of what the deck itself is playing.
    TapeRecord,

    // QA-581 auxiliary input — the cable, and whatever is on the end of it
    /// Open the picker listing what is playing and could be plugged in.
    AuxOpen,
    AuxClose,
    AuxUp,
    AuxDown,
    /// Plug the n-th listed stream in.
    AuxSelect(usize),
    /// Transport, sent to the plugged-in player over MPRIS.
    AuxPlayPause,
    AuxStop,
    AuxNext,
    AuxPrev,

    // LT-581 tuner
    TunerBand,
    TunerSeekUp,
    TunerSeekDown,
    TunerStepUp,
    TunerStepDown,
    TunerPreset(usize),
    TunerStorePreset(usize),
    TunerLocal,
    TunerPower,

    // source selection
    Source(SourceKind),

    // browser overlay
    BrowserOpen,
    BrowserClose,
    BrowserUp,
    BrowserDown,
    BrowserEnter,
    BrowserParent,
    BrowserSelect(usize),
    /// Load the highlighted directory into the tray or the deck.
    BrowserLoadDisc,
    BrowserLoadTape,

    // QE-581 graphic equaliser
    EqBand { bank: Bank, band: usize, db: f32 },
    /// Move the band cursor. Pure UI state — never reaches the engine.
    EqSelect { bank: Bank, band: usize },
    EqDefeat,
    EqFlat,

    // QM-571 power amplifier
    AmpPower,

    /// Take a unit out of the signal path, or put it back. Not the same as
    /// hiding it: a hidden unit still plays, a powered-down one does not.
    UnitPower(Unit),

    // control head
    VolUp,
    VolDown,
    /// The equaliser's output trim — makeup for a quiet source, or somewhere
    /// to put the level back after boosting nine bands.
    GainUp,
    GainDown,
    /// Absolute volume, 0..=1. The keyboard nudges; the mouse points at a spot
    /// on the bar and means it.
    Volume(f32),
    Att,
    BassUp,
    BassDown,
    Bass(i8),
    TrebleUp,
    TrebleDown,
    Treble(i8),
    Fader(f32),
    Ill,
    /// Open the output-device picker.
    OutputsOpen,
    OutputsClose,
    OutputsUp,
    OutputsDown,
    /// Choose the n-th listed device.
    OutputsSelect(usize),
    /// Instrument dimmer, the way the dash rheostat works: one step per press.
    DimUp,
    DimDown,

    /// Fold a unit's faceplate down to its bar, or open it again.
    UnitCollapse(Unit),

    // Arranging the rack. None of this reaches the engine — a unit that is
    // hidden or folded shut still plays.
    LayoutOpen,
    LayoutClose,
    LayoutUp,
    LayoutDown,
    /// Take hold of the highlighted unit so the arrows carry it, or let go.
    LayoutGrab,
    /// Take the highlighted unit out of the rack, or put it back.
    LayoutToggle,
    /// Back to the factory arrangement: everything in, in signal order.
    LayoutReset,

    Quit,
}

impl Command {
    /// Which unit this command operates, if any.
    ///
    /// Drives the activity lamp on each unit's bar. It has to be exhaustive
    /// rather than a best guess: the whole point of the lamp is that a unit
    /// folded shut — or taken out of the rack entirely — still shows that it
    /// heard you.
    pub fn unit(&self) -> Option<Unit> {
        use Command::*;
        Some(match self {
            CdPlayPause | CdStop | CdPrev | CdNext | CdTrack(_) | CdEject | CdRepeat
            | CdRandom | BrowserLoadDisc => Unit::Cd,

            TapePlayPause | TapeStop | TapeApsNext | TapeApsPrev | TapeRew | TapeFf
            | TapeFlip | TapeEject | TapeDolby | TapeAutoReverse | TapeRecord
            | BrowserLoadTape => Unit::Tape,

            TunerBand | TunerSeekUp | TunerSeekDown | TunerStepUp | TunerStepDown
            | TunerPreset(_) | TunerStorePreset(_) | TunerLocal | TunerPower => Unit::Tuner,

            Source(kind) => match kind {
                SourceKind::Cd => Unit::Cd,
                SourceKind::Tape => Unit::Tape,
                SourceKind::Tuner => Unit::Tuner,
                SourceKind::Aux => Unit::Aux,
            },

            AuxOpen | AuxClose | AuxUp | AuxDown | AuxSelect(_) | AuxPlayPause | AuxStop
            | AuxNext | AuxPrev => Unit::Aux,

            EqBand { .. } | EqSelect { .. } | EqDefeat | EqFlat => Unit::Eq,

            AmpPower => Unit::Amp,
            UnitPower(u) => *u,

            VolUp | VolDown | GainUp | GainDown | Volume(_) | Att | BassUp | BassDown
            | Bass(_) | TrebleUp | TrebleDown | Treble(_) | Fader(_) | Ill | DimUp
            | DimDown | OutputsOpen | OutputsClose | OutputsUp | OutputsDown
            | OutputsSelect(_) => Unit::Ctrl,

            // Opening a browser, arranging the rack, or quitting is not an
            // operation on any one box.
            BrowserOpen | BrowserClose | BrowserUp | BrowserDown | BrowserEnter
            | BrowserParent | BrowserSelect(_) | Quit => return None,

            UnitCollapse(u) => *u,
            LayoutOpen | LayoutClose | LayoutUp | LayoutDown | LayoutGrab | LayoutToggle
            | LayoutReset => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Unit state
// ---------------------------------------------------------------------------

/// One track on the loaded disc. A 1985 player had no text display at all —
/// title and artist exist here for the shelf strip below the rack, not for the
/// panel window, which shows only track number and elapsed time.
#[derive(Clone, Debug)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    /// Duration in seconds, read from the container when the disc is loaded.
    pub seconds: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Disc {
    pub title: String,
    pub tracks: Vec<Track>,
    /// The folder this was loaded from, so the memory can put it back in the
    /// tray next time.
    pub path: PathBuf,
}

impl Disc {
    pub fn total_seconds(&self) -> f64 {
        self.tracks.iter().map(|t| t.seconds).sum()
    }
}

#[derive(Clone, Debug)]
pub struct CdState {
    /// Out of the signal path when false. Distinct from the arranger's
    /// hidden/shown, which is only about what you are looking at.
    pub power: bool,
    pub transport: Transport,
    /// 1-based, as printed on the display. 0 means no track cued.
    pub track: usize,
    pub elapsed: f64,
    pub repeat: bool,
    pub random: bool,
    pub disc: Option<Disc>,
    pub sample_rate: u32,
}

impl Default for CdState {
    fn default() -> Self {
        CdState {
            power: true,
            transport: Transport::Stop,
            track: 0,
            elapsed: 0.0,
            repeat: false,
            random: false,
            disc: None,
            sample_rate: 44_100,
        }
    }
}

impl CdState {
    pub fn current(&self) -> Option<&Track> {
        let d = self.disc.as_ref()?;
        d.tracks.get(self.track.checked_sub(1)?)
    }
}

/// A tape is a playlist with a seam in the middle.
///
/// Real cassettes hold two sides of roughly equal length, so `split` is chosen
/// where the cumulative running time first passes the halfway mark rather than
/// at the midpoint of the track count — the same thing anyone compiling a tape
/// used to do by hand.
#[derive(Clone, Debug, Default)]
pub struct Tape {
    pub title: String,
    pub tracks: Vec<Track>,
    /// Index of the first track on side B.
    pub split: usize,
    /// The folder this was compiled from.
    pub path: PathBuf,
}

impl Tape {
    pub fn from_tracks(title: String, path: PathBuf, tracks: Vec<Track>) -> Self {
        let total: f64 = tracks.iter().map(|t| t.seconds).sum();
        let mut run = 0.0;
        let mut split = tracks.len();
        for (i, t) in tracks.iter().enumerate() {
            if run >= total / 2.0 {
                split = i;
                break;
            }
            run += t.seconds;
        }
        // A one-track tape is all side A; never produce an empty side A.
        let split = split.clamp(1.min(tracks.len()), tracks.len());
        Tape { title, tracks, split, path }
    }

    /// The half-open track range belonging to a side.
    pub fn side_range(&self, side: Side) -> std::ops::Range<usize> {
        match side {
            Side::A => 0..self.split,
            Side::B => self.split..self.tracks.len(),
        }
    }

    pub fn side_seconds(&self, side: Side) -> f64 {
        self.tracks[self.side_range(side)].iter().map(|t| t.seconds).sum()
    }
}

#[derive(Clone, Debug)]
pub struct TapeState {
    pub power: bool,
    pub transport: Transport,
    pub side: Side,
    /// Index into `tape.tracks`, absolute across both sides.
    pub index: usize,
    /// Seconds elapsed on the current side — the deck's linear counter, which
    /// is what a cassette shows instead of a track time.
    pub counter: f64,
    pub dolby: bool,
    pub auto_reverse: bool,
    pub tape: Option<Tape>,
    /// What the deck is writing down.
    pub rec: RecState,
}

/// TRACK mode — the deck keeping a log of what played.
///
/// Every field here is a count of something that has already happened. The
/// panel shows `wrote` and `following`, and both must be exactly what the log
/// would contain if the rack stopped at this instant; a REC lamp lit over a
/// log that is not being appended to would be the display wishing rather than
/// reading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecState {
    /// Whether the log is being appended to.
    pub on: bool,
    /// Entries written this session. Not tracks heard — tracks *committed to
    /// disk*, so a log that cannot be written stops counting.
    pub wrote: u32,
    /// Players currently being followed. Anything counted here has an entry
    /// that will be written when it ends.
    pub following: u8,
    /// An append has failed since REC was last switched on.
    ///
    /// Latched rather than transient, because the status line that reports the
    /// failure scrolls away in seconds and the REC lamp does not. Without this
    /// the lamp burns steadily over a log taking no writes at all — a full
    /// disk, a permission change — which is the display wishing rather than
    /// reading. Cleared by switching REC off and on.
    pub failed: bool,
}

/// What is on the other end of the cable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuxState {
    /// The stream plugged in, if one has been chosen.
    pub source: Option<String>,
    /// The MPRIS player driving it, if one was found.
    pub player: Option<String>,
    /// Metadata the player reports. A cable carries none of this; MPRIS does.
    pub title: String,
    pub artist: String,
    /// Whether audio is actually arriving over the cable.
    pub live: bool,
}

impl AuxState {
    /// Whether the thing on the other end says it is playing. A cable cannot
    /// know this; MPRIS can, which is the one place this beats the real object.
    pub fn playing(&self) -> bool {
        self.player.is_some() && self.live
    }
}

/// The auxiliary input, as the panel shows it.
///
/// Level lives outside [`AuxState`] for the same reason the tuner's RSSI lives
/// outside its presets: it changes every frame, and `AuxState` is compared for
/// equality to decide whether anything actually changed.
#[derive(Clone, Debug)]
pub struct Aux {
    pub power: bool,
    pub state: AuxState,
    /// Peak level arriving on the cable, 0.0–1.0, measured before the
    /// equaliser touches it.
    pub input: f32,
}

impl Default for Aux {
    fn default() -> Self {
        Aux { power: true, state: AuxState::default(), input: 0.0 }
    }
}

impl Default for TapeState {
    fn default() -> Self {
        TapeState {
            power: true,
            transport: Transport::Stop,
            side: Side::A,
            index: 0,
            counter: 0.0,
            dolby: true,
            auto_reverse: true,
            tape: None,
            rec: RecState::default(),
        }
    }
}

impl TapeState {
    pub fn current(&self) -> Option<&Track> {
        self.tape.as_ref()?.tracks.get(self.index)
    }
}

/// FM band limits and the step the TUNE keys move in, in MHz.
pub const FM_LO: f64 = 87.5;
pub const FM_HI: f64 = 108.0;
pub const FM_STEP: f64 = 0.1;

#[derive(Clone, Debug)]
pub struct TunerState {
    /// The tuner's own power switch, separate from the amplifier's. Off means
    /// the display goes dark and the front end stops — a radio that is off,
    /// not merely unselected.
    pub power: bool,
    pub freq: f64,
    /// True once the demodulator has locked a 19 kHz pilot.
    pub stereo: bool,
    /// 0..=1, from the mean IQ magnitude. A real signal-strength reading.
    pub rssi: f32,
    /// Local/DX — raises the seek threshold so scanning skips weak stations.
    pub local: bool,
    pub seeking: bool,
    pub preset: Option<usize>,
    pub presets: [Option<f64>; 6],
    /// What the radio hardware reported about itself, or why it is absent.
    pub device: Option<String>,
}

impl Default for TunerState {
    fn default() -> Self {
        TunerState {
            power: true,
            freq: 88.5,
            stereo: false,
            rssi: 0.0,
            local: false,
            seeking: false,
            preset: None,
            presets: [None; 6],
            device: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EqState {
    pub defeat: bool,
    pub front: [f32; 9],
    pub rear: [f32; 9],
    /// Which cell the keyboard is on, for terminals without a mouse.
    pub cursor: (Bank, usize),
}

impl Default for EqState {
    fn default() -> Self {
        EqState {
            defeat: false,
            front: [0.0; 9],
            rear: [0.0; 9],
            cursor: (Bank::Front, 4),
        }
    }
}

impl EqState {
    pub fn bank(&self, b: Bank) -> &[f32; 9] {
        match b {
            Bank::Front => &self.front,
            Bank::Rear => &self.rear,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AmpState {
    pub power: bool,
    /// Per-column levels, 0..=1. Driven by the real output buffer.
    pub levels: [f32; 9],
    /// Peak-hold per column — the solid red bar riding the top.
    pub peaks: [f32; 9],
}

impl Default for AmpState {
    fn default() -> Self {
        AmpState { power: true, levels: [0.0; 9], peaks: [0.0; 9] }
    }
}

#[derive(Clone, Debug)]
pub struct CtrlState {
    pub volume: f32,
    pub att: bool,
    /// GAIN: the equaliser's output trim in whole dB, ±GAIN_LIMIT_DB. Separate
    /// from volume so the dial keeps meaning what it meant.
    pub gain_db: i8,
    /// Tone steps run -2 ..= +2, as on the panel's five-tick X display.
    pub bass: i8,
    pub treble: i8,
    /// 0.0 = all rear, 0.5 = centred, 1.0 = all front.
    pub fader: f32,
    pub ill: crate::ui::theme::Ill,
    /// Instrument-lighting rheostat, 0..=7. Global to the whole rack, because
    /// on a car it is one knob wired to every lamp on the dash.
    pub dimmer: u8,
}

impl Default for CtrlState {
    fn default() -> Self {
        CtrlState {
            volume: 0.55,
            att: false,
            gain_db: 0,
            bass: 0,
            treble: 0,
            fader: 0.5,
            ill: crate::ui::theme::Ill::Orange,
            dimmer: crate::ui::theme::DIM_DEFAULT,
        }
    }
}

// ---------------------------------------------------------------------------
// The stack itself — which units, in what order, and how tall
// ---------------------------------------------------------------------------

/// A component in the rack.
///
/// The stack is a stack of *separates*, so which boxes are in it, and in what
/// order, is the operator's business rather than the program's. Hiding a unit
/// only takes its faceplate out of the rack; its transport, its keys and its
/// place in the signal path are untouched, exactly as pulling a box out of a
/// hi-fi cabinet would not be an option on a real one — which is the one place
/// this improves on the object it imitates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    Cd,
    Tape,
    Tuner,
    Aux,
    Eq,
    Amp,
    Ctrl,
}

impl Unit {
    /// Every unit, in the order signal travels through the rack. This is the
    /// factory arrangement `r` resets to.
    pub const ALL: [Unit; 7] = [
        Unit::Cd,
        Unit::Tape,
        Unit::Tuner,
        Unit::Aux,
        Unit::Eq,
        Unit::Amp,
        Unit::Ctrl,
    ];

    pub fn index(self) -> usize {
        Unit::ALL.iter().position(|u| *u == self).unwrap_or(0)
    }

    /// The type plate, as printed on the faceplate's bottom-right corner.
    pub fn plate(self) -> &'static str {
        match self {
            Unit::Cd => "COMPACT DISC PLAYER QD-585",
            Unit::Tape => "STEREO CASSETTE DECK QD-581",
            Unit::Tuner => "AM/FM STEREO TUNER LT-581",
            Unit::Aux => "AUXILIARY INPUT QA-581",
            Unit::Eq => "GRAPHIC EQUALIZER QE-581",
            Unit::Amp => "POWER AMPLIFIER QM-571",
            Unit::Ctrl => "CONTROL HEAD LT-581",
        }
    }

    /// What the layout menu calls it — short enough to scan down a column.
    pub fn label(self) -> &'static str {
        match self {
            Unit::Cd => "COMPACT DISC",
            Unit::Tape => "CASSETTE",
            Unit::Tuner => "TUNER",
            Unit::Aux => "AUX INPUT",
            Unit::Eq => "GRAPHIC EQ",
            Unit::Amp => "POWER AMP",
            Unit::Ctrl => "CONTROL HEAD",
        }
    }

    /// The token used in the memory file, so the layout can be edited by hand.
    pub fn token(self) -> &'static str {
        match self {
            Unit::Cd => "cd",
            Unit::Tape => "tape",
            Unit::Tuner => "tuner",
            Unit::Aux => "aux",
            Unit::Eq => "eq",
            Unit::Amp => "amp",
            Unit::Ctrl => "ctrl",
        }
    }

    pub fn from_token(s: &str) -> Option<Unit> {
        Unit::ALL.into_iter().find(|u| u.token() == s)
    }

    /// The key that folds this unit away — the shifted form of whatever key
    /// already operates it, where there is one.
    pub fn collapse_key(self) -> char {
        match self {
            Unit::Cd => 'C',
            Unit::Tape => 'T',
            Unit::Tuner => 'U',
            Unit::Aux => 'X',
            Unit::Eq => 'E',
            Unit::Amp => 'W',
            Unit::Ctrl => 'H',
        }
    }
}

/// Which faceplates are in the rack, in what order, and which are folded shut.
///
/// Order is always a permutation of [`Unit::ALL`]; `hidden` and `collapsed` are
/// indexed by [`Unit::index`]. Nothing here reaches the audio engine — this is
/// entirely about what the operator is looking at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub order: [Unit; 7],
    pub hidden: [bool; 7],
    pub collapsed: [bool; 7],
}

impl Default for Layout {
    fn default() -> Self {
        Layout { order: Unit::ALL, hidden: [false; 7], collapsed: [false; 7] }
    }
}

impl Layout {
    pub fn is_hidden(&self, u: Unit) -> bool {
        self.hidden[u.index()]
    }

    pub fn is_collapsed(&self, u: Unit) -> bool {
        self.collapsed[u.index()]
    }

    /// Move the unit at `from` one place up or down, carrying it with the
    /// cursor. Returns where it ended up.
    pub fn bubble(&mut self, from: usize, down: bool) -> usize {
        let to = if down {
            (from + 1).min(self.order.len() - 1)
        } else {
            from.saturating_sub(1)
        };
        self.order.swap(from, to);
        to
    }

    /// Repair anything a hand-edited memory file got wrong: every unit appears
    /// exactly once, whatever the file said.
    ///
    /// A unit the file never mentions is put back where the factory had it,
    /// not appended. That matters when a *new* unit appears in a later
    /// version: every saved arrangement predates it, and dropping it at the
    /// bottom of everyone's rack would be a worse default than the order it
    /// was designed into. This is how AUX arrived above the equaliser rather
    /// than below the control head.
    pub fn sanitise(&mut self) {
        let mut seen = [false; 7];
        let mut fixed: Vec<Unit> = Vec::with_capacity(7);
        for u in self.order {
            if !seen[u.index()] {
                seen[u.index()] = true;
                fixed.push(u);
            }
        }
        for u in Unit::ALL {
            if seen[u.index()] {
                continue;
            }
            // Ahead of the first unit the factory put after it, so the new
            // arrival lands among its neighbours however the rest was moved.
            let at = fixed.iter().position(|p| p.index() > u.index()).unwrap_or(fixed.len());
            fixed.insert(at, u);
        }
        for (slot, u) in self.order.iter_mut().zip(fixed) {
            *slot = u;
        }
    }
}


#[derive(Clone, Debug, Default)]
pub struct Stack {
    pub source: SourceKind,
    pub cd: CdState,
    pub tape: TapeState,
    pub tuner: TunerState,
    pub aux: Aux,
    pub eq: EqState,
    pub amp: AmpState,
    pub ctrl: CtrlState,
    /// The output device the rack drives, by name. `None` follows the system
    /// default, which is only safe while the default is not our own input.
    pub output: Option<String>,
    /// Every output device offered, for cycling.
    pub outputs: Vec<String>,
    /// Last line of the colophon — what the engine last reported. Errors land
    /// here rather than being swallowed.
    pub status: String,
    /// Which faceplates are in the rack and how they are arranged.
    pub layout: Layout,
    /// Frames of activity lamp left to burn, per unit. Set when a command for
    /// that unit is dispatched and counted down by the render loop, so an
    /// action on a folded-away or hidden unit still shows on its bar.
    pub blink: [u8; 7],
}

// ---------------------------------------------------------------------------
// Patches — engine to UI
// ---------------------------------------------------------------------------

/// The engine's report of truth. Every field is optional; a patch touches only
/// what changed. This is `FT.apply({unit, state})` with the shape checked.
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub transport: Option<Transport>,
    pub track: Option<usize>,
    pub elapsed: Option<f64>,
    pub repeat: Option<bool>,
    pub random: Option<bool>,
    pub disc: Option<Option<Disc>>,
    pub sample_rate: Option<u32>,

    pub source: Option<SourceKind>,

    pub tape_transport: Option<Transport>,
    pub tape_index: Option<usize>,
    pub tape_side: Option<Side>,
    pub tape_counter: Option<f64>,
    pub tape_dolby: Option<bool>,
    pub tape_auto_reverse: Option<bool>,
    pub tape_rec: Option<RecState>,
    pub cd_power: Option<bool>,
    pub tape_power: Option<bool>,
    pub aux_power: Option<bool>,

    pub tape: Option<Option<Tape>>,
    pub aux: Option<AuxState>,
    pub aux_input: Option<f32>,

    pub tuner_freq: Option<f64>,
    pub tuner_stereo: Option<bool>,
    pub tuner_rssi: Option<f32>,
    pub tuner_local: Option<bool>,
    pub tuner_seeking: Option<bool>,
    pub tuner_preset: Option<Option<usize>>,
    pub tuner_presets: Option<[Option<f64>; 6]>,
    pub tuner_device: Option<Option<String>>,
    pub tuner_power: Option<bool>,

    pub eq_defeat: Option<bool>,
    pub eq_front: Option<[f32; 9]>,
    pub eq_rear: Option<[f32; 9]>,

    pub amp_power: Option<bool>,
    pub amp_levels: Option<[f32; 9]>,

    pub volume: Option<f32>,
    pub att: Option<bool>,
    pub gain_db: Option<i8>,
    pub bass: Option<i8>,
    pub treble: Option<i8>,
    pub fader: Option<f32>,
    pub ill: Option<crate::ui::theme::Ill>,
    pub dimmer: Option<u8>,

    pub status: Option<String>,
    pub output: Option<Option<String>>,
    pub outputs: Option<Vec<String>>,
}

impl Stack {
    /// `FT.apply` — the only place unit state is written.
    pub fn apply(&mut self, p: Patch) {
        if let Some(v) = p.transport {
            self.cd.transport = v;
        }
        if let Some(v) = p.track {
            self.cd.track = v;
        }
        if let Some(v) = p.elapsed {
            self.cd.elapsed = v;
        }
        if let Some(v) = p.repeat {
            self.cd.repeat = v;
        }
        if let Some(v) = p.random {
            self.cd.random = v;
        }
        if let Some(v) = p.disc {
            self.cd.disc = v;
        }
        if let Some(v) = p.sample_rate {
            self.cd.sample_rate = v;
        }

        if let Some(v) = p.source {
            self.source = v;
        }

        if let Some(v) = p.tape_transport {
            self.tape.transport = v;
        }
        if let Some(v) = p.tape_index {
            self.tape.index = v;
        }
        if let Some(v) = p.tape_side {
            self.tape.side = v;
        }
        if let Some(v) = p.tape_counter {
            self.tape.counter = v;
        }
        if let Some(v) = p.tape_dolby {
            self.tape.dolby = v;
        }
        if let Some(v) = p.tape_auto_reverse {
            self.tape.auto_reverse = v;
        }
        if let Some(v) = p.tape_rec {
            self.tape.rec = v;
        }
        if let Some(v) = p.cd_power {
            self.cd.power = v;
        }
        if let Some(v) = p.tape_power {
            self.tape.power = v;
        }
        if let Some(v) = p.aux_power {
            self.aux.power = v;
        }
        if let Some(v) = p.tape {
            self.tape.tape = v;
        }
        if let Some(v) = p.aux {
            self.aux.state = v;
        }
        if let Some(v) = p.aux_input {
            self.aux.input = v;
        }

        if let Some(v) = p.tuner_freq {
            self.tuner.freq = v;
        }
        if let Some(v) = p.tuner_stereo {
            self.tuner.stereo = v;
        }
        if let Some(v) = p.tuner_rssi {
            self.tuner.rssi = v;
        }
        if let Some(v) = p.tuner_local {
            self.tuner.local = v;
        }
        if let Some(v) = p.tuner_seeking {
            self.tuner.seeking = v;
        }
        if let Some(v) = p.tuner_preset {
            self.tuner.preset = v;
        }
        if let Some(v) = p.tuner_presets {
            self.tuner.presets = v;
        }
        if let Some(v) = p.tuner_device {
            self.tuner.device = v;
        }
        if let Some(v) = p.tuner_power {
            self.tuner.power = v;
        }

        if let Some(v) = p.eq_defeat {
            self.eq.defeat = v;
        }
        if let Some(v) = p.eq_front {
            self.eq.front = v;
        }
        if let Some(v) = p.eq_rear {
            self.eq.rear = v;
        }

        if let Some(v) = p.amp_power {
            self.amp.power = v;
        }
        if let Some(v) = p.amp_levels {
            // Peak-hold with a slow fall, so the red bar lingers the way a
            // real meter's does rather than snapping back each frame.
            for ((level, peak), &fresh) in
                self.amp.levels.iter_mut().zip(self.amp.peaks.iter_mut()).zip(v.iter())
            {
                *level = fresh;
                *peak = if fresh >= *peak { fresh } else { (*peak - 0.02).max(fresh) };
            }
        }

        if let Some(v) = p.volume {
            self.ctrl.volume = v;
        }
        if let Some(v) = p.gain_db {
            let l = crate::audio::dsp::GAIN_LIMIT_DB;
            self.ctrl.gain_db = v.clamp(-l, l);
        }
        if let Some(v) = p.att {
            self.ctrl.att = v;
        }
        if let Some(v) = p.bass {
            self.ctrl.bass = v;
        }
        if let Some(v) = p.treble {
            self.ctrl.treble = v;
        }
        if let Some(v) = p.fader {
            self.ctrl.fader = v;
        }
        if let Some(v) = p.ill {
            self.ctrl.ill = v;
        }
        if let Some(v) = p.dimmer {
            self.ctrl.dimmer = v.min(crate::ui::theme::DIM_MAX);
        }
        if let Some(v) = p.output {
            self.output = v;
        }
        if let Some(v) = p.outputs {
            self.outputs = v;
        }
        if let Some(v) = p.status {
            self.status = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(sec: f64) -> Track {
        Track {
            path: std::path::PathBuf::from("/x"),
            title: "t".into(),
            artist: "a".into(),
            seconds: sec,
        }
    }

    #[test]
    fn every_unit_has_a_distinct_token_plate_and_fold_key() {
        for a in Unit::ALL {
            for b in Unit::ALL {
                if a == b {
                    continue;
                }
                assert_ne!(a.token(), b.token());
                assert_ne!(a.plate(), b.plate());
                assert_ne!(a.collapse_key(), b.collapse_key(), "{a:?} and {b:?} share a key");
            }
            assert_eq!(Unit::from_token(a.token()), Some(a));
            assert!(a.collapse_key().is_ascii_uppercase(), "fold keys are the shifted forms");
        }
    }

    #[test]
    fn bubbling_carries_a_unit_and_stops_at_the_ends() {
        let mut l = Layout::default();
        assert_eq!(l.order[0], Unit::Cd);

        let at = l.bubble(0, true);
        assert_eq!(at, 1);
        assert_eq!(l.order[0], Unit::Tape);
        assert_eq!(l.order[1], Unit::Cd, "the cursor keeps hold of the unit");

        // The top of the rack is the top of the rack.
        let mut l = Layout::default();
        assert_eq!(l.bubble(0, false), 0);
        assert_eq!(l.order, Unit::ALL, "bubbling off the end must not reorder");
        let last = Unit::ALL.len() - 1;
        assert_eq!(l.bubble(last, true), last);
        assert_eq!(l.order, Unit::ALL);
    }

    #[test]
    fn a_hand_edited_order_is_repaired_rather_than_rejected() {
        // Every way a person can get this wrong in a text file at once:
        // duplicates, and units left out entirely.
        let mut l = Layout {
            order: [
                Unit::Ctrl,
                Unit::Ctrl,
                Unit::Eq,
                Unit::Ctrl,
                Unit::Eq,
                Unit::Amp,
                Unit::Eq,
            ],
            ..Default::default()
        };
        l.sanitise();

        for u in Unit::ALL {
            assert_eq!(l.order.iter().filter(|x| **x == u).count(), 1, "{u:?} appears once");
        }
        // What the file did say is honoured, in the order it said it.
        let said: Vec<Unit> =
            l.order.iter().copied().filter(|u| matches!(u, Unit::Ctrl | Unit::Eq | Unit::Amp)).collect();
        assert_eq!(said, vec![Unit::Ctrl, Unit::Eq, Unit::Amp]);
    }

    /// Every saved arrangement predates any unit added later. Appending it
    /// would put a brand-new bay at the bottom of everyone's rack.
    #[test]
    fn a_unit_the_file_never_heard_of_lands_where_the_factory_put_it() {
        let mut l = Layout::default();
        // A file written before AUX existed: the other six, in factory order.
        let old = [Unit::Cd, Unit::Tape, Unit::Tuner, Unit::Eq, Unit::Amp, Unit::Ctrl];
        for (slot, u) in l.order.iter_mut().zip(old) {
            *slot = u;
        }
        l.order[6] = Unit::Ctrl; // the untouched tail, as `Memory::layout` leaves it
        l.sanitise();
        assert_eq!(l.order, Unit::ALL, "a missing unit belongs in its own place");
    }

    #[test]
    fn every_command_says_which_unit_it_operates() {
        // The activity lamp is only honest if this is exhaustive, and the
        // compiler cannot check that a match arm returns the *right* unit.
        assert_eq!(Command::CdEject.unit(), Some(Unit::Cd));
        assert_eq!(Command::AuxOpen.unit(), Some(Unit::Aux));
        assert_eq!(Command::Source(SourceKind::Aux).unit(), Some(Unit::Aux));
        assert_eq!(Command::TunerLocal.unit(), Some(Unit::Tuner));
        assert_eq!(Command::EqFlat.unit(), Some(Unit::Eq));
        assert_eq!(Command::AmpPower.unit(), Some(Unit::Amp));
        assert_eq!(Command::GainUp.unit(), Some(Unit::Ctrl));
        assert_eq!(Command::Source(SourceKind::Tuner).unit(), Some(Unit::Tuner));
        // Loading from the browser is an operation on the machine you load.
        assert_eq!(Command::BrowserLoadTape.unit(), Some(Unit::Tape));
        // Arranging the cabinet is not an operation on any one box.
        assert_eq!(Command::LayoutOpen.unit(), None);
        assert_eq!(Command::Quit.unit(), None);
    }

    #[test]
    fn tape_splits_by_running_time_not_track_count() {
        // One long opener then three short ones: a real compilation would put
        // the long one alone on side A, and so does this.
        let t = Tape::from_tracks("x".into(), "/x".into(), vec![track(600.0), track(60.0), track(60.0), track(60.0)]);
        assert_eq!(t.split, 1);
        assert_eq!(t.side_range(Side::A).len(), 1);
        assert_eq!(t.side_range(Side::B).len(), 3);
    }

    #[test]
    fn tape_sides_are_balanced_for_even_tracks() {
        let t = Tape::from_tracks("x".into(), "/x".into(), vec![track(100.0); 6]);
        assert_eq!(t.split, 3);
        assert!((t.side_seconds(Side::A) - t.side_seconds(Side::B)).abs() < 1.0);
    }

    #[test]
    fn a_single_track_tape_is_all_side_a() {
        let t = Tape::from_tracks("x".into(), "/x".into(), vec![track(100.0)]);
        assert_eq!(t.side_range(Side::A).len(), 1);
        assert!(t.side_range(Side::B).is_empty());
    }

    #[test]
    fn an_empty_tape_has_empty_sides() {
        let t = Tape::from_tracks("x".into(), "/x".into(), vec![]);
        assert!(t.side_range(Side::A).is_empty());
        assert!(t.side_range(Side::B).is_empty());
    }

    #[test]
    fn patch_only_touches_what_it_names() {
        let mut s = Stack::default();
        s.apply(Patch { volume: Some(0.9), ..Default::default() });
        assert_eq!(s.ctrl.volume, 0.9);
        assert_eq!(s.ctrl.bass, 0, "an unrelated field must not move");
        assert_eq!(s.source, SourceKind::Cd);
    }
}
