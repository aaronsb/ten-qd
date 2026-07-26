//! Fujitsu Ten component stack — a TUI head unit that actually plays.
//!
//! The panel is a readout, never a wish. Pressing a key emits a `Command`; the
//! display only changes when the engine reports back through a `Patch`. That
//! is the same one-way contract the HTML prototype set up, and keeping it is
//! what stops the transport from claiming to play a file it could not open.

mod audio;
mod browser;
mod disc;
mod state;
mod ui;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;

use audio::dsp::DspParams;
use audio::radio::RadioCmd;
use audio::{EngineCmd, EngineEvent};
use browser::Browser;
use state::{Bank, Command, Patch, Side, SourceKind, Stack, Transport};
use ui::hit::HitMap;
use ui::units::eq::{snap, RANGE_DB, STEP_DB};

const FRAME: Duration = Duration::from_millis(33);

/// A pending track start, waiting for the output clock to reach it.
struct Mark {
    epoch: u64,
    source: SourceKind,
    index: usize,
    side: Side,
    at_frame: u64,
}

struct App {
    stack: Stack,
    engine: Option<audio::Engine>,
    marks: VecDeque<Mark>,
    current: Option<Mark>,
    /// Bumped whenever a control changes something that requires the filter
    /// coefficients to be recomputed. Continuous controls do not touch it.
    generation: u64,
    epoch: u64,
    scroll: u16,
    show_help: bool,
    running: bool,
    hits: HitMap,
    browser: Browser,
}

impl App {
    fn new(engine: Option<audio::Engine>) -> Self {
        let mut stack = Stack::default();
        if let Some(e) = &engine {
            stack.cd.sample_rate = e.sample_rate;
        }
        App {
            stack,
            engine,
            marks: VecDeque::new(),
            current: None,
            generation: 1,
            epoch: 0,
            scroll: 0,
            show_help: false,
            running: true,
            hits: HitMap::new(),
            browser: Browser::new(music_dir().unwrap_or_else(|| PathBuf::from("/"))),
        }
    }

    fn status(&mut self, msg: impl Into<String>) {
        self.stack.apply(Patch { status: Some(msg.into()), ..Default::default() });
    }

    /// Republish the DSP snapshot. `retune` marks the change as one that needs
    /// new filter coefficients rather than just a gain update.
    fn publish(&mut self, retune: bool) {
        if retune {
            self.generation += 1;
        }
        let Some(engine) = &self.engine else { return };
        let s = &self.stack;
        engine.set_params(DspParams {
            generation: self.generation,
            sample_rate: engine.sample_rate as f32,
            eq_front: s.eq.front,
            eq_rear: s.eq.rear,
            defeat: s.eq.defeat,
            bass: s.ctrl.bass,
            treble: s.ctrl.treble,
            volume: s.ctrl.volume,
            att: s.ctrl.att,
            fader: s.ctrl.fader,
            power: s.amp.power,
        });
    }

    /// Invalidate outstanding track marks — used by anything that flushes.
    fn new_epoch(&mut self) -> u64 {
        self.epoch += 1;
        self.marks.clear();
        self.current = None;
        self.epoch
    }

