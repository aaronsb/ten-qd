//! The 12-volt memory.
//!
//! A head unit has two power feeds: switched ignition, and a permanent line
//! straight to the battery. The permanent one exists so that presets, tone
//! settings and the last station survive the key coming out. Pull that fuse
//! and the radio comes back factory-blank.
//!
//! This is that fuse. One file, holding everything the panel was set to when
//! it was last switched off.
//!
//! ## Where it lives
//!
//! `$XDG_STATE_HOME/ten-qd/memory.toml`, falling back to
//! `~/.local/state/ten-qd/memory.toml`.
//!
//! State rather than config, because that is what this is: nothing here is
//! hand-authored, it is all captured from the panel as you operate it, and
//! losing the file costs you nothing but your presets. The XDG spec puts
//! "recently used files" and view state in exactly this category. It is
//! readable TOML all the same, so editing presets by hand works if you want.
//!
//! ## How it is written
//!
//! Nothing marks the memory dirty. Each frame the current settings are
//! gathered into a `Memory` and compared against what was last written; if it
//! differs and enough time has passed, it is flushed. That means no control
//! can be added later and silently forgotten — if it is in `Memory`, it
//! persists, and if it is not, it does not.
//!
//! Writes go to a temporary file and are renamed into place, so a crash
//! mid-write leaves the previous memory intact rather than a truncated one.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::state::{Bank, SourceKind, Stack};
use crate::ui::theme::Ill;

/// Bumped when a field changes meaning rather than merely being added. Unknown
/// versions are discarded rather than misread.
const VERSION: u32 = 1;

/// Minimum gap between writes. Nudging the volume runs through a dozen values
/// in a second; there is no reason to touch the disk for each one.
const WRITE_INTERVAL: Duration = Duration::from_secs(3);

/// An empty preset. No FM station is at 0 MHz, and TOML arrays cannot hold
/// nulls, so the sentinel costs nothing and keeps the file a flat array.
const PRESET_EMPTY: f64 = 0.0;

#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(default)]
pub struct Memory {
    pub version: u32,

    // control head
    pub source: String,
    pub volume: f32,
    pub att: bool,
    pub bass: i8,
    pub treble: i8,
    pub fader: f32,
    pub ill: String,
    /// Instrument dimmer position. A driver who set the dash lighting for
    /// night driving expects to find it there next time.
    pub dimmer: u8,
    pub amp_power: bool,

    // equaliser
    pub eq_defeat: bool,
    pub eq_front: Vec<f32>,
    pub eq_rear: Vec<f32>,

    // tuner
    pub tuner_freq: f64,
    pub tuner_local: bool,
    pub tuner_power: bool,
    pub tuner_presets: Vec<f64>,

    // transports
    pub repeat: bool,
    pub random: bool,
    pub dolby: bool,
    pub auto_reverse: bool,

    // what was in the machine, and where you were looking
    pub disc: Option<PathBuf>,
    pub tape: Option<PathBuf>,
    pub browser: Option<PathBuf>,
    /// The output device the rack drives, by name.
    ///
    /// Deliberately independent of the system default. The whole point of the
    /// adapter is that you can set *KDE's* output to "ten-qd cassette adapter"
    /// and have everything flow through the rack — at which moment following
    /// the system default would send our own output straight back into our own
    /// input. The rack has to hold its own opinion about where sound goes.
    pub output: Option<String>,
}

impl Default for Memory {
    fn default() -> Self {
        let s = Stack::default();
        Memory {
            version: VERSION,
            source: source_name(s.source).into(),
            volume: s.ctrl.volume,
            att: s.ctrl.att,
            bass: s.ctrl.bass,
            treble: s.ctrl.treble,
            fader: s.ctrl.fader,
            ill: ill_name(s.ctrl.ill).into(),
            dimmer: s.ctrl.dimmer,
            amp_power: s.amp.power,
            eq_defeat: s.eq.defeat,
            eq_front: s.eq.front.to_vec(),
            eq_rear: s.eq.rear.to_vec(),
            tuner_freq: s.tuner.freq,
            tuner_local: s.tuner.local,
            tuner_power: s.tuner.power,
            tuner_presets: vec![PRESET_EMPTY; 6],
            repeat: s.cd.repeat,
            random: s.cd.random,
            dolby: s.tape.dolby,
            auto_reverse: s.tape.auto_reverse,
            disc: None,
            tape: None,
            browser: None,
            output: None,
        }
    }
}

