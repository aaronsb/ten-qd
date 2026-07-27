//! Fujitsu Ten component stack — a TUI head unit that actually plays.
//!
//! The panel is a readout, never a wish. Pressing a key emits a `Command`; the
//! display only changes when the engine reports back through a `Patch`. That
//! is the same one-way contract the HTML prototype set up, and keeping it is
//! what stops the transport from claiming to play a file it could not open.

mod adapter;
mod audio;
mod browser;
mod disc;
mod listen;
mod memory;
mod mpris;
mod playlist;
mod project;
mod state;
mod ui;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;

use audio::dsp::DspParams;
use audio::radio::RadioCmd;
use audio::{EngineCmd, EngineEvent};
use browser::Browser;
use memory::{Keeper, Memory};
use state::{Bank, Command, Patch, RecState, Side, SourceKind, Stack, Transport, Unit};
use ui::hit::HitMap;
use ui::units::eq::{snap, RANGE_DB, STEP_DB};

const FRAME: Duration = Duration::from_millis(33);

/// How many frames a unit's activity lamp stays lit after it is operated.
/// At 33 ms a frame this is about a sixth of a second — long enough to read as
/// a flash, short enough not to look like a state.
const BLINK_FRAMES: u8 = 5;

const USAGE: &str = "\
ten-qd — a terminal music player wearing an 80s car stereo

    ten-qd [FOLDER]        load FOLDER as a disc, or resume the last one
    ten-qd --screenshot    render one frame to stdout and exit
                           (--aux --browser --outputs --arrange --fold=… --hide=…)
    ten-qd --radio-check   sweep the FM band and report signal per channel
    ten-qd --aux-check     open the aux input and check the cable
    ten-qd --log           what the listening log holds, by session
    ten-qd --export[=WHAT] cut a playlist out of the log, to stdout
                           WHAT: all (default) · last · a date or session,
                           e.g. 2026-07 · add --rank to order by play count
    ten-qd --forget        clear the 12-volt memory (presets, tone, last disc)
    ten-qd --help          this

Settings persist to $XDG_STATE_HOME/ten-qd/memory.toml, and REC writes
listening.jsonl beside it. Press ? inside the panel for the key map.";

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
    /// The output picker, when open, and where its cursor sits.
    outputs_open: bool,
    outputs_cursor: usize,
    /// The aux source picker, when open, and where its cursor sits.
    aux_open: bool,
    aux_cursor: usize,
    /// The rack arranger, when open. `grabbed` means the arrows are carrying
    /// the highlighted unit rather than moving past it.
    layout_open: bool,
    layout_cursor: usize,
    layout_grabbed: bool,
    /// The null sink, while an adapter is inserted. Dropping it removes the
    /// sink, so the desktop never keeps a phantom audio device after we exit.
    adapter: Option<adapter::Adapter>,
    /// Watches whatever is on the other end of the cable.
    mpris: mpris::Mpris,
    /// TRACK mode: what has been heard so far this session, and where it is
    /// written down. The watcher runs whether or not REC is on — it is fed
    /// only while it is, so switching REC on starts a clean slate rather than
    /// back-dating whatever happened to be playing.
    watcher: listen::Watcher,
    /// `None` when there is no HOME. Writing the log to the current directory
    /// instead would put it somewhere `--export` does not look, which is worse
    /// than not writing it: the operator would be told entries were saved and
    /// then be unable to find one of them.
    log: Option<listen::Log>,
    /// Streams offered for plugging in, refreshed when the adapter goes in.
    streams: Vec<adapter::Stream>,
    /// Where the loop guard should put us back, shared with its thread. It
    /// has to be told when the operator changes output, or it would restore
    /// whatever was remembered at start-up for the rest of the run.
    guard_target: Arc<std::sync::Mutex<Option<String>>>,
}