    fn load(&mut self, path: PathBuf) {
        match disc::load(&path) {
            Ok(d) => {
                let total = d.total_seconds() as u64;
                let msg = format!(
                    "disc: {} · {} tracks · {}:{:02}",
                    d.title,
                    d.tracks.len(),
                    total / 60,
                    total % 60
                );
                let arc = Arc::new(d.clone());
                let epoch = self.new_epoch();
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::Load { disc: arc, start: 0, epoch });
                }
                self.stack.apply(Patch {
                    disc: Some(Some(d)),
                    track: Some(0),
                    elapsed: Some(0.0),
                    transport: Some(Transport::Stop),
                    ..Default::default()
                });
                self.status(msg);
            }
            Err(e) => self.status(format!("no disc: {e}")),
        }
    }

    // -- command dispatch --------------------------------------------------

    fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::Quit => self.running = false,

            Command::CdPlayPause => {
                if self.stack.source != SourceKind::Cd {
                    self.select_source(SourceKind::Cd);
                }
                if self.stack.cd.disc.is_none() {
                    self.status("no disc loaded");
                    return;
                }
                match self.stack.cd.transport {
                    Transport::Play => {
                        if let Some(e) = &self.engine {
                            e.send(EngineCmd::Pause);
                        }
                        self.stack.apply(Patch {
                            transport: Some(Transport::Pause),
                            ..Default::default()
                        });
                    }
                    Transport::Pause => {
                        if let Some(e) = &self.engine {
                            e.send(EngineCmd::Play);
                        }
                        self.stack.apply(Patch {
                            transport: Some(Transport::Play),
                            ..Default::default()
                        });
                    }
                    _ => self.cue(0),
                }
            }

            Command::CdStop => {
                let epoch = self.new_epoch();
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::Stop { epoch });
                }
                self.stack.apply(Patch {
                    transport: Some(Transport::Stop),
                    track: Some(0),
                    elapsed: Some(0.0),
                    ..Default::default()
                });
            }

            Command::CdNext => {
                let n = self.track_count();
                if n == 0 {
                    return;
                }
                let cur = self.stack.cd.track.saturating_sub(1);
                self.cue((cur + 1) % n);
            }

            Command::CdPrev => {
                let n = self.track_count();
                if n == 0 {
                    return;
                }
                // Below three seconds in, PREV goes to the previous track;
                // past that it restarts the current one. Every CD player did
                // this and it is muscle memory.
                let cur = self.stack.cd.track.saturating_sub(1);
                if self.stack.cd.elapsed > 3.0 {
                    self.cue(cur);
                } else {
                    self.cue(if cur == 0 { n - 1 } else { cur - 1 });
                }
            }

            Command::CdTrack(i) => match self.stack.source {
                SourceKind::Tape => self.tape_cue(i),
                _ => {
                    if i < self.track_count() {
                        self.cue(i);
                    }
                }
            },

            Command::CdEject => {
                let epoch = self.new_epoch();
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::Eject { epoch });
                }
                self.stack.apply(Patch {
                    disc: Some(None),
                    transport: Some(Transport::Stop),
                    track: Some(0),
                    elapsed: Some(0.0),
                    ..Default::default()
                });
                self.status("disc ejected");
            }

            Command::CdRepeat => {
                let v = !self.stack.cd.repeat;
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::SetRepeat(v));
                }
                self.stack.apply(Patch { repeat: Some(v), ..Default::default() });
            }

            Command::CdRandom => {
                let v = !self.stack.cd.random;
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::SetRandom(v));
                }
                self.stack.apply(Patch { random: Some(v), ..Default::default() });
            }

            Command::EqBand { bank, band, db } => {
                let db = snap(db.clamp(-RANGE_DB, RANGE_DB));
                let mut front = self.stack.eq.front;
                let mut rear = self.stack.eq.rear;
                match bank {
                    Bank::Front => front[band] = db,
                    Bank::Rear => rear[band] = db,
                }
                self.stack.apply(Patch {
                    eq_front: Some(front),
                    eq_rear: Some(rear),
                    ..Default::default()
                });
                self.publish(true);
            }

            // ---- source ---------------------------------------------------
            Command::Source(kind) => self.select_source(kind),

            // ---- cassette deck --------------------------------------------
            Command::TapePlayPause => {
                if self.stack.tape.tape.is_none() {
                    self.status("no tape loaded");
                    return;
                }
                match self.stack.tape.transport {
                    Transport::Play => {
                        if let Some(e) = &self.engine {
                            e.send(EngineCmd::Pause);
                        }
                        self.stack.apply(Patch {
                            tape_transport: Some(Transport::Pause),
                            ..Default::default()
                        });
                    }
                    Transport::Pause => {
                        if let Some(e) = &self.engine {
                            e.send(EngineCmd::Play);
                        }
                        self.stack.apply(Patch {
                            tape_transport: Some(Transport::Play),
                            ..Default::default()
                        });
                    }
                    _ => {
                        let start = self
                            .stack
                            .tape
                            .tape
                            .as_ref()
                            .map(|t| t.side_range(self.stack.tape.side).start)
                            .unwrap_or(0);
                        self.tape_cue(start);
                    }
                }
            }

            Command::TapeStop => {
                let epoch = self.new_epoch();
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::Stop { epoch });
                }
                self.stack.apply(Patch {
                    tape_transport: Some(Transport::Stop),
                    tape_counter: Some(0.0),
                    ..Default::default()
                });
            }

            Command::TapeApsNext => {
                let i = self.stack.tape.index + 1;
                self.tape_cue(i);
            }

            Command::TapeApsPrev => {
                // Same three-second rule as the CD: past it, APS restarts the
                // track rather than stepping back.
                let cur = self.stack.tape.index;
                let elapsed = self.stack.tape.counter - self.tape_prefix(cur);
                if elapsed > 3.0 {
                    self.tape_cue(cur);
                } else {
                    self.tape_cue(cur.saturating_sub(1));
                }
            }

            Command::TapeRew | Command::TapeFf => {
                // Wind by ten seconds a press. A deck has no index, so this
                // scrubs the tape rather than jumping to a known point.
                let back = matches!(cmd, Command::TapeRew);
                let cur = self.stack.tape.index;
                let within = (self.stack.tape.counter - self.tape_prefix(cur)).max(0.0);
                let target = if back { within - 10.0 } else { within + 10.0 };
                if target < 0.0 {
                    self.tape_cue(cur.saturating_sub(1));
                } else {
                    let epoch = self.new_epoch();
                    if let Some(e) = &self.engine {
                        e.send(EngineCmd::Seek { seconds: target, epoch });
                    }
                }
            }

            Command::TapeFlip => {
                let epoch = self.new_epoch();
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::Flip { epoch });
                }
                let side = self.stack.tape.side.flip();
                let index = self
                    .stack
                    .tape
                    .tape
                    .as_ref()
                    .map(|t| t.side_range(side).start)
                    .unwrap_or(0);
                self.stack.apply(Patch {
                    tape_side: Some(side),
                    tape_index: Some(index),
                    tape_counter: Some(0.0),
                    tape_transport: Some(Transport::Stop),
                    ..Default::default()
                });
                self.status(format!("side {}", side.label()));
            }

            Command::TapeEject => {
                let epoch = self.new_epoch();
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::Eject { epoch });
                }
                self.stack.apply(Patch {
                    tape: Some(None),
                    tape_transport: Some(Transport::Stop),
                    tape_counter: Some(0.0),
                    tape_side: Some(Side::A),
                    ..Default::default()
                });
                self.status("tape ejected");
            }

            Command::TapeDolby => {
                let v = !self.stack.tape.dolby;
                self.stack.apply(Patch { tape_dolby: Some(v), ..Default::default() });
            }

            Command::TapeAutoReverse => {
                let v = !self.stack.tape.auto_reverse;
                if let Some(e) = &self.engine {
                    e.send(EngineCmd::SetAutoReverse(v));
                }
                self.stack.apply(Patch { tape_auto_reverse: Some(v), ..Default::default() });
            }

            // ---- tuner ------------------------------------------------------
            Command::TunerBand => {
                self.status("AM needs a direct-sampling mod — this build is FM only");
            }

            Command::TunerStepUp => self.tune(self.stack.tuner.freq + state::FM_STEP),
            Command::TunerStepDown => self.tune(self.stack.tuner.freq - state::FM_STEP),

            Command::TunerSeekUp | Command::TunerSeekDown => {
                let dir = if matches!(cmd, Command::TunerSeekUp) { 1 } else { -1 };
                if let Some(e) = &self.engine {
                    e.radio.send(RadioCmd::Seek(dir, self.stack.tuner.local));
                }
                self.stack.apply(Patch {
                    tuner_seeking: Some(true),
                    tuner_preset: Some(None),
                    ..Default::default()
                });
            }

            Command::TunerLocal => {
                let v = !self.stack.tuner.local;
                self.stack.apply(Patch { tuner_local: Some(v), ..Default::default() });
            }

            Command::TunerPreset(i) => match self.stack.tuner.presets.get(i).copied().flatten() {
                Some(f) => {
                    self.tune(f);
                    self.stack.apply(Patch { tuner_preset: Some(Some(i)), ..Default::default() });
                }
                None => self.status(format!("preset {} is empty", i + 1)),
            },

            Command::TunerStorePreset(i) => {
                let mut p = self.stack.tuner.presets;
                if i < p.len() {
                    p[i] = Some(self.stack.tuner.freq);
                    let f = self.stack.tuner.freq;
                    self.stack.apply(Patch {
                        tuner_presets: Some(p),
                        tuner_preset: Some(Some(i)),
                        ..Default::default()
                    });
                    self.status(format!("preset {} = {f:.1} MHz", i + 1));
                }
            }

            // ---- browser ----------------------------------------------------
            Command::BrowserOpen => {
                self.browser.error = None;
                self.browser.refresh();
                self.browser.open = true;
            }
            Command::BrowserClose => self.browser.open = false,
            Command::BrowserUp => self.browser.move_by(-1),
            Command::BrowserDown => self.browser.move_by(1),
            Command::BrowserEnter => {
                self.browser.error = None;
                self.browser.enter();
            }
            Command::BrowserParent => {
                self.browser.error = None;
                self.browser.parent();
            }
            Command::BrowserSelect(i) => {
                // A click on the highlighted row means "open it"; on any other
                // row it just moves the highlight there.
                if self.browser.cursor == i {
                    self.browser.enter();
                } else {
                    self.browser.cursor = i;
                }
            }
            Command::BrowserLoadDisc => {
                let path = self.browser.selected().map(|e| e.path.clone());
                if let Some(p) = path {
                    self.browser.open = false;
                    if self.stack.source != SourceKind::Cd {
                        self.select_source(SourceKind::Cd);
                    }
                    self.load(p);
                }
            }
            Command::BrowserLoadTape => match self.browser.as_tape() {
                Ok(tape) => {
                    self.browser.open = false;
                    let msg = format!(
                        "tape: {} · {} tracks · side A {}, side B {}",
                        tape.title,
                        tape.tracks.len(),
                        tape.side_range(Side::A).len(),
                        tape.side_range(Side::B).len()
                    );
                    let epoch = self.new_epoch();
                    if let Some(e) = &self.engine {
                        e.send(EngineCmd::LoadTape { tape: Arc::new(tape.clone()), epoch });
                    }
                    if self.stack.source != SourceKind::Tape {
                        self.select_source(SourceKind::Tape);
                    }
                    self.stack.apply(Patch {
                        tape: Some(Some(tape)),
                        tape_side: Some(Side::A),
                        tape_index: Some(0),
                        tape_counter: Some(0.0),
                        tape_transport: Some(Transport::Stop),
                        ..Default::default()
                    });
                    self.status(msg);
                }
                Err(e) => self.browser.error = Some(e.to_string()),
            },

            Command::EqSelect { bank, band } => {
                self.stack.eq.cursor = (bank, band.min(8));
            }

            Command::EqDefeat => {
                let v = !self.stack.eq.defeat;
                self.stack.apply(Patch { eq_defeat: Some(v), ..Default::default() });
                self.publish(true);
            }

            Command::EqFlat => {
                self.stack.apply(Patch {
                    eq_front: Some([0.0; 9]),
                    eq_rear: Some([0.0; 9]),
                    ..Default::default()
                });
                self.publish(true);
            }

            Command::AmpPower => {
                let v = !self.stack.amp.power;
                self.stack.apply(Patch { amp_power: Some(v), ..Default::default() });
                self.publish(false);
            }

            Command::VolUp | Command::VolDown => {
                let d = if matches!(cmd, Command::VolUp) { 0.04 } else { -0.04 };
                let v = (self.stack.ctrl.volume + d).clamp(0.0, 1.0);
                self.stack.apply(Patch {
                    volume: Some(v),
                    // Touching VOLUME up releases the attenuator, as on the panel.
                    att: if d > 0.0 { Some(false) } else { None },
                    ..Default::default()
                });
                self.publish(false);
            }

            Command::Volume(v) => {
                let v = v.clamp(0.0, 1.0);
                self.stack.apply(Patch {
                    volume: Some(v),
                    att: Some(false),
                    ..Default::default()
                });
                self.publish(false);
            }

            Command::Att => {
                let v = !self.stack.ctrl.att;
                self.stack.apply(Patch { att: Some(v), ..Default::default() });
                self.publish(false);
            }

            Command::BassUp | Command::BassDown => {
                let d = if matches!(cmd, Command::BassUp) { 1 } else { -1 };
                let v = (self.stack.ctrl.bass + d).clamp(-2, 2);
                self.stack.apply(Patch { bass: Some(v), ..Default::default() });
                self.publish(true);
            }

            Command::TrebleUp | Command::TrebleDown => {
                let d = if matches!(cmd, Command::TrebleUp) { 1 } else { -1 };
                let v = (self.stack.ctrl.treble + d).clamp(-2, 2);
                self.stack.apply(Patch { treble: Some(v), ..Default::default() });
                self.publish(true);
            }

            Command::Bass(v) => {
                self.stack.apply(Patch { bass: Some(v.clamp(-2, 2)), ..Default::default() });
                self.publish(true);
            }

            Command::Treble(v) => {
                self.stack.apply(Patch { treble: Some(v.clamp(-2, 2)), ..Default::default() });
                self.publish(true);
            }

            Command::Fader(v) => {
                self.stack.apply(Patch { fader: Some(v.clamp(0.0, 1.0)), ..Default::default() });
                self.publish(false);
            }

            Command::Ill => {
                let v = self.stack.ctrl.ill.toggle();
                self.stack.apply(Patch { ill: Some(v), ..Default::default() });
            }
        }
    }

    /// Which transport the shared keys act on.
    fn transport_cmd(&self, cd: Command, tape: Command, tuner: Option<Command>) -> Option<Command> {
        match self.stack.source {
            SourceKind::Cd => Some(cd),
            SourceKind::Tape => Some(tape),
            SourceKind::Tuner => tuner,
        }
    }

    fn select_source(&mut self, kind: SourceKind) {
        let epoch = self.new_epoch();
        if let Some(e) = &self.engine {
            e.send(EngineCmd::SelectSource { source: kind, epoch });
            // Only demodulate when the tuner is actually the source; the SDR
            // keeps reading either way so seek and the meter stay live.
            e.radio.send(RadioCmd::Enable(kind == SourceKind::Tuner));
        }
        // Selecting a source stops whatever the others were doing, the way a
        // single-transport head unit has to.
        self.stack.apply(Patch {
            source: Some(kind),
            transport: Some(Transport::Stop),
            tape_transport: Some(Transport::Stop),
            ..Default::default()
        });
        self.status(format!("source: {}", kind.label()));
    }

    /// Running time on the current side up to the start of `index` — the
    /// deck's counter is linear across a side, so a track position has to be
    /// added to everything before it.
    fn tape_prefix(&self, index: usize) -> f64 {
        let Some(tape) = self.stack.tape.tape.as_ref() else { return 0.0 };
        let r = tape.side_range(self.stack.tape.side);
        let end = index.clamp(r.start, r.end);
        tape.tracks[r.start..end].iter().map(|t| t.seconds).sum()
    }

    /// Cue a track on the tape, staying inside the current side.
    fn tape_cue(&mut self, index: usize) {
        let Some(tape) = self.stack.tape.tape.as_ref() else { return };
        let range = tape.side_range(self.stack.tape.side);
        if !range.contains(&index) {
            return;
        }
        let epoch = self.new_epoch();
        if let Some(e) = &self.engine {
            e.send(EngineCmd::Cue { index, epoch });
        }
        self.stack.apply(Patch { tape_counter: Some(0.0), ..Default::default() });
    }

    fn tune(&mut self, mhz: f64) {
        let mhz = mhz.clamp(state::FM_LO, state::FM_HI);
        if let Some(e) = &self.engine {
            e.radio.send(RadioCmd::Tune(mhz));
        }
        self.stack.apply(Patch {
            tuner_freq: Some(mhz),
            tuner_preset: Some(None),
            ..Default::default()
        });
    }

    fn track_count(&self) -> usize {
        self.stack.cd.disc.as_ref().map_or(0, |d| d.tracks.len())
    }

    fn cue(&mut self, index: usize) {
        if index >= self.track_count() {
            return;
        }
        let epoch = self.new_epoch();
        if let Some(e) = &self.engine {
            e.send(EngineCmd::Cue { index, epoch });
        }
        // The transport still reads STOP until the engine's first mark lands.
        self.stack.apply(Patch { elapsed: Some(0.0), ..Default::default() });
    }

    // -- engine feedback ---------------------------------------------------

    fn poll_engine(&mut self) {
        let Some(engine) = &self.engine else { return };

        let mut incoming = Vec::new();
        while let Ok(evt) = engine.events.try_recv() {
            incoming.push(evt);
        }

        for evt in incoming {
            match evt {
                EngineEvent::TrackMark { epoch, source, index, side, at_frame } => {
                    if epoch == self.epoch {
                        self.marks.push_back(Mark { epoch, source, index, side, at_frame });
                    }
                }
                EngineEvent::Ended => {
                    let epoch = self.new_epoch();
                    if let Some(e) = &self.engine {
                        e.send(EngineCmd::Stop { epoch });
                    }
                    let tape = self.stack.source == SourceKind::Tape;
                    self.stack.apply(Patch {
                        transport: if tape { None } else { Some(Transport::Stop) },
                        track: if tape { None } else { Some(0) },
                        elapsed: if tape { None } else { Some(0.0) },
                        tape_transport: if tape { Some(Transport::Stop) } else { None },
                        tape_counter: if tape { Some(0.0) } else { None },
                        status: Some(if tape { "end of tape".into() } else { "end of disc".into() }),
                        ..Default::default()
                    });
                }
                EngineEvent::Status(s) => self.status(s),
            }
        }

        let Some(engine) = &self.engine else { return };
        let frames = engine.meters.frames_out.load(Ordering::Acquire);
        let rate = engine.sample_rate.max(1) as f64;
        let levels = engine.meters.read();

        // Promote any mark the output clock has now reached.
        while self.marks.front().is_some_and(|m| frames >= m.at_frame) {
            self.current = self.marks.pop_front();
        }

        // The radio reports itself continuously; it has no marks to promote.
        let radio = self.engine.as_ref().map(|e| &e.radio);
        let tuner_patch = radio.map(|r| Patch {
            tuner_freq: Some(r.shared.freq()),
            tuner_stereo: Some(r.shared.stereo()),
            tuner_rssi: Some(r.shared.rssi()),
            tuner_seeking: Some(r.shared.seeking()),
            tuner_device: r.shared.device().map(|d| Some(match d {
                Ok(name) => name,
                Err(why) => {
                    // An empty string is how the tuner panel says "not here";
                    // the reason still reaches the shelf strip.
                    let _ = why;
                    String::new()
                }
            })),
            ..Default::default()
        });

        let promoted = match &self.current {
            Some(m) if m.epoch == self.epoch => Some((m.source, m.index, m.side, m.at_frame)),
            _ => None,
        };

        let mut patch = Patch { amp_levels: Some(levels), ..Default::default() };
        if let Some(t) = tuner_patch {
            patch.tuner_freq = t.tuner_freq;
            patch.tuner_stereo = t.tuner_stereo;
            patch.tuner_rssi = t.tuner_rssi;
            patch.tuner_seeking = t.tuner_seeking;
            patch.tuner_device = t.tuner_device;
        }

        if let Some((source, index, side, at_frame)) = promoted {
            let elapsed = frames.saturating_sub(at_frame) as f64 / rate;
            match source {
                SourceKind::Cd => {
                    patch.track = Some(index + 1);
                    patch.elapsed = Some(elapsed);
                    if self.stack.cd.transport == Transport::Stop {
                        patch.transport = Some(Transport::Play);
                    }
                }
                SourceKind::Tape => {
                    patch.tape_index = Some(index);
                    patch.tape_side = Some(side);
                    // Counter is linear along the side, not per track.
                    let prefix = {
                        let saved = self.stack.tape.side;
                        self.stack.tape.side = side;
                        let p = self.tape_prefix(index);
                        self.stack.tape.side = saved;
                        p
                    };
                    patch.tape_counter = Some(prefix + elapsed);
                    if !self.stack.tape.transport.is_running() {
                        patch.tape_transport = Some(Transport::Play);
                    }
                }
                SourceKind::Tuner => {}
            }
        }

        self.stack.apply(patch);
    }

    // -- input -------------------------------------------------------------

    fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(2),
            MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(2),
            MouseEventKind::Down(MouseButton::Left) => {
                if self.show_help {
                    self.show_help = false;
                    return;
                }
                if let Some(cmd) = self.hits.hit(m.column, m.row).cloned() {
                    self.dispatch(cmd);
                }
            }
            _ => {}
        }
    }

    /// Keys while the browser has focus. It is modal, so it consumes
    /// everything rather than letting a stray `d` defeat the equaliser.
    fn on_key_browser(&mut self, k: KeyEvent) {
        let cmd = match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('o') => Command::BrowserClose,
            KeyCode::Up | KeyCode::Char('k') => Command::BrowserUp,
            KeyCode::Down | KeyCode::Char('j') => Command::BrowserDown,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => Command::BrowserEnter,
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => Command::BrowserParent,
            KeyCode::Char('d') => Command::BrowserLoadDisc,
            KeyCode::Char('t') => Command::BrowserLoadTape,
            _ => return,
        };
        self.dispatch(cmd);
    }

    fn on_key(&mut self, k: KeyEvent) {
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.browser.open {
            self.on_key_browser(k);
            return;
        }

        let eq_cursor = self.stack.eq.cursor;
        let tuner = self.stack.source == SourceKind::Tuner;
        let cmd = match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => Some(Command::Quit),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(Command::Quit),
            (KeyCode::Char('?'), _) => {
                self.show_help = true;
                None
            }

            // source
            (KeyCode::Char('c'), _) => Some(Command::Source(SourceKind::Cd)),
            (KeyCode::Char('t'), _) => Some(Command::Source(SourceKind::Tape)),
            (KeyCode::Char('u'), _) => Some(Command::Source(SourceKind::Tuner)),
            (KeyCode::Char('o'), _) => Some(Command::BrowserOpen),

            // transport — the shared keys act on whichever source is selected
            (KeyCode::Char(' '), _) => {
                self.transport_cmd(Command::CdPlayPause, Command::TapePlayPause, None)
            }
            (KeyCode::Char('s'), _) => {
                self.transport_cmd(Command::CdStop, Command::TapeStop, None)
            }
            (KeyCode::Right, _) | (KeyCode::Char('n'), _) => self.transport_cmd(
                Command::CdNext,
                Command::TapeApsNext,
                Some(Command::TunerSeekUp),
            ),
            (KeyCode::Left, _) | (KeyCode::Char('p'), _) => self.transport_cmd(
                Command::CdPrev,
                Command::TapeApsPrev,
                Some(Command::TunerSeekDown),
            ),
            (KeyCode::Char('['), _) => Some(Command::TunerStepDown),
            (KeyCode::Char(']'), _) => Some(Command::TunerStepUp),
            (KeyCode::Char('g'), _) => Some(Command::TunerLocal),
            (KeyCode::Char('e'), _) => {
                self.transport_cmd(Command::CdEject, Command::TapeEject, None)
            }
            (KeyCode::Char('r'), _) => Some(Command::CdRepeat),
            (KeyCode::Char('z'), _) => Some(Command::CdRandom),
            (KeyCode::Char('v'), _) => Some(Command::TapeFlip),
            (KeyCode::Char('y'), _) => Some(Command::TapeDolby),
            (KeyCode::Char('a'), _) => Some(Command::TapeAutoReverse),

            // Digits cue a track, or recall a preset when the tuner is up.
            (KeyCode::Char(c @ '1'..='9'), _) => {
                let n = c.to_digit(10).unwrap() as usize - 1;
                Some(if tuner {
                    Command::TunerPreset(n)
                } else if self.stack.source == SourceKind::Tape {
                    let base = self
                        .stack
                        .tape
                        .tape
                        .as_ref()
                        .map(|t| t.side_range(self.stack.tape.side).start)
                        .unwrap_or(0);
                    Command::CdTrack(base + n)
                } else {
                    Command::CdTrack(n)
                })
            }
            // Shifted digits store the current station.
            (KeyCode::Char(c @ ('!' | '@' | '#' | '$' | '%' | '^')), _) => {
                let n = "!@#$%^".find(c).unwrap_or(0);
                Some(Command::TunerStorePreset(n))
            }

            // control head
            (KeyCode::Up, _) => Some(Command::VolUp),
            (KeyCode::Down, _) => Some(Command::VolDown),
            (KeyCode::Char('m'), _) => Some(Command::Att),
            (KeyCode::Char(','), _) => Some(Command::BassDown),
            (KeyCode::Char('.'), _) => Some(Command::BassUp),
            (KeyCode::Char('<'), _) => Some(Command::TrebleDown),
            (KeyCode::Char('>'), _) => Some(Command::TrebleUp),
            (KeyCode::Char(';'), _) => Some(Command::Fader(self.stack.ctrl.fader - 0.0625)),
            (KeyCode::Char('\''), _) => Some(Command::Fader(self.stack.ctrl.fader + 0.0625)),
            (KeyCode::Char('i'), _) => Some(Command::Ill),
            (KeyCode::Char('w'), _) => Some(Command::AmpPower),

            // equaliser
            (KeyCode::Char('h'), _) => {
                self.stack.eq.cursor.1 = eq_cursor.1.saturating_sub(1);
                None
            }
            (KeyCode::Char('l'), _) => {
                self.stack.eq.cursor.1 = (eq_cursor.1 + 1).min(8);
                None
            }
            (KeyCode::Char('f'), _) => {
                self.stack.eq.cursor.0 =
                    if eq_cursor.0 == Bank::Front { Bank::Rear } else { Bank::Front };
                None
            }
            (KeyCode::Char('k'), _) => Some(Command::EqBand {
                bank: eq_cursor.0,
                band: eq_cursor.1,
                db: self.stack.eq.bank(eq_cursor.0)[eq_cursor.1] + STEP_DB,
            }),
            (KeyCode::Char('j'), _) => Some(Command::EqBand {
                bank: eq_cursor.0,
                band: eq_cursor.1,
                db: self.stack.eq.bank(eq_cursor.0)[eq_cursor.1] - STEP_DB,
            }),
            (KeyCode::Char('d'), _) => Some(Command::EqDefeat),
            (KeyCode::Char('0'), _) => Some(Command::EqFlat),

            // rack scrolling
            (KeyCode::PageDown, _) | (KeyCode::Char('J'), _) => {
                self.scroll = self.scroll.saturating_add(4);
                None
            }
            (KeyCode::PageUp, _) | (KeyCode::Char('K'), _) => {
                self.scroll = self.scroll.saturating_sub(4);
                None
            }

            _ => None,
        };

        if let Some(c) = cmd {
            self.dispatch(c);
        }
    }
}