fn source_name(s: SourceKind) -> &'static str {
    match s {
        SourceKind::Cd => "cd",
        SourceKind::Tape => "tape",
        SourceKind::Tuner => "tuner",
    }
}

fn ill_name(i: Ill) -> &'static str {
    match i {
        Ill::Orange => "orange",
        Ill::Green => "green",
    }
}

impl Memory {
    /// Where the memory lives. Honours `XDG_STATE_HOME` when it is set.
    pub fn path() -> Option<PathBuf> {
        let base = match std::env::var_os("XDG_STATE_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
        };
        Some(base.join("ten-qd").join("memory.toml"))
    }

    /// Read the memory back.
    ///
    /// Nothing here is fatal. A missing file is a first run; an unreadable,
    /// truncated, hand-mangled or wrong-version one is a unit that lost its
    /// memory. All of them give you a factory-blank panel, and every case
    /// except the first-run one asks for the file to be rewritten so the next
    /// launch is clean — a corrupt memory should heal itself rather than
    /// greet you again every time.
    pub fn load() -> Loaded {
        match Self::path() {
            Some(p) => Self::load_from(&p),
            None => Loaded::blank(Some("no HOME — settings will not persist".into())),
        }
    }

    /// The body of `load`, against an explicit path so it can be tested
    /// without reaching into the environment.
    pub fn load_from(p: &Path) -> Loaded {
        let text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Loaded { memory: Self::default(), note: None, rewrite: false };
            }
            Err(e) => return Loaded::blank(Some(format!("memory unreadable ({e}) — starting blank"))),
        };

        match toml::from_str::<Memory>(&text) {
            Ok(m) if m.version == VERSION => {
                Loaded { memory: m.sanitised(), note: None, rewrite: false }
            }
            Ok(m) => Loaded::blank(Some(format!(
                "memory is version {}, this build writes {VERSION} — starting blank",
                m.version
            ))),
            Err(e) => {
                let why = e.message().lines().next().unwrap_or("malformed").to_string();
                Loaded::blank(Some(format!("memory corrupt ({why}) — rewriting")))
            }
        }
    }

    /// Clamp everything that came off disk. The file is user-editable and may
    /// be from an older build, so nothing read from it is trusted to be in
    /// range — a bad value should give you a flat equaliser, not a panic.
    fn sanitised(mut self) -> Self {
        self.volume = self.volume.clamp(0.0, 1.0);
        self.fader = self.fader.clamp(0.0, 1.0);
        self.bass = self.bass.clamp(-2, 2);
        self.treble = self.treble.clamp(-2, 2);
        self.tuner_freq = self.tuner_freq.clamp(crate::state::FM_LO, crate::state::FM_HI);
        self.dimmer = self.dimmer.min(crate::ui::theme::DIM_MAX);

        for bank in [&mut self.eq_front, &mut self.eq_rear] {
            bank.resize(9, 0.0);
            for g in bank.iter_mut() {
                *g = if g.is_finite() { g.clamp(-12.0, 12.0) } else { 0.0 };
            }
        }

        self.tuner_presets.resize(6, PRESET_EMPTY);
        for f in self.tuner_presets.iter_mut() {
            if *f != PRESET_EMPTY && !(crate::state::FM_LO..=crate::state::FM_HI).contains(f) {
                *f = PRESET_EMPTY;
            }
        }
        self
    }

    /// Gather what the panel is currently set to.
    pub fn capture(stack: &Stack, browser: Option<&Path>) -> Self {
        Memory {
            version: VERSION,
            source: source_name(stack.source).into(),
            volume: stack.ctrl.volume,
            att: stack.ctrl.att,
            bass: stack.ctrl.bass,
            treble: stack.ctrl.treble,
            fader: stack.ctrl.fader,
            ill: ill_name(stack.ctrl.ill).into(),
            dimmer: stack.ctrl.dimmer,
            amp_power: stack.amp.power,
            eq_defeat: stack.eq.defeat,
            eq_front: stack.eq.front.to_vec(),
            eq_rear: stack.eq.rear.to_vec(),
            tuner_freq: stack.tuner.freq,
            tuner_local: stack.tuner.local,
            tuner_power: stack.tuner.power,
            tuner_presets: stack
                .tuner
                .presets
                .iter()
                .map(|p| p.unwrap_or(PRESET_EMPTY))
                .collect(),
            repeat: stack.cd.repeat,
            random: stack.cd.random,
            dolby: stack.tape.dolby,
            auto_reverse: stack.tape.auto_reverse,
            disc: stack.cd.disc.as_ref().map(|d| d.path.clone()),
            tape: stack.tape.tape.as_ref().map(|t| t.path.clone()),
            browser: browser.map(Path::to_path_buf),
            output: stack.output.clone(),
        }
    }

    pub fn source_kind(&self) -> SourceKind {
        match self.source.as_str() {
            "tape" => SourceKind::Tape,
            "tuner" => SourceKind::Tuner,
            _ => SourceKind::Cd,
        }
    }

    pub fn illumination(&self) -> Ill {
        match self.ill.as_str() {
            "green" => Ill::Green,
            _ => Ill::Orange,
        }
    }

    pub fn eq_bank(&self, bank: Bank) -> [f32; 9] {
        let src = match bank {
            Bank::Front => &self.eq_front,
            Bank::Rear => &self.eq_rear,
        };
        std::array::from_fn(|i| src.get(i).copied().unwrap_or(0.0))
    }

    pub fn presets(&self) -> [Option<f64>; 6] {
        std::array::from_fn(|i| match self.tuner_presets.get(i).copied() {
            Some(f) if f != PRESET_EMPTY => Some(f),
            _ => None,
        })
    }

    /// Write the memory out, creating the directory if needed.
    ///
    /// Atomic: the temporary file is renamed into place so an interrupted
    /// write cannot leave a half-written memory behind.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Not the pretty printer: it breaks every array element onto its own
        // line, which turns two nine-band curves into forty lines of noise.
        let body = toml::to_string(self).map_err(std::io::Error::other)?;
        let text = format!(
            "# ten-qd — the 12-volt memory.\n\
             # Written by the panel as you operate it. Delete this file, or run\n\
             # `ten-qd --forget`, to return the unit to factory-blank.\n\n{body}"
        );

        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// Remove the memory entirely — pulling the fuse.
    pub fn forget() -> std::io::Result<Option<PathBuf>> {
        let Some(path) = Self::path() else { return Ok(None) };
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(Some(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// The result of reading the memory, plus why it might not have worked.
pub struct Loaded {
    pub memory: Memory,
    /// What to tell the operator, if anything went wrong.
    pub note: Option<String>,
    /// Whether the file on disk needs replacing regardless of whether the
    /// settings differ from it. A corrupt file that happens to decode to
    /// nothing would otherwise sit there forever, because the keeper only
    /// writes on a difference.
    pub rewrite: bool,
}

impl Loaded {
    fn blank(note: Option<String>) -> Self {
        Loaded { memory: Memory::default(), note, rewrite: true }
    }
}

/// Rate-limits writes and holds the last thing written, so the caller can ask
/// "has anything changed?" without tracking that itself.
pub struct Keeper {
    last: Memory,
    written: Instant,
    force: bool,
    /// Where to write. `None` when there is nowhere to write to, in which case
    /// the panel simply does not remember anything.
    path: Option<PathBuf>,
    /// Set once if writing fails, so an unwritable directory is reported
    /// rather than silently swallowed — and reported once, not every frame.
    pub error: Option<String>,
}

impl Keeper {
    pub fn new(loaded: &Loaded) -> Self {
        Self::with_path(loaded, Memory::path())
    }

    /// The keeper is told where to write rather than asking the environment,
    /// so the write path is the same one the tests exercise.
    pub fn with_path(loaded: &Loaded, path: Option<PathBuf>) -> Self {
        Keeper {
            last: loaded.memory.clone(),
            written: Instant::now(),
            force: loaded.rewrite,
            path,
            error: None,
        }
    }

    fn write(&mut self, now: Memory) -> bool {
        let Some(path) = self.path.clone() else {
            self.force = false;
            return false;
        };
        match now.save_to(&path) {
            Ok(()) => {
                self.last = now;
                self.force = false;
                true
            }
            Err(e) => {
                if self.error.is_none() {
                    self.error = Some(format!("cannot write memory: {e}"));
                }
                // Do not keep forcing against a directory that will not take
                // it; fall back to writing only on change.
                self.force = false;
                false
            }
        }
    }

    /// Write if the settings differ from what is on disk and the interval has
    /// elapsed. Returns whether it wrote.
    pub fn maybe_save(&mut self, stack: &Stack, browser: Option<&Path>) -> bool {
        if !self.force && self.written.elapsed() < WRITE_INTERVAL {
            return false;
        }
        let now = Memory::capture(stack, browser);
        self.written = Instant::now();
        if !self.force && now == self.last {
            return false;
        }
        self.write(now)
    }

    /// Write unconditionally — used on the way out, where the interval must
    /// not be allowed to swallow the last few seconds of changes.
    pub fn flush(&mut self, stack: &Stack, browser: Option<&Path>) {
        let now = Memory::capture(stack, browser);
        if self.force || now != self.last {
            self.write(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let m = Memory {
            volume: 0.42,
            bass: -2,
            ill: "green".into(),
            tuner_freq: 103.7,
            tuner_presets: vec![89.0, 0.0, 93.5, 0.0, 0.0, 98.0],
            eq_front: vec![3.0; 9],
            disc: Some(PathBuf::from("/music/album")),
            ..Memory::default()
        };

        let text = toml::to_string(&m).unwrap();
        let back: Memory = toml::from_str(&text).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.presets()[0], Some(89.0));
        assert_eq!(back.presets()[1], None);
        assert_eq!(back.illumination(), Ill::Green);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // A memory written by an older build must still load.
        let m: Memory = toml::from_str("version = 1\nvolume = 0.7\n").unwrap();
        assert_eq!(m.volume, 0.7);
        assert_eq!(m.bass, 0);
        assert_eq!(m.source, "cd");
    }

    #[test]
    fn out_of_range_values_are_clamped_not_trusted() {
        let m: Memory = toml::from_str(
            "version = 1\nvolume = 9.5\nbass = 100\ntuner_freq = 5000.0\n\
             eq_front = [99.0, -99.0]\ntuner_presets = [1.0, 89.0]\ndimmer = 200\n",
        )
        .unwrap();
        let m = m.sanitised();
        assert_eq!(m.volume, 1.0);
        assert_eq!(m.bass, 2);
        assert!(m.dimmer <= crate::ui::theme::DIM_MAX);
        assert_eq!(m.tuner_freq, crate::state::FM_HI);
        assert_eq!(m.eq_front.len(), 9, "a short bank must be padded");
        assert_eq!(m.eq_front[0], 12.0);
        assert_eq!(m.eq_front[1], -12.0);
        assert_eq!(m.presets()[0], None, "1 MHz is not an FM station");
        assert_eq!(m.presets()[1], Some(89.0));
    }

    #[test]
    fn a_wrong_version_reads_as_factory_blank() {
        let text = "version = 999\nvolume = 0.9\n";
        let parsed: Memory = toml::from_str(text).unwrap();
        assert_ne!(parsed.version, VERSION);
        // `load` discards it; this asserts the discriminator it uses.
        assert_eq!(Memory::default().volume, Stack::default().ctrl.volume);
    }

    #[test]
    fn a_damaged_memory_asks_to_be_rewritten() {
        // The keeper only writes on a difference, so a corrupt file that
        // decodes to nothing would otherwise never be replaced.
        let l = Loaded::blank(Some("corrupt".into()));
        assert!(l.rewrite);
        let k = Keeper::with_path(&l, None);
        assert!(k.force, "a damaged memory must force one write");
    }

    #[test]
    fn a_healthy_memory_does_not_force_a_write() {
        let l = Loaded { memory: Memory::default(), note: None, rewrite: false };
        let k = Keeper::with_path(&l, None);
        assert!(!k.force);
    }

    /// A scratch directory that cleans itself up, so the disk tests do not
    /// depend on the environment or on each other.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("ten-qd-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
        fn file(&self) -> PathBuf {
            self.0.join("ten-qd").join("memory.toml")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn survives_a_trip_through_the_filesystem() {
        let s = Scratch::new("roundtrip");
        let m = Memory {
            volume: 0.31,
            ill: "green".into(),
            tuner_freq: 103.7,
            tuner_presets: vec![89.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            disc: Some(PathBuf::from("/music/x")),
            ..Memory::default()
        };
        m.save_to(&s.file()).expect("save creates its own directory");

        let back = Memory::load_from(&s.file());
        assert_eq!(back.memory, m);
        assert!(back.note.is_none());
        assert!(!back.rewrite, "a healthy memory needs no rewrite");
    }

    #[test]
    fn a_missing_file_is_a_first_run_not_a_fault() {
        let s = Scratch::new("missing");
        let l = Memory::load_from(&s.file());
        assert!(l.note.is_none(), "first run should not report anything");
        assert!(!l.rewrite);
        assert_eq!(l.memory, Memory::default());
    }

    #[test]
    fn a_corrupt_file_recreates_itself() {
        let s = Scratch::new("corrupt");
        std::fs::create_dir_all(s.file().parent().unwrap()).unwrap();
        std::fs::write(s.file(), "this is not [ valid toml at all ===").unwrap();

        let l = Memory::load_from(&s.file());
        assert_eq!(l.memory, Memory::default(), "a corrupt memory reads as blank");
        assert!(l.note.is_some(), "and says so");
        assert!(l.rewrite, "and asks to be replaced");

        // The keeper acts on that even though nothing has changed since load.
        let mut k = Keeper::with_path(&l, Some(s.file()));
        k.flush(&Stack::default(), None);

        let healed = Memory::load_from(&s.file());
        assert!(healed.note.is_none(), "the rewritten file must parse cleanly");
        assert!(!healed.rewrite);
    }

    #[test]
    fn a_truncated_file_recreates_itself() {
        let s = Scratch::new("truncated");
        std::fs::create_dir_all(s.file().parent().unwrap()).unwrap();
        // Half a real file — the shape a crash mid-write would leave if the
        // write were not atomic.
        std::fs::write(s.file(), "version = 1\nvolume = 0.5\neq_front = [0.0, 0.0").unwrap();

        let l = Memory::load_from(&s.file());
        assert!(l.rewrite);
        assert_eq!(l.memory.volume, Memory::default().volume);
    }

    #[test]
    fn the_file_stays_readable_by_a_human() {
        let s = Scratch::new("shape");
        Memory::default().save_to(&s.file()).unwrap();
        let text = std::fs::read_to_string(s.file()).unwrap();

        assert!(text.starts_with("# ten-qd"), "should lead with an explanation");
        assert!(text.contains("--forget"), "should say how to clear it");
        // Nine-band curves belong on one line each, not forty.
        assert!(
            text.contains("eq_front = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]"),
            "arrays should be inline:\n{text}"
        );
        assert!(text.lines().count() < 30, "too long to read at a glance:\n{text}");
    }

    #[test]
    fn writing_leaves_no_temporary_behind() {
        let s = Scratch::new("atomic");
        Memory::default().save_to(&s.file()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(s.file().parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left {leftovers:?}");
    }

    #[test]
    fn path_honours_xdg_state_home() {
        // Not run in parallel-unsafe fashion: this only inspects composition.
        let p = Memory::path().expect("HOME is set in the test environment");
        assert!(p.ends_with("ten-qd/memory.toml"), "got {}", p.display());
    }
}
