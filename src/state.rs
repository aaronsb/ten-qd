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
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Cd => "CD",
            SourceKind::Tape => "TAPE",
            SourceKind::Tuner => "TUNER",
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

    // control head
    VolUp,
    VolDown,
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
    /// Instrument dimmer, the way the dash rheostat works: one step per press.
    DimUp,
    DimDown,

    Quit,
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
}

impl Default for TapeState {
    fn default() -> Self {
        TapeState {
            transport: Transport::Stop,
            side: Side::A,
            index: 0,
            counter: 0.0,
            dolby: true,
            auto_reverse: true,
            tape: None,
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
            bass: 0,
            treble: 0,
            fader: 0.5,
            ill: crate::ui::theme::Ill::Orange,
            dimmer: crate::ui::theme::DIM_DEFAULT,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stack {
    pub source: SourceKind,
    pub cd: CdState,
    pub tape: TapeState,
    pub tuner: TunerState,
    pub eq: EqState,
    pub amp: AmpState,
    pub ctrl: CtrlState,
    /// Last line of the colophon — what the engine last reported. Errors land
    /// here rather than being swallowed.
    pub status: String,
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
    pub tape: Option<Option<Tape>>,

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
    pub bass: Option<i8>,
    pub treble: Option<i8>,
    pub fader: Option<f32>,
    pub ill: Option<crate::ui::theme::Ill>,
    pub dimmer: Option<u8>,

    pub status: Option<String>,
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
        if let Some(v) = p.tape {
            self.tape.tape = v;
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