// ---------------------------------------------------------------------------

/// Render one frame to stdout with real colours and exit.
///
/// Being able to look at the panel without occupying a terminal is worth a
/// flag: layout bugs in a fixed-geometry rack are far easier to see in a
/// scrollback buffer than in a live session.
fn screenshot(app: &mut App, width: u16, height: u16) -> Result<()> {
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    let mut term = Terminal::new(TestBackend::new(width, height))?;
    app.poll_engine();
    let mut hits = HitMap::new();
    term.draw(|f| {
        ui::draw(f, &app.stack, app.scroll, &mut hits);
        if app.browser.open {
            let theme = ui::theme::Theme::new(app.stack.ctrl.ill);
            ui::overlay::draw_browser(f, &app.browser, &theme, &mut hits);
        }
    })?;

    let buf = term.backend().buffer();
    let mut out = String::new();
    let code = |c: Color, layer: u8| match c {
        Color::Rgb(r, g, b) => format!("\x1b[{layer};2;{r};{g};{b}m"),
        _ => String::new(),
    };

    for y in 0..height {
        for x in 0..width {
            let Some(cell) = buf.cell((x, y)) else { continue };
            out.push_str(&code(cell.fg, 38));
            out.push_str(&code(cell.bg, 48));
            out.push_str(cell.symbol());
        }
        out.push_str("\x1b[0m\n");
    }
    print!("{out}");
    Ok(())
}