impl App {
    fn new(engine: Option<audio::Engine>, mem: &Memory) -> Self {
        let mut stack = Stack::default();
        if let Some(e) = &engine {
            stack.cd.sample_rate = e.sample_rate;
        }

        // Restore the panel before the first frame, so nothing is ever drawn
        // in a state the operator did not leave it in.
        stack.source = mem.source_kind();
        stack.ctrl.volume = mem.volume;
        stack.ctrl.att = mem.att;
        stack.ctrl.gain_db =
            mem.gain_db.clamp(-audio::dsp::GAIN_LIMIT_DB, audio::dsp::GAIN_LIMIT_DB);
        stack.ctrl.bass = mem.bass;
        stack.ctrl.treble = mem.treble;
        stack.ctrl.fader = mem.fader;
        stack.ctrl.ill = mem.illumination();
        stack.ctrl.dimmer = mem.dimmer.min(ui::theme::DIM_MAX);
        stack.amp.power = mem.powered(state::Unit::Amp);
        stack.cd.power = mem.powered(state::Unit::Cd);
        stack.tape.power = mem.powered(state::Unit::Tape);
        stack.aux.power = mem.powered(state::Unit::Aux);
        stack.eq.defeat = !mem.powered(state::Unit::Eq);
        stack.eq.front = mem.eq_bank(Bank::Front);
        stack.eq.rear = mem.eq_bank(Bank::Rear);
        stack.tuner.freq = mem.tuner_freq;
        stack.tuner.local = mem.tuner_local;
        stack.tuner.power = mem.powered(state::Unit::Tuner);
        stack.output = mem.output.clone();
        stack.outputs = adapter::sinks().into_iter().map(|s| s.description).collect();
        stack.tuner.presets = mem.presets();
        stack.cd.repeat = mem.repeat;
        stack.cd.random = mem.random;
        stack.tape.dolby = mem.dolby;
        stack.tape.auto_reverse = mem.auto_reverse;
        stack.tape.rec.on = mem.rec;
        stack.layout = mem.layout();

        let started = SystemTime::now();
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
            browser: Browser::new(
                music_dir().unwrap_or_else(|| PathBuf::from("/")),
                mem.browser.clone(),
            ),
            outputs_open: false,
            outputs_cursor: 0,
            aux_open: false,
            aux_cursor: 0,
            layout_open: false,
            layout_cursor: 0,
            layout_grabbed: false,
            adapter: None,
            mpris: mpris::Mpris::start(),
            watcher: listen::Watcher::new(listen::session_id(started)),
            log: listen::Log::path().map(listen::Log::at),
            streams: Vec::new(),
            guard_target: Arc::new(std::sync::Mutex::new(mem.output.clone())),
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
            gain_db: s.ctrl.gain_db,
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
        // A playlist is a tape, not a disc: someone already chose the running
        // order, and a disc is a folder read in sorted order. This is also
        // what `--export` writes, so a tape cut out of the log can be handed
        // straight back to the deck by name.
        if playlist::is_playlist(&path) {
            match browser::tape_from_playlist(&path) {
                Ok((tape, skipped)) => {
                    let msg = tape_line(&tape, skipped);
                    self.insert_tape(tape, true);
                    self.status(msg);
                }
                Err(e) => self.status(format!("no tape: {e}")),
            }
            return;
        }
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
        // Every command lights its unit's lamp, whether or not that unit is
        // currently on screen. A rack you cannot see still answers.
        if let Some(u) = cmd.unit() {
            self.stack.blink[u.index()] = BLINK_FRAMES;
        }
        match cmd {
            Command::Quit => self.running = false,

            // Power is the signal path; the arranger's hidden/shown is only
            // what you are looking at. A hidden unit still plays. A unit with
            // its power off is out of the chain, which is why turning one off
            // under the transport stops the transport rather than quietly
            // carrying on into a box that is no longer there.
            Command::UnitPower(u) => {
                let mut patch = Patch::default();
                let on = match u {
                    Unit::Cd => {
                        patch.cd_power = Some(!self.stack.cd.power);
                        !self.stack.cd.power
                    }
                    Unit::Tape => {
                        patch.tape_power = Some(!self.stack.tape.power);
                        !self.stack.tape.power
                    }
                    Unit::Aux => {
                        let on = !self.stack.aux.power;
                        // Off takes the device off the desktop too. Nothing
                        // else drops it — the refactor that made AUX a source
                        // took the old eject path with it — so without this the
                        // sink outlives its usefulness until the program exits,
                        // still offered in everyone's audio settings.
                        if !on {
                            if let Some(e) = &self.engine {
                                e.capture.arm(false);
                            }
                            self.adapter = None;
                            self.streams.clear();
                            self.mpris.prefer(None);
                            patch.aux = Some(Default::default());
                        }
                        patch.aux_power = Some(on);
                        on
                    }
                    // The equaliser's power switch *is* its bypass. Giving it
                    // a second name would be two controls for one circuit.
                    Unit::Eq => {
                        self.dispatch(Command::EqDefeat);
                        return;
                    }
                    Unit::Amp => {
                        self.dispatch(Command::AmpPower);
                        return;
                    }
                    Unit::Tuner => {
                        self.dispatch(Command::TunerPower);
                        return;
                    }
                    // The head is the rack. There is nothing to switch it off
                    // with, and nothing left to press if you had.
                    Unit::Ctrl => return,
                };
                self.stack.apply(patch);
                // A deck with no power is not writing anything down either.
                // The switch position is left alone — pull the power and put
                // it back and the log picks up where it was, which is what
                // always-append is for.
                if u == Unit::Tape && !on {
                    self.rec_stop();
                }
                let selected = matches!(
                    (u, self.stack.source),
                    (Unit::Cd, SourceKind::Cd)
                        | (Unit::Tape, SourceKind::Tape)
                        | (Unit::Aux, SourceKind::Aux)
                );
                if !on && selected {
                    let epoch = self.new_epoch();
                    if let Some(e) = &self.engine {
                        e.send(EngineCmd::Stop { epoch });
                    }
                    self.stack.apply(Patch {
                        transport: Some(Transport::Stop),
                        tape_transport: Some(Transport::Stop),
                        ..Default::default()
                    });
                }
                // Switching a live source back on has to put it back in the
                // chain. Stopping on power-down leaves CD and TAPE reading STOP
                // truthfully, but AUX has no transport field to read — so
                // without this the bay showed powered, live and metering while
                // the decoder sat idle, and the only cure was to press `a`.
                if on && selected {
                    self.select_source(self.stack.source);
                }
                self.status(format!(
                    "{} {}",
                    u.label(),
                    if on { "in the signal path" } else { "out of the signal path" }
                ));
            }

            Command::UnitCollapse(u) => {
                let i = u.index();
                self.stack.layout.collapsed[i] = !self.stack.layout.collapsed[i];
                let what = if self.stack.layout.collapsed[i] { "folded" } else { "opened" };
                self.status(format!("{} {what}", u.label()));
            }

            Command::LayoutOpen => {
                self.layout_open = true;
                self.layout_grabbed = false;
                self.layout_cursor = 0;
            }
            // Escape preserves whatever is on screen — the arranging *was* the
            // editing, so there is nothing to cancel back to.
            Command::LayoutClose => {
                self.layout_open = false;
                self.layout_grabbed = false;
            }
            Command::LayoutUp | Command::LayoutDown => {
                let down = matches!(cmd, Command::LayoutDown);
                if self.layout_grabbed {
                    self.layout_cursor = self.stack.layout.bubble(self.layout_cursor, down);
                } else if down {
                    self.layout_cursor = (self.layout_cursor + 1).min(Unit::ALL.len() - 1);
                } else {
                    self.layout_cursor = self.layout_cursor.saturating_sub(1);
                }
            }
            Command::LayoutGrab => {
                self.layout_grabbed = !self.layout_grabbed;
                let u = self.stack.layout.order[self.layout_cursor];
                self.status(if self.layout_grabbed {
                    format!("{} — arrows move it, space to set it down", u.label())
                } else {
                    format!("{} set down", u.label())
                });
            }
            Command::LayoutToggle => {
                let u = self.stack.layout.order[self.layout_cursor];
                let i = u.index();
                self.stack.layout.hidden[i] = !self.stack.layout.hidden[i];
                self.status(if self.stack.layout.hidden[i] {
                    format!("{} out of the rack — it still plays", u.label())
                } else {
                    format!("{} back in the rack", u.label())
                });
            }
            Command::LayoutReset => {
                self.stack.layout = Default::default();
                self.layout_cursor = 0;
                self.layout_grabbed = false;
                self.status("rack reset — every unit in, in signal order");
            }

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
                if let Some(e) = &self.engine
                    && self.stack.source == SourceKind::Cd
                {
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
                // Every bay is drawn and clickable at all times, so a click on
                // this one while another source is selected must empty the
                // tray without touching the transport that is actually
                // running. The keyboard is already routed by source; the mouse
                // is not, and `Eject` clears `playing` for whatever is playing.
                let epoch = self.new_epoch();
                if let Some(e) = &self.engine
                    && self.stack.source == SourceKind::Cd
                {
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
                if let Some(e) = &self.engine
                    && self.stack.source == SourceKind::Tape
                {
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
                if let Some(e) = &self.engine
                    && self.stack.source == SourceKind::Tape
                {
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

            // TRACK mode. What this records is every media player on the
            // desktop, which is not the same thing as what the rack is
            // playing — so the status says which it is rather than leaving
            // "recording" to be read as the tape heads.
            Command::TapeRecord => {
                let Some(log) = self.log.as_ref().map(|l| l.file().display().to_string()) else {
                    self.status("no HOME — there is nowhere to write a log");
                    return;
                };
                // REC is a switch, not an action on the signal path, so it
                // moves whether or not the deck has power — otherwise a rack
                // that came up with REC remembered and the deck switched off
                // would need powering up merely to switch REC back off. The
                // lamp is what reports whether anything is actually being
                // written, and it reads `power` as well as this.
                let on = !self.stack.tape.rec.on;
                if !on {
                    self.rec_stop();
                }
                let mut rec = self.stack.tape.rec.clone();
                rec.on = on;
                // Switching on clears a previous failure: this is the operator
                // saying "try again", and a latch nothing can reset would
                // outlive the fault it was reporting.
                rec.failed = false;
                let wrote = rec.wrote;
                self.stack.apply(Patch { tape_rec: Some(rec), ..Default::default() });

                self.status(match (on, self.stack.tape.power) {
                    // The session name is here because it is what selects a
                    // run back out of the log later — the tape you will cut.
                    (true, true) => format!("REC · session {} · {log}", self.watcher.session()),
                    (true, false) => {
                        "REC set, but TAPE has no power — nothing is being written".into()
                    }
                    (false, _) => format!("REC off — {wrote} entries logged this session"),
                });
            }

            // ---- auxiliary input ------------------------------------------
            // The sink is created on demand rather than at start-up: a device
            // that appears in everyone's audio settings the moment the program
            // runs is a device they did not ask for.
            Command::AuxOpen => {
                if self.adapter.is_none() {
                    match adapter::Adapter::insert() {
                        Ok(a) => {
                            self.adapter = Some(a);
                            if let Some(e) = &self.engine {
                                e.capture.arm(true);
                            }
                        }
                        Err(e) => {
                            self.status(format!("no aux input: {e}"));
                            return;
                        }
                    }
                }
                self.streams = adapter::streams().unwrap_or_default();
                self.aux_open = true;
                self.aux_cursor = 0;
                self.status(if self.streams.is_empty() {
                    format!("nothing playing — or send audio to \"{}\"", adapter::DESCRIPTION)
                } else {
                    format!("{} playing — pick one", self.streams.len())
                });
            }
            Command::AuxClose => self.aux_open = false,
            Command::AuxUp => self.aux_cursor = self.aux_cursor.saturating_sub(1),
            Command::AuxDown => {
                self.aux_cursor =
                    (self.aux_cursor + 1).min(self.streams.len().saturating_sub(1));
            }

            Command::AuxSelect(i) => {
                // Re-list first: indices are the server's, and a stream that
                // ended since the menu was drawn must not be moved by number.
                self.streams = adapter::streams().unwrap_or_default();
                let Some(stream) = self.streams.get(i).cloned() else {
                    self.status("no such stream");
                    return;
                };
                let Some(a) = self.adapter.as_mut() else {
                    self.status("no aux input");
                    return;
                };
                match a.plug(&stream) {
                    Ok(()) => {
                        self.mpris.prefer(Some(stream.app.clone()));
                        let mut t = self.stack.aux.state.clone();
                        t.source = Some(stream.label());
                        self.stack.apply(Patch { aux: Some(t), ..Default::default() });
                        self.aux_open = false;
                        if self.stack.source != SourceKind::Aux {
                            self.select_source(SourceKind::Aux);
                        }
                        self.status(format!("plugged in: {}", stream.app));
                    }
                    Err(e) => self.status(format!("could not plug that in: {e}")),
                }
            }

            // A real auxiliary input was a one-way cable and these keys would
            // have done nothing. They reach the player over MPRIS instead, and
            // say so when nothing is listening rather than pretending the
            // press landed.
            Command::AuxPlayPause
            | Command::AuxStop
            | Command::AuxNext
            | Command::AuxPrev => {
                let t = match cmd {
                    Command::AuxPlayPause => mpris::Transport::PlayPause,
                    Command::AuxStop => mpris::Transport::Stop,
                    Command::AuxNext => mpris::Transport::Next,
                    _ => mpris::Transport::Previous,
                };
                if !self.mpris.send(t) {
                    self.status("nothing on the cable is listening");
                }
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

            Command::TunerPower => {
                let v = !self.stack.tuner.power;
                if let Some(e) = &self.engine {
                    e.radio.send(RadioCmd::Enable(v && self.stack.source == SourceKind::Tuner));
                }
                self.stack.apply(Patch { tuner_power: Some(v), ..Default::default() });
                self.status(if v { "tuner on" } else { "tuner off" });
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
                Ok((tape, skipped)) => {
                    self.browser.open = false;
                    let msg = tape_line(&tape, skipped);
                    self.insert_tape(tape, true);
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

            Command::GainUp | Command::GainDown => {
                let d = if matches!(cmd, Command::GainUp) { 1 } else { -1 };
                let l = audio::dsp::GAIN_LIMIT_DB;
                let v = (self.stack.ctrl.gain_db + d).clamp(-l, l);
                self.stack.apply(Patch { gain_db: Some(v), ..Default::default() });
                self.status(match v {
                    0 => "GAIN: unity".to_string(),
                    v if v > 0 => format!("GAIN: {v:+} dB of makeup above unity"),
                    v => format!("GAIN: {v:+} dB, trimming the curve back"),
                });
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

            Command::OutputsOpen => {
                // Re-enumerate on open: devices come and go with a Bluetooth
                // connection or a USB interface, and a stale list would offer
                // something that is no longer there.
                let list: Vec<String> =
                    adapter::sinks().into_iter().map(|s| s.description).collect();
                self.outputs_cursor = self
                    .stack
                    .output
                    .as_ref()
                    .and_then(|o| list.iter().position(|d| d == o))
                    .unwrap_or(0);
                self.stack.apply(Patch { outputs: Some(list), ..Default::default() });
                self.outputs_open = true;
            }
            Command::OutputsClose => self.outputs_open = false,
            Command::OutputsUp => {
                let n = self.stack.outputs.len();
                if n > 0 {
                    self.outputs_cursor = (self.outputs_cursor + n - 1) % n;
                }
            }
            Command::OutputsDown => {
                let n = self.stack.outputs.len();
                if n > 0 {
                    self.outputs_cursor = (self.outputs_cursor + 1) % n;
                }
            }
            Command::OutputsSelect(i) => {
                // A click on the highlighted row chooses it; on any other row
                // it moves the highlight there — the browser's rule.
                if self.outputs_cursor != i {
                    self.outputs_cursor = i;
                    return;
                }
                let Some(desc) = self.stack.outputs.get(i).cloned() else { return };
                self.outputs_open = false;
                // Moving our own sink-input applies immediately; there is no
                // stream to rebuild and nothing to restart.
                let sink = adapter::sinks().into_iter().find(|s| s.description == desc);
                match sink {
                    Some(s) => match adapter::route_own_output(&s.name) {
                        Ok(()) => {
                            self.stack.apply(Patch {
                                output: Some(Some(desc.clone())),
                                ..Default::default()
                            });
                            // Tell the loop guard where "back" is now.
                            if let Ok(mut g) = self.guard_target.lock() {
                                *g = Some(desc.clone());
                            }
                            self.status(format!("output: {desc}"));
                        }
                        Err(e) => self.status(format!("could not route output: {e}")),
                    },
                    None => self.status(format!("{desc} is no longer there")),
                }
            }

            Command::DimUp | Command::DimDown => {
                let d = &mut self.stack.ctrl.dimmer;
                let v = if matches!(cmd, Command::DimUp) {
                    (*d + 1).min(ui::theme::DIM_MAX)
                } else {
                    d.saturating_sub(1)
                };
                self.stack.apply(Patch { dimmer: Some(v), ..Default::default() });
            }

            Command::Ill => {
                let v = self.stack.ctrl.ill.toggle();
                self.stack.apply(Patch { ill: Some(v), ..Default::default() });
            }
        }
    }

    /// Which transport the shared keys act on.
    fn transport_cmd(
        &self,
        cd: Command,
        tape: Command,
        tuner: Option<Command>,
        aux: Command,
    ) -> Option<Command> {
        match self.stack.source {
            SourceKind::Cd => Some(cd),
            SourceKind::Tape => Some(tape),
            SourceKind::Tuner => tuner,
            SourceKind::Aux => Some(aux),
        }
    }

    fn select_source(&mut self, kind: SourceKind) {
        // A unit that is out of the signal path cannot be the signal. Said
        // plainly rather than silently ignored, because a key that appears to
        // do nothing is worse than one that explains itself.
        let powered = match kind {
            SourceKind::Cd => self.stack.cd.power,
            SourceKind::Tape => self.stack.tape.power,
            SourceKind::Aux => self.stack.aux.power,
            SourceKind::Tuner => self.stack.tuner.power,
        };
        if !powered {
            self.status(format!("{} is switched off", kind.label()));
            return;
        }
        // No sink means no cable. Capture would have nothing to target, and an
        // unresolvable target is the failure this whole path was built to stop
        // being silent about.
        if kind == SourceKind::Aux && self.adapter.is_none() {
            self.status("no aux input — press A to choose a source");
            return;
        }
        let epoch = self.new_epoch();
        if let Some(e) = &self.engine {
            e.send(EngineCmd::SelectSource { source: kind, epoch });
            // Only demodulate when the tuner is actually the source; the SDR
            // keeps reading either way so seek and the meter stay live.
            e.radio.send(RadioCmd::Enable(
                kind == SourceKind::Tuner && self.stack.tuner.power,
            ));
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

    /// Put a tape in the deck. `select` switches the source to it, which the
    /// browser wants and a start-up restore does not — the memory decides
    /// which source is live on its own.
    fn insert_tape(&mut self, tape: state::Tape, select: bool) {
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
        if select && self.stack.source != SourceKind::Tape {
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

    /// Restore a remembered tape. A folder that has since moved or emptied is
    /// simply not loaded — it is not worth an error on the way in.
    fn load_tape_from(&mut self, dir: &std::path::Path) {
        if let Ok(tape) = browser::tape_from_dir(dir) {
            self.insert_tape(tape, false);
        }
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

    // ---- the listening log ------------------------------------------------

    /// Fold one look at the bus into the log.
    ///
    /// Called every frame. All of the deciding — where a track ends, when a
    /// player counts as having gone away — lives in the watcher; the only
    /// thing that happens here is that finished entries reach the disk and the
    /// panel is told what has actually been written.
    fn poll_log(&mut self) {
        if !self.recording() {
            return;
        }
        // A bus that did not answer is not a bus with nothing on it. Feeding
        // the watcher an empty list here would close every open entry, and the
        // outage clearing would open fresh ones — one track logged as two.
        // Skipping the poll entirely also means no time is credited across the
        // outage, which is the truthful account: nobody was looking.
        let Some(seen) = self.mpris.all() else { return };

        let done = self.watcher.observe(&seen, SystemTime::now());
        let wrote = self.write_log(&done);
        let following = self.watcher.following().count().min(u8::MAX as usize) as u8;

        let rec = &self.stack.tape.rec;
        let (was, total, failed) = (rec.following, rec.wrote + wrote, rec.failed);
        if wrote > 0 || following != was {
            self.stack.apply(Patch {
                tape_rec: Some(RecState { on: true, wrote: total, following, failed }),
                ..Default::default()
            });
        }
    }

    /// Whether the log is genuinely being appended to. REC is a switch
    /// position; a deck with its power off is not writing anything down, and
    /// the lamp reads this rather than the switch.
    fn recording(&self) -> bool {
        self.stack.tape.rec.on && self.stack.tape.power && self.log.is_some()
    }

    /// Close everything the watcher has open and write it out.
    ///
    /// A track that was playing when you pressed STOP was still played, and
    /// dropping it would lose the part you heard — so the tail of a session
    /// counts, at whatever length it actually ran.
    fn rec_stop(&mut self) {
        let done = self.watcher.close(SystemTime::now());
        let wrote = self.write_log(&done);
        let mut rec = self.stack.tape.rec.clone();
        rec.wrote += wrote;
        rec.following = 0;
        self.stack.apply(Patch { tape_rec: Some(rec), ..Default::default() });
    }

    /// Append entries, returning how many reached the disk.
    ///
    /// Counting successes rather than attempts is the point: `wrote` on the
    /// panel is a claim about the file, so a log that cannot be written stops
    /// counting and says why instead of reporting tracks it did not save.
    fn write_log(&mut self, entries: &[listen::Entry]) -> u32 {
        let mut n = 0;
        let mut failed = None;
        for e in entries {
            match self.log.as_ref().map(|l| l.append(e)) {
                Some(Ok(())) => n += 1,
                Some(Err(why)) => failed = Some(format!("log unwritable: {why}")),
                None => failed = Some("no HOME — the log has nowhere to live".into()),
            }
        }
        if let Some(why) = failed {
            // Latched, not just announced. The status line scrolls away within
            // seconds and the REC lamp does not, so a lamp burning over a log
            // that has stopped taking writes is exactly the display wishing
            // rather than reading. Cleared only by switching REC off and on.
            let mut rec = self.stack.tape.rec.clone();
            rec.failed = true;
            self.stack.apply(Patch { tape_rec: Some(rec), ..Default::default() });
            self.status(why);
        }
        n
    }

    fn poll_engine(&mut self) {
        // Let the activity lamps burn down. Done here rather than in the
        // renderer so a lamp lit by a command is timed by frames, not by how
        // often something happens to redraw.
        for b in &mut self.stack.blink {
            *b = b.saturating_sub(1);
        }

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

        // The cable reports what is on the other end of it. Only the aux
        // unit's own readout moves: the deck's transport is the deck's, and
        // letting a player's state drive it is what once had the panel reading
        // PLAY over a stopped decoder.
        {
            let a = &self.stack.aux.state;
            let mut next = a.clone();
            // Only when something is actually plugged in. `capture.running()`
            // means "pw-record is producing bytes" and MPRIS with no preference
            // reports whichever player happens to be playing — neither is a
            // statement about this cable. Left ungated, the bay described a
            // player going straight to the speakers as though it were coming
            // through the rack.
            if a.source.is_some() {
                let live = self.engine.as_ref().is_some_and(|e| e.capture.state.running());
                let np = self.mpris.now_playing();
                next.live = live && np.as_ref().is_some_and(|n| n.playing);
                next.player = np.as_ref().map(|n| n.player.clone());
                next.title = np.as_ref().map(|n| n.title.clone()).unwrap_or_default();
                next.artist = np.as_ref().map(|n| n.artist.clone()).unwrap_or_default();
            } else {
                next = Default::default();
            }
            if next != *a {
                self.stack.apply(Patch { aux: Some(next), ..Default::default() });
            }
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

        let mut patch = Patch {
            amp_levels: Some(levels),
            // Reported every frame regardless of source, so the aux bay shows
            // a signal arriving even while the rack is playing a disc.
            aux_input: Some(engine.capture.state.level()),
            ..Default::default()
        };
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
                SourceKind::Aux => {}
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
                // The overlays consume the keyboard; they have to consume the
                // mouse too. The hit map still holds the rack's zones from the
                // frame underneath, so a click meant for a panel row would
                // otherwise land on the bay behind it and eject a disc.
                if self.layout_open || self.aux_open {
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

    /// Keys while the output picker has focus. Modal, like the browser.
    fn on_key_outputs(&mut self, k: KeyEvent) {
        let cmd = match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('O') => Command::OutputsClose,
            KeyCode::Up | KeyCode::Char('k') => Command::OutputsUp,
            KeyCode::Down | KeyCode::Char('j') => Command::OutputsDown,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                Command::OutputsSelect(self.outputs_cursor)
            }
            _ => return,
        };
        self.dispatch(cmd);
    }

    /// Keys while the aux source picker has focus. Modal, like the browser.
    fn on_key_aux(&mut self, k: KeyEvent) {
        let cmd = match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('A') => Command::AuxClose,
            KeyCode::Up | KeyCode::Char('k') => Command::AuxUp,
            KeyCode::Down | KeyCode::Char('j') => Command::AuxDown,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                Command::AuxSelect(self.aux_cursor)
            }
            _ => return,
        };
        self.dispatch(cmd);
    }

    /// Keys while the rack arranger has focus. Modal, and deliberately small:
    /// move, grab, take out, reset, done.
    fn on_key_layout(&mut self, k: KeyEvent) {
        let cmd = match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('~') => Command::LayoutClose,
            KeyCode::Up | KeyCode::Char('k') => Command::LayoutUp,
            KeyCode::Down | KeyCode::Char('j') => Command::LayoutDown,
            KeyCode::Char(' ') => Command::LayoutGrab,
            KeyCode::Enter => Command::LayoutToggle,
            KeyCode::Char('r') => Command::LayoutReset,
            _ => return,
        };
        self.dispatch(cmd);
    }

    fn on_key(&mut self, k: KeyEvent) {
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.aux_open {
            self.on_key_aux(k);
            return;
        }
        if self.layout_open {
            self.on_key_layout(k);
            return;
        }
        if self.outputs_open {
            self.on_key_outputs(k);
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
            (KeyCode::Char('a'), _) => Some(Command::Source(SourceKind::Aux)),
            (KeyCode::Char('o'), _) => Some(Command::BrowserOpen),

            // transport — the shared keys act on whichever source is selected
            (KeyCode::Char(' '), _) => {
                self.transport_cmd(
                    Command::CdPlayPause,
                    Command::TapePlayPause,
                    None,
                    Command::AuxPlayPause,
                )
            }
            (KeyCode::Char('s'), _) => {
                self.transport_cmd(Command::CdStop, Command::TapeStop, None, Command::AuxStop)
            }
            (KeyCode::Right, _) | (KeyCode::Char('n'), _) => self.transport_cmd(
                Command::CdNext,
                Command::TapeApsNext,
                Some(Command::TunerSeekUp),
                Command::AuxNext,
            ),
            (KeyCode::Left, _) | (KeyCode::Char('p'), _) => self.transport_cmd(
                Command::CdPrev,
                Command::TapeApsPrev,
                Some(Command::TunerSeekDown),
                Command::AuxPrev,
            ),
            (KeyCode::Char('['), _) => Some(Command::TunerStepDown),
            (KeyCode::Char(']'), _) => Some(Command::TunerStepUp),
            (KeyCode::Char('g'), _) => Some(Command::TunerLocal),
            (KeyCode::Char('P'), _) => Some(Command::TunerPower),
            (KeyCode::Char('e'), _) => {
                self.transport_cmd(
                    Command::CdEject,
                    Command::TapeEject,
                    None,
                    Command::AuxClose,
                )
            }
            (KeyCode::Char('r'), _) => Some(Command::CdRepeat),
            (KeyCode::Char('z'), _) => Some(Command::CdRandom),
            (KeyCode::Char('v'), _) => Some(Command::TapeFlip),
            (KeyCode::Char('A'), _) => Some(Command::AuxOpen),
            (KeyCode::Char('y'), _) => Some(Command::TapeDolby),
            (KeyCode::Char('R'), _) => Some(Command::TapeRecord),
            // Auto-reverse moved off `a` when the aux input took it. `b` is
            // the side it turns the tape over to.
            (KeyCode::Char('b'), _) => Some(Command::TapeAutoReverse),

            // Digits cue a track, or recall a preset when the tuner is up.
            (KeyCode::Char(c @ '1'..='9'), _) => {
                let n = c.to_digit(10).unwrap() as usize - 1;
                Some(if tuner {
                    Command::TunerPreset(n)
                } else if self.stack.source == SourceKind::Aux {
                    Command::AuxSelect(n)
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
            (KeyCode::Char('~'), _) => Some(Command::LayoutOpen),
            (KeyCode::Char(c), _)
                if Unit::ALL.iter().any(|u| u.collapse_key() == c) =>
            {
                Unit::ALL.iter().find(|u| u.collapse_key() == c).map(|u| Command::UnitCollapse(*u))
            }
            (KeyCode::Char('{'), _) => Some(Command::GainDown),
            (KeyCode::Char('}'), _) => Some(Command::GainUp),
            (KeyCode::Char(','), _) => Some(Command::BassDown),
            (KeyCode::Char('.'), _) => Some(Command::BassUp),
            (KeyCode::Char('<'), _) => Some(Command::TrebleDown),
            (KeyCode::Char('>'), _) => Some(Command::TrebleUp),
            (KeyCode::Char(';'), _) => Some(Command::Fader(self.stack.ctrl.fader - 0.0625)),
            (KeyCode::Char('\''), _) => Some(Command::Fader(self.stack.ctrl.fader + 0.0625)),
            (KeyCode::Char('i'), _) => Some(Command::Ill),
            (KeyCode::Char('-'), _) => Some(Command::DimDown),
            (KeyCode::Char('='), _) | (KeyCode::Char('+'), _) => Some(Command::DimUp),
            (KeyCode::Char('w'), _) => Some(Command::AmpPower),
            (KeyCode::Char('O'), _) => Some(Command::OutputsOpen),

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
        let theme = ui::theme::Theme::for_stack(&app.stack);
        if app.browser.open {
            ui::overlay::draw_browser(f, &app.browser, &theme, &mut hits);
        }
        if app.outputs_open {
            ui::overlay::draw_outputs(f, &app.stack, app.outputs_cursor, &theme, &mut hits);
        }
        if app.aux_open {
            ui::overlay::draw_aux(f, &app.streams, app.aux_cursor, &theme, &mut hits);
        }
        if app.layout_open {
            ui::overlay::draw_layout(
                f,
                &app.stack,
                app.layout_cursor,
                app.layout_grabbed,
                &theme,
            );
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
    let _hush = Hush(silence_stderr());
    let engine = audio::start(None)?;
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

/// Insert the adapter, plug in whatever is playing, and report whether audio
/// actually reached the amplifier.
///
/// The unit tests cover the pactl plumbing; only this can tell you the cable
/// is carrying sound all the way through the DSP chain to the meters.
fn aux_check() -> Result<()> {
    let _hush = Hush(silence_stderr());
    let engine = audio::start(None)?;
    let mut a = adapter::Adapter::insert()?;
    println!("aux input: \"{}\" ({SINK_NOTE})", adapter::DESCRIPTION, SINK_NOTE = adapter::SINK);

    let streams = adapter::streams()?;
    if streams.is_empty() {
        println!("\nnothing is playing. Start something, choose \"{}\"", adapter::DESCRIPTION);
        println!("as its output, and run this again.");
        return Ok(());
    }
    for (i, s) in streams.iter().enumerate() {
        println!("  {}. {}", i + 1, s.label());
    }

    // A diagnostic plugs everything: the question being answered is whether
    // the cable carries at all, and picking one stream risks picking a silent
    // one and blaming the wiring.
    for s in &streams {
        a.plug(s)?;
    }
    println!("\nplugged in all {} stream(s)", streams.len());

    engine.send(EngineCmd::SelectSource { source: SourceKind::Aux, epoch: 1 });
    engine.send(EngineCmd::Play);

    // Hold the highest level seen rather than sampling once: the meter falls
    // between blocks, and a single read can land in a trough and libel a
    // perfectly good cable.
    let mut peak = 0.0f32;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(20));
        peak = peak.max(engine.capture.state.level());
    }

    let bands = engine.meters.read();
    let lit = bands.iter().filter(|v| **v > 0.02).count();
    println!("capture running: {}", engine.capture.state.running());
    // Two separate questions, in order: is anything on the cable, and did it
    // survive the rack. Reporting only the second is what made a capture
    // pointed at the wrong node look like a gain fault.
    if peak > 0.0 {
        println!("input peak:      {:.1} dBFS", 20.0 * peak.log10());
    } else {
        println!("input peak:      nothing at all");
    }
    println!("meters: {:?}", bands.map(|v| (v * 100.0) as u32));

    if peak <= 0.0 {
        println!("\nthe cable is silent — nothing is reaching \"{}\".", adapter::DESCRIPTION);
    } else if lit == 0 {
        println!("\naudio is on the cable but none reached the amplifier —");
        println!("the fault is inside the rack, not in the routing.");
    } else {
        println!("\naudio present in {lit}/9 bands — it went through the whole rack.");
    }
    // Dropping `a` restores the stream and removes the sink.
    Ok(())
}

/// Read the log, or say plainly why there is nothing to read.
///
/// A missing file is not an error: it means REC has never been switched on,
/// and telling the operator that is more use than a path that does not exist.
fn open_log() -> Result<project::Read> {
    let path = listen::Log::path().context("no HOME — the log has nowhere to live")?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // stderr, not stdout: `--export` prints a playlist to stdout, and
            // a prose notice landing above `#EXTM3U` would be read back as a
            // track named after the sentence. A diagnostic on stderr costs
            // `--log` nothing.
            eprintln!("nothing logged yet — press R in the panel to switch REC on");
            eprintln!("the log will be written to {}", path.display());
            return Ok(project::Read { entries: Vec::new(), torn: 0 });
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("read {}", path.display()))),
    };

    let read = project::read(&text);
    // On stderr so it cannot end up inside a playlist being piped to a file.
    if read.torn > 0 {
        eprintln!(
            "{} unreadable line(s) skipped — one at the end is a crash mid-write, \
             several means something else",
            read.torn
        );
    }
    Ok(read)
}

fn list_log() -> Result<()> {
    let log = open_log()?;
    let sessions = project::sessions(&log.entries);
    if sessions.is_empty() {
        return Ok(());
    }
    for s in &sessions {
        println!("{}", project::describe(s));
    }
    let plays: usize = sessions.iter().map(|s| s.entries).sum();
    println!("\n{} session(s), {plays} play(s)", sessions.len());
    println!("cut one out with: ten-qd --export={}", sessions[sessions.len() - 1].id);
    Ok(())
}

fn export(what: &str, rank: bool) -> Result<()> {
    // Parsed before the log is opened: a selector that means nothing should
    // say so rather than emitting a header first.
    let pick = project::Pick::parse(what).map_err(anyhow::Error::msg)?;
    let log = open_log()?;
    let picked = project::select(&log.entries, &pick);
    let cuts = project::cut(&picked, rank);

    // The playlist goes to stdout so it can be redirected; everything about
    // the playlist goes to stderr so that redirect stays clean.
    let missing = cuts.iter().filter(|c| c.uri.is_empty()).count();
    print!("{}", project::m3u(&cuts, &pick));
    eprintln!(
        "{} play(s) → {} track(s){}",
        picked.len(),
        cuts.len() - missing,
        match missing {
            0 => String::new(),
            n => format!(", {n} with no location (listed as comments)"),
        }
    );
    if picked.is_empty() && !log.entries.is_empty() {
        eprintln!("nothing matched — 'ten-qd --log' lists what is there");
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--radio-check") {
        return radio_check();
    }
    if args.iter().any(|a| a == "--aux-check") {
        return aux_check();
    }
    if args.iter().any(|a| a == "--log") {
        return list_log();
    }
    if let Some(what) = args.iter().find_map(|a| {
        (a == "--export").then_some("").or_else(|| a.strip_prefix("--export="))
    }) {
        return export(what, args.iter().any(|a| a == "--rank"));
    }
    if args.iter().any(|a| a == "--forget") {
        return match Memory::forget() {
            Ok(Some(p)) => {
                println!("memory cleared: {}", p.display());
                Ok(())
            }
            Ok(None) => {
                println!("no memory to clear");
                Ok(())
            }
            Err(e) => Err(e.into()),
        };
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let shot = args.iter().any(|a| a == "--screenshot");
    let arg = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);

    // A sink outliving a killed process strands whatever was plugged into it.
    adapter::remove_orphan();

    // librtlsdr prints "[R82XX] PLL not locked!" straight to fd 2 from C every
    // time the tuner is retuned — several times a second during a seek. No
    // Rust-side logging switch can intercept a C library writing to a
    // descriptor, and inside the alternate screen it lands on top of the
    // panel. The descriptor itself is the only place to stop it.
    //
    // Done before the engine opens the radio so the start-up chatter never
    // reaches the terminal either. The diagnostics above return before this
    // point and keep their stderr.
    let saved_stderr = silence_stderr();

    let loaded = Memory::load();

    // Name the sink *before* cpal creates the stream.
    //
    // This is the difference between correcting a feedback loop and never
    // making one. cpal opens the ALSA default, PipeWire places that stream on
    // the system *default sink*, and if the operator has pointed the desktop
    // at "ten-qd aux input" — which is exactly what you do to run everything
    // through the rack — then for as long as it takes us to notice and issue a
    // `move-sink-input`, the rack is driving its own input. That is audible.
    //
    // `PULSE_SINK` is read when the stream is created, so the stream is born
    // on the right device and there is no window at all.
    //
    // SAFETY: single-threaded here. The memory has been read, and nothing has
    // been spawned yet — the engine, the MPRIS watcher and the loop guard all
    // start below this point, so no other thread can be in `getenv`.
    if let Some(s) = adapter::safe_output(loaded.memory.output.as_deref()) {
        unsafe { std::env::set_var("PULSE_SINK", &s.name) };
    }

    // The panel should come up even with no sound card — it says so in the
    // colophon rather than refusing to start.
    let (engine, engine_err) = match audio::start(loaded.memory.output.as_deref()) {
        Ok(e) => (Some(e), None),
        Err(e) => (None, Some(e.to_string())),
    };

    let mut app = App::new(engine, &loaded.memory);
    if let Some(msg) = engine_err {
        app.status(format!("audio engine unavailable: {msg}"));
    }
    app.publish(true);

    // Restore what was in the machine. An explicit path on the command line
    // wins; otherwise the disc that was in the tray last time, and only then
    // a first-run guess at the music directory.
    let remembered_tape = loaded.memory.tape.clone().filter(|p| p.is_dir());
    let start_disc = arg
        .or_else(|| loaded.memory.disc.clone().filter(|p| p.is_dir()))
        .or_else(|| music_dir().and_then(|m| disc::first_disc(&m)));
    match start_disc {
        Some(p) => app.load(p),
        None => app.status("no disc — pass a directory of audio files as an argument"),
    }
    if let Some(p) = remembered_tape {
        app.load_tape_from(&p);
    }

    // Belt and braces behind `PULSE_SINK`: move the stream if it still landed
    // somewhere we did not want. Runs unconditionally, because the case that
    // used to be skipped — no remembered device — is exactly the case where
    // the stream sits on the system default, which may be our own input.
    if let Some(s) = adapter::safe_output(app.stack.output.as_deref()) {
        // The stream has to appear on the graph before it can be moved, and
        // cpal creates it asynchronously — so retry briefly rather than
        // racing it and silently keeping the default output.
        for _ in 0..20 {
            if adapter::route_own_output(&s.name).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Park the radio where it was left. The transport stays stopped: a
    // terminal program that starts making noise on launch is a bad neighbour,
    // whatever the car would have done when you turned the key.
    // Tell the decoder which source the panel came up on.
    //
    // Without this the selector could read TAPE or TUNER while the decoder sat
    // on its own default of Cd — and the first transport press would then cue
    // a track from the *disc*, light the CD transport, and leave the deck
    // reading STOP over music it was not playing.
    let epoch = app.new_epoch();
    let source = app.stack.source;
    if let Some(e) = &app.engine {
        e.send(EngineCmd::SelectSource { source, epoch });
    }

    if let Some(e) = &app.engine {
        e.radio.send(RadioCmd::Tune(app.stack.tuner.freq));
        e.radio.send(RadioCmd::Enable(
            app.stack.source == SourceKind::Tuner && app.stack.tuner.power,
        ));
    }
    if let Some(note) = &loaded.note {
        app.status(note.clone());
    }

    if shot {
        // Give the engine a moment to open the first track so the display has
        // something real to show rather than a blank transport.
        app.dispatch(Command::CdPlayPause);
        if args.iter().any(|a| a == "--browser") {
            app.dispatch(Command::BrowserOpen);
        }
        if args.iter().any(|a| a == "--outputs") {
            app.dispatch(Command::OutputsOpen);
        }
        if args.iter().any(|a| a == "--arrange") {
            app.dispatch(Command::LayoutOpen);
        }
        // Folds the named units away, e.g. --fold=tuner,amp — the arranger is
        // interactive, and a still frame needs some way to reach it.
        // Authoritative, not additive: `--fold=eq` means the equaliser is
        // folded and everything else is open, whatever the memory happens to
        // hold. A screenshot flag that inherits saved state produces a
        // different picture on every machine.
        if let Some(list) = args.iter().find_map(|a| a.strip_prefix("--fold=")) {
            app.stack.layout.collapsed = [false; 7];
            for t in list.split(',') {
                if let Some(u) = Unit::from_token(t.trim()) {
                    app.stack.layout.collapsed[u.index()] = true;
                }
            }
        }
        if args.iter().any(|a| a == "--unfold") {
            app.stack.layout.collapsed = [false; 7];
            app.stack.layout.hidden = [false; 7];
        }
        if let Some(list) = args.iter().find_map(|a| a.strip_prefix("--hide=")) {
            app.stack.layout.hidden = [false; 7];
            for t in list.split(',') {
                if let Some(u) = Unit::from_token(t.trim()) {
                    app.stack.layout.hidden[u.index()] = true;
                }
            }
        }
        // Shows the rack switched to the auxiliary input. The input meter reads empty,
        // because in a still frame nothing is plugged in — seeding it with a
        // plausible level would be the one thing this panel does not do.
        if args.iter().any(|a| a == "--aux") {
            app.dispatch(Command::Source(SourceKind::Aux));
        }
        if args.iter().any(|a| a == "--tuner-off") {
            app.dispatch(Command::TunerPower);
        }
        if let Some(v) = args.iter().find_map(|a| a.strip_prefix("--dim=")) {
            app.stack.ctrl.dimmer = v.parse().unwrap_or(ui::theme::DIM_MAX);
        }
        app.stack.eq.front = [0.0, 3.0, 6.0, 3.0, 0.0, -3.0, 0.0, 6.0, 9.0];
        app.stack.eq.rear = [6.0, 3.0, 0.0, 0.0, -3.0, -6.0, -3.0, 0.0, 3.0];
        app.publish(true);
        std::thread::sleep(Duration::from_millis(700));
        let h = ui::rack_height(&app.stack.layout);
        return screenshot(&mut app, 110, h);
    }

    // Keep the rack's own output off its own input, continuously.
    //
    // Two separate things can put it there. The desktop's "send everything to
    // ten-qd" is one, and it is a perfectly reasonable thing to click. The
    // other is simply restarting while our sink is the system default, because
    // a new stream lands on the default before anything of ours can object.
    // Either way the result is a howl, so this watches rather than assuming.
    {
        let want = app.guard_target.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _ = stop;
        std::thread::Builder::new().name("ten-qd/loopguard".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                if !adapter::own_output_is_looping() {
                    continue;
                }
                let preferred = want.lock().ok().and_then(|g| g.clone());
                if let Some(s) = adapter::safe_output(preferred.as_deref()) {
                    let _ = adapter::route_own_output(&s.name);
                }
            }
        })?;
    }

    let mut keeper = Keeper::new(&loaded);

    let mut terminal = ratatui::init();
    // Mouse capture costs the terminal's native text selection; most emulators
    // still give it back under Shift, and having the panel be clickable is
    // worth that trade for a device that was all buttons.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = run(&mut terminal, &mut app, &mut keeper);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    restore_stderr(saved_stderr);

    // Last write on the way out, so the final few seconds of fiddling are not
    // swallowed by the rate limiter.
    keeper.flush(&app.stack, Some(&app.browser.cwd));
    res
}

/// Point fd 2 at /dev/null, returning the original so it can be put back.
///
/// Blunt, and deliberately so: the noise comes from a C library writing to the
/// descriptor directly, so it cannot be filtered by level or module. Errors
/// this program raises itself go to the colophon, not to stderr, so nothing
/// worth reading is lost — and stderr is restored before exit so a panic
/// message still reaches the terminal.
fn silence_stderr() -> Option<i32> {
    unsafe {
        let saved = libc::dup(libc::STDERR_FILENO);
        if saved < 0 {
            return None;
        }
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
        if null < 0 {
            libc::close(saved);
            return None;
        }
        libc::dup2(null, libc::STDERR_FILENO);
        libc::close(null);
        Some(saved)
    }
}

fn restore_stderr(saved: Option<i32>) {
    if let Some(fd) = saved {
        unsafe {
            libc::dup2(fd, libc::STDERR_FILENO);
            libc::close(fd);
        }
    }
}

/// Silences stderr for as long as it is held.
///
/// The diagnostics print a report, and librtlsdr prints `[R82XX] PLL not
/// locked!` straight to the descriptor in the middle of it. Anything the
/// diagnostics themselves have to say goes to stdout, so nothing readable is
/// lost — and because this restores on drop, an early return cannot leave the
/// terminal muted.
struct Hush(Option<i32>);

impl Drop for Hush {
    fn drop(&mut self) {
        restore_stderr(self.0.take());
    }
}

/// What to say about a tape that has just gone in.
///
/// Names what was dropped as well as what loaded. A tape cut from a week of
/// Spotify and mpv arrives with the service URIs missing, and reporting only
/// the tracks that made it is how half a playlist goes quietly absent.
fn tape_line(tape: &state::Tape, skipped: usize) -> String {
    let head = format!("tape: {} · {} tracks", tape.title, tape.tracks.len());
    match skipped {
        0 => head,
        n => format!("{head} · {n} the deck cannot cue"),
    }
}

fn music_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join("Music");
    p.is_dir().then_some(p)
}

fn panel(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    keeper: &mut Keeper,
) -> Result<()> {
    let mut last = Instant::now();

    while app.running {
        app.poll_engine();
        app.poll_log();

        keeper.maybe_save(&app.stack, Some(&app.browser.cwd));
        if let Some(err) = keeper.error.take() {
            app.status(err);
        }

        let view_h = terminal.size()?.height;
        app.scroll = ui::clamp_scroll(app.scroll as i32, view_h, &app.stack.layout);

        let mut hits = HitMap::new();
        terminal.draw(|f| {
            ui::draw(f, &app.stack, app.scroll, &mut hits);
            let theme = ui::theme::Theme::for_stack(&app.stack);
            if app.browser.open {
                ui::overlay::draw_browser(f, &app.browser, &theme, &mut hits);
            }
            if app.outputs_open {
                ui::overlay::draw_outputs(f, &app.stack, app.outputs_cursor, &theme, &mut hits);
            }
            if app.aux_open {
                ui::overlay::draw_aux(f, &app.streams, app.aux_cursor, &theme, &mut hits);
            }
            if app.layout_open {
                ui::overlay::draw_layout(
                    f,
                    &app.stack,
                    app.layout_cursor,
                    app.layout_grabbed,
                    &theme,
                );
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

/// Run the panel, and close the listening log however it ends.
///
/// The loop propagates terminal errors with `?`, so a flush placed after it
/// runs on a clean quit and on nothing else — and the open entries of a
/// session that died to a resize error are exactly the ones worth keeping.
fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    keeper: &mut Keeper,
) -> Result<()> {
    let played = panel(terminal, app, keeper);
    app.rec_stop();
    played
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tape cut from a week of Spotify and mpv loads with the service URIs
    /// missing. Reporting only the tracks that made it is how half a playlist
    /// goes quietly absent.
    #[test]
    fn a_tape_says_what_it_could_not_cue() {
        let track = |t: &str| state::Track {
            title: t.into(),
            artist: "BoC".into(),
            seconds: 100.0,
            path: PathBuf::from(t),
        };
        let tape = state::Tape::from_tracks(
            "mix".into(),
            PathBuf::from("mix.m3u"),
            vec![track("a"), track("b")],
        );
        assert_eq!(tape_line(&tape, 0), "tape: mix · 2 tracks");
        assert_eq!(tape_line(&tape, 3), "tape: mix · 2 tracks · 3 the deck cannot cue");
    }
}