/// Open the radio, sweep a few channels, and report what came back.
///
/// The unit tests prove the discriminator's maths against a synthetic carrier;
/// this proves the whole path against the actual antenna, which is the only
/// thing that can tell you the dongle is tuned where you think it is.
fn radio_check() -> Result<()> {
    let engine = audio::start()?;
    let radio = &engine.radio;

    // Give the device thread a moment to open and report itself.
    std::thread::sleep(Duration::from_millis(700));
    match radio.shared.device() {
        Some(Ok(name)) => println!("radio: {name}"),
        Some(Err(why)) => {
            println!("radio unavailable: {why}");
            return Ok(());
        }
        None => {
            println!("radio: still probing — no answer in 700 ms");
            return Ok(());
        }
    }

    radio.send(audio::radio::RadioCmd::Enable(true));
    println!("{:>8}  {:>6}  {:>8}  {:>6}", "MHz", "SIGNAL", "dBFS", "STEREO");

    let mut best: Option<(f64, f32)> = None;
    let mut f = state::FM_LO;
    while f <= state::FM_HI {
        radio.send(audio::radio::RadioCmd::Tune(f));
        std::thread::sleep(Duration::from_millis(220));
        let rssi = radio.shared.rssi();
        if best.is_none_or(|(_, b)| rssi > b) {
            best = Some((f, rssi));
        }
        let bar: String = "█".repeat((rssi * 20.0) as usize);
        println!(
            "{f:>8.1}  {:>5.0}%  {:>8.1}  {:>6}  {bar}",
            rssi * 100.0,
            radio.shared.rssi_db(),
            if radio.shared.stereo() { "yes" } else { "-" }
        );
        f += 0.5;
    }

    // End-to-end: park on the strongest channel found, select the tuner, and
    // read the amplifier's own meters. If those move, audio has travelled the
    // whole path — demodulator, ring, DSP chain, output callback.
    let best = best.unwrap_or((state::FM_LO, 0.0));
    println!("\nparking on {:.1} MHz and checking the audio path", best.0);
    radio.send(audio::radio::RadioCmd::Tune(best.0));
    engine.send(EngineCmd::SelectSource { source: SourceKind::Tuner, epoch: 1 });
    engine.send(EngineCmd::Play);
    std::thread::sleep(Duration::from_millis(1500));

    let bands = engine.meters.read();
    let lit = bands.iter().filter(|v| **v > 0.02).count();
    println!("meters: {:?}", bands.map(|v| (v * 100.0) as u32));
    if lit == 0 {
        println!("no audio reached the output — the demodulator or the ring is stalled");
    } else {
        println!("audio present in {lit}/9 bands · stereo: {}", radio.shared.stereo());
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--radio-check") {
        return radio_check();
    }
    let shot = args.iter().any(|a| a == "--screenshot");
    let arg = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);

    // The panel should come up even with no sound card — it says so in the
    // colophon rather than refusing to start.
    let (engine, engine_err) = match audio::start() {
        Ok(e) => (Some(e), None),
        Err(e) => (None, Some(e.to_string())),
    };

    let mut app = App::new(engine);
    if let Some(msg) = engine_err {
        app.status(format!("audio engine unavailable: {msg}"));
    }
    app.publish(true);

    // Put something in the tray: the argument, or the first album under the
    // user's music directory.
    let start_disc = arg.or_else(|| music_dir().and_then(|m| disc::first_disc(&m)));
    match start_disc {
        Some(p) => app.load(p),
        None => app.status("no disc — pass a directory of audio files as an argument"),
    }

    if shot {
        // Give the engine a moment to open the first track so the display has
        // something real to show rather than a blank transport.
        app.dispatch(Command::CdPlayPause);
        if args.iter().any(|a| a == "--browser") {
            app.dispatch(Command::BrowserOpen);
        }
        app.stack.eq.front = [0.0, 3.0, 6.0, 3.0, 0.0, -3.0, 0.0, 6.0, 9.0];
        app.stack.eq.rear = [6.0, 3.0, 0.0, 0.0, -3.0, -6.0, -3.0, 0.0, 3.0];
        app.publish(true);
        std::thread::sleep(Duration::from_millis(700));
        return screenshot(&mut app, 110, ui::RACK_HEIGHT);
    }

    let mut terminal = ratatui::init();
    // Mouse capture costs the terminal's native text selection; most emulators
    // still give it back under Shift, and having the panel be clickable is
    // worth that trade for a device that was all buttons.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = run(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

fn music_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join("Music");
    p.is_dir().then_some(p)
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last = Instant::now();

    while app.running {
        app.poll_engine();

        let view_h = terminal.size()?.height;
        app.scroll = ui::clamp_scroll(app.scroll as i32, view_h);

        let mut hits = HitMap::new();
        terminal.draw(|f| {
            ui::draw(f, &app.stack, app.scroll, &mut hits);
            let theme = ui::theme::Theme::new(app.stack.ctrl.ill);
            if app.browser.open {
                ui::overlay::draw_browser(f, &app.browser, &theme, &mut hits);
            }
            if app.show_help {
                ui::overlay::draw_help(f, &theme);
            }
        })?;
        app.hits = hits;

        // Poll for whatever is left of the frame budget.
        let wait = FRAME.saturating_sub(last.elapsed());
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                Event::Mouse(m) => app.on_mouse(m),
                _ => {}
            }
        }
        last = Instant::now();
    }
    Ok(())
}
