//! The audio engine: decode thread, output callback, analysis thread.
//!
//! ```text
//!   decoder thread ──push──▶ [ring] ──pop──▶ cpal callback ──▶ device
//!    symphonia                                  │  DSP
//!    resample                                   └──push──▶ [ring] ──▶ analysis thread
//!                                                                       9-band FFT
//! ```
//!
//! ## The clock
//!
//! The elapsed-time display runs on frames the *callback delivered*, not on
//! the decoder's read position — the decoder runs ahead by the depth of the
//! ring, so using it would make the display lead the sound. The decoder stamps
//! each track start with the total-frames value at that point (`TrackMark`),
//! and the UI switches the display when the delivered count passes the stamp.
//! That keeps the readout honest through gapless track changes.
//!
//! Anything that discontinuously changes the stream — stop, cue, eject —
//! invalidates those stamps, so it goes through `flush()`: the decoder stops
//! pushing, asks the callback to drain and zero the counter, waits for the
//! acknowledgement, and only then resumes. Producer and consumer are never
//! both touching the ring during a reset.

pub mod analysis;
pub mod capture;
pub mod dsp;
pub mod radio;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Receiver, Sender};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::formats::{SeekMode, SeekTo};
use symphonia::core::units::Time;

use analysis::{Analyzer, Meters, FFT_SIZE};
use dsp::{DspChain, DspParams};

use crate::state::{Disc, Side, SourceKind, Tape, Track};

/// ~250 ms of stereo at 48 kHz. Short enough that a STOP feels immediate,
/// long enough to ride out a scheduling hiccup during a FLAC frame decode.
const RING_FRAMES: usize = 12_288;
const ANALYSIS_HOP: usize = 1024;

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Commands that discontinuously change the stream carry an `epoch`. The UI
/// increments it, the decoder stamps every `TrackMark` with the epoch in force
/// when the mark was made, and the UI discards marks from a superseded epoch.
/// Without this, a mark emitted just before a STOP arrives just after it and
/// the display jumps to a track that is no longer playing.
#[derive(Clone, Debug)]
pub enum EngineCmd {
    SelectSource { source: SourceKind, epoch: u64 },
    Load { disc: Arc<Disc>, start: usize, epoch: u64 },
    LoadTape { tape: Arc<Tape>, epoch: u64 },
    /// Put an adapter in the deck: stop decoding, start passing the cable
    /// through.
    LoadAdapter { epoch: u64 },
    Play,
    Pause,
    Stop { epoch: u64 },
    /// Cue a 0-based track index and begin playing it.
    Cue { index: usize, epoch: u64 },
    Eject { epoch: u64 },
    /// Turn the cassette over.
    Flip { epoch: u64 },
    /// Jump to an absolute position within the current track.
    Seek { seconds: f64, epoch: u64 },
    SetRepeat(bool),
    SetRandom(bool),
    SetAutoReverse(bool),
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// The decoder began pushing track `index` (0-based) at this output-frame
    /// count. The UI flips the display when the delivered count passes it.
    TrackMark {
        epoch: u64,
        source: SourceKind,
        index: usize,
        side: Side,
        at_frame: u64,
    },
    /// Playback ran off the end of the disc.
    Ended,
    /// Something went wrong and the panel should say so rather than pretend.
    Status(String),
}

pub struct Engine {
    pub params: Arc<ArcSwap<DspParams>>,
    pub meters: Arc<Meters>,
    pub events: Receiver<EngineEvent>,
    pub radio: radio::RadioHandle,
    pub capture: capture::Capture,
    cmd_tx: Sender<EngineCmd>,
    /// Output rate, which is what the clock is denominated in.
    pub sample_rate: u32,
    _stream: cpal::Stream,
}

impl Engine {
    pub fn send(&self, c: EngineCmd) {
        // A full command queue means the decoder is wedged; dropping the
        // command is better than blocking the UI thread on it.
        let _ = self.cmd_tx.try_send(c);
    }

    /// Publish a new parameter snapshot to the audio callback.
    pub fn set_params(&self, p: DspParams) {
        self.params.store(Arc::new(p));
    }
}

/// Shared reset handshake between the decoder and the callback.
struct Flush {
    req: AtomicU64,
    ack: AtomicU64,
}

// ---------------------------------------------------------------------------
// Start-up
// ---------------------------------------------------------------------------

pub fn start(preferred: Option<&str>) -> Result<Engine> {
    let host = cpal::default_host();

    // The rack holds its own opinion about where sound goes, rather than
    // following the system default. That is not a preference — it is what
    // makes the adapter safe. Set KDE's output to "ten-qd cassette adapter"
    // and a rack that followed the default would be driving its own input.
    let device = preferred
        .and_then(|want| {
            host.output_devices()
                .ok()
                .and_then(|mut ds| {
                    ds.find(|d: &cpal::Device| {
                        d.description().is_ok_and(|x| x.name() == want)
                    })
                })
        })
        .filter(|d| {
            !d.description().map(|x| x.name().contains(crate::adapter::SINK)).unwrap_or(false)
        })
        .or_else(|| host.default_output_device())
        .ok_or_else(|| anyhow!("no output device"))?;
    let supported = device.default_output_config().context("no default output config")?;

    let sample_rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.into();

    let params = Arc::new(ArcSwap::from_pointee(DspParams {
        sample_rate: sample_rate as f32,
        ..Default::default()
    }));
    let meters = Arc::new(Meters::new());
    let flush = Arc::new(Flush { req: AtomicU64::new(0), ack: AtomicU64::new(0) });

    let (mut prod, mut cons) = HeapRb::<f32>::new(RING_FRAMES * 2).split();
    // The radio produces into its own ring; the decoder drains it. Keeping a
    // single producer on the output ring is what makes the flush handshake
    // sound — two producers could not be paused coherently.
    let (radio_prod, mut radio_cons) = HeapRb::<f32>::new(RING_FRAMES * 2).split();
    let (adapter_prod, mut adapter_cons) = HeapRb::<f32>::new(RING_FRAMES * 2).split();
    let (mut an_prod, mut an_cons) = HeapRb::<f32>::new(FFT_SIZE * 8).split();
    let (cmd_tx, cmd_rx) = bounded::<EngineCmd>(64);
    let (evt_tx, evt_rx) = bounded::<EngineEvent>(256);

    // --- output callback -------------------------------------------------
    let cb_params = params.clone();
    let cb_meters = meters.clone();
    let cb_flush = flush.clone();
    let mut chain = DspChain::new();
    let mut stereo: Vec<f32> = vec![0.0; 4096];
    let mut seen_flush = 0u64;

    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Honour a pending reset before touching the ring.
            let want = cb_flush.req.load(Ordering::Acquire);
            if want != seen_flush {
                while cons.try_pop().is_some() {}
                cb_meters.frames_out.store(0, Ordering::Release);
                seen_flush = want;
                cb_flush.ack.store(want, Ordering::Release);
            }

            chain.sync(&cb_params.load());

            let frames = data.len() / channels.max(1);
            if stereo.len() < frames * 2 {
                stereo.resize(frames * 2, 0.0);
            }
            let want_samples = frames * 2;
            let got = cons.pop_slice(&mut stereo[..want_samples]);
            stereo[got..want_samples].fill(0.0);

            chain.process(&mut stereo[..want_samples]);

            // Only frames we actually had audio for advance the clock, so an
            // underrun shows as the display pausing rather than as drift.
            cb_meters.frames_out.fetch_add((got / 2) as u64, Ordering::Release);

            for (i, out) in data.chunks_mut(channels.max(1)).enumerate() {
                let l = stereo[i * 2];
                let r = stereo[i * 2 + 1];
                match out.len() {
                    0 => {}
                    1 => out[0] = (l + r) * 0.5,
                    _ => {
                        out[0] = l;
                        out[1] = r;
                        out[2..].fill(0.0);
                    }
                }
            }

            // Feed the analyser post-DSP, so the meters show what left the
            // building rather than what came off the disc.
            if an_prod.vacant_len() >= want_samples / 2 {
                for i in 0..frames {
                    let _ = an_prod.try_push((stereo[i * 2] + stereo[i * 2 + 1]) * 0.5);
                }
            }
        },
        move |err| eprintln!("output stream error: {err}"),
        None,
    )?;
    stream.play()?;

    // --- analysis thread -------------------------------------------------
    let an_meters = meters.clone();
    std::thread::Builder::new().name("ten-qd/fft".into()).spawn(move || {
        let mut analyzer = Analyzer::new(sample_rate as f32);
        let mut window = vec![0.0f32; FFT_SIZE];
        let mut filled = 0usize;
        loop {
            let n = an_cons.pop_slice(&mut window[filled..]);
            filled += n;
            if filled < FFT_SIZE {
                if n == 0 {
                    // Nothing playing: let the meters fall instead of freezing.
                    an_meters.decay(0.88);
                    std::thread::sleep(Duration::from_millis(25));
                }
                continue;
            }
            analyzer.process(&window, &an_meters);
            // 50% overlap keeps the meters responsive without re-reading data.
            window.copy_within(ANALYSIS_HOP.., 0);
            filled = FFT_SIZE - ANALYSIS_HOP;
        }
    })?;

    // --- decoder thread --------------------------------------------------
    let dec_flush = flush.clone();
    std::thread::Builder::new().name("ten-qd/decode".into()).spawn(move || {
        decoder_loop(
            cmd_rx,
            evt_tx,
            &mut prod,
            &mut radio_cons,
            &mut adapter_cons,
            dec_flush,
            sample_rate,
        );
    })?;

    // --- radio ------------------------------------------------------------
    let radio = radio::spawn(radio_prod, sample_rate);

    // --- adapter ----------------------------------------------------------
    // Started unconditionally and left running: it retries quietly until the
    // sink exists, so inserting the adapter later needs no coordination.
    let capture = capture::Capture::start(crate::adapter::MONITOR, sample_rate, adapter_prod);

    Ok(Engine {
        params,
        meters,
        events: evt_rx,
        cmd_tx,
        sample_rate,
        radio,
        capture,
        _stream: stream,
    })
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// Four-point, third-order Hermite interpolation.
///
/// The device rate rarely matches the disc's 44.1 kHz — PipeWire usually runs
/// at 48 kHz — so the decoder resamples. Hermite is not a windowed sinc; it
/// trades a little high-frequency accuracy for being cheap and stateless
/// enough to run inline. If this ever needs to be transparent, `rubato` is the
/// drop-in replacement and this is the only function that changes.
#[inline]
fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c = (x1 - xm1) * 0.5;
    let v = x0 - x1;
    let w = c + v;
    let a = w + v + (x2 - x0) * 0.5;
    let b = w + a;
    ((a * t - b) * t + c) * t + x0
}

pub(crate) struct Resampler {
    ratio: f64,
    pos: f64,
    ch: [Vec<f32>; 2],
    passthrough: bool,
}

impl Resampler {
    pub(crate) fn new(in_rate: u32, out_rate: u32) -> Self {
        Resampler {
            ratio: in_rate as f64 / out_rate as f64,
            // Start at 1.0 so the interpolator always has an x[-1] to read.
            pos: 1.0,
            ch: [vec![0.0; 2], vec![0.0; 2]],
            passthrough: in_rate == out_rate,
        }
    }

    pub(crate) fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        for f in input.chunks_exact(2) {
            self.ch[0].push(f[0]);
            self.ch[1].push(f[1]);
        }
        let len = self.ch[0].len();
        while (self.pos as usize) + 2 < len {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            for c in 0..2 {
                let b = &self.ch[c];
                out.push(hermite(b[i - 1], b[i], b[i + 1], b[i + 2], t));
            }
            self.pos += self.ratio;
        }
        // Retain one sample of history behind the read position.
        let drop = (self.pos as usize).saturating_sub(1);
        if drop > 0 {
            for c in 0..2 {
                self.ch[c].drain(..drop);
            }
            self.pos -= drop as f64;
        }
    }
}

/// One open, decoding track.
struct Source {
    reader: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    track_id: u32,
    resampler: Resampler,
    scratch: Vec<f32>,
}

fn open(path: &PathBuf, out_rate: u32) -> Result<(Source, u32)> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let reader = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;

    let track = reader.default_track(TrackType::Audio).ok_or_else(|| anyhow!("no audio track"))?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(ap)) = track.codec_params.as_ref() else {
        return Err(anyhow!("track has no audio codec parameters"));
    };
    let in_rate = ap.sample_rate.unwrap_or(out_rate);

    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(ap, &AudioDecoderOptions::default())?;

    Ok((
        Source {
            reader,
            decoder,
            track_id,
            resampler: Resampler::new(in_rate, out_rate),
            scratch: Vec::with_capacity(8192),
        },
        in_rate,
    ))
}

/// Which program the decoder is currently pulling from.
///
/// One source at a time, one ring producer. The tuner does not decode files —
/// when it is selected the decoder's job is simply to pump the radio's ring
/// into the output ring, so everything downstream (EQ, meters, fader, volume)
/// is identical no matter where the audio came from.
struct DecodeState {
    source: SourceKind,
    disc: Option<Arc<Disc>>,
    tape: Option<Arc<Tape>>,
    /// Playback position within the active program, absolute across tape sides.
    index: usize,
    /// Remembered so switching sources and back returns to where you were.
    cd_index: usize,
    tape_index: usize,
    tape_side: Side,
    auto_reverse: bool,
    /// The tape in the deck is an adapter, so there is nothing to decode —
    /// the audio arrives over the cable.
    tape_is_adapter: bool,
    playing: bool,
    repeat: bool,
    random: bool,
    src: Option<Source>,
    pushed: u64,
    epoch: u64,
    rng: u64,
}

impl DecodeState {
    /// xorshift64* — enough randomness for shuffling a disc, no dependency.
    fn next_random(&mut self, n: usize) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % n.max(1) as u64) as usize
    }

    fn program(&self) -> Option<&[Track]> {
        match self.source {
            SourceKind::Cd => self.disc.as_ref().map(|d| d.tracks.as_slice()),
            SourceKind::Tape => self.tape.as_ref().map(|t| t.tracks.as_slice()),
            SourceKind::Tuner => None,
        }
    }

    /// The track range the transport may move through right now. For a tape
    /// that is the current side; for a disc it is the whole thing.
    fn range(&self) -> std::ops::Range<usize> {
        match (self.source, self.tape.as_ref()) {
            (SourceKind::Tape, Some(t)) => t.side_range(self.tape_side),
            _ => 0..self.program().map_or(0, |p| p.len()),
        }
    }

    fn remember(&mut self) {
        match self.source {
            SourceKind::Cd => self.cd_index = self.index,
            SourceKind::Tape => self.tape_index = self.index,
            SourceKind::Tuner => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decoder_loop(
    cmds: Receiver<EngineCmd>,
    events: Sender<EngineEvent>,
    prod: &mut impl Producer<Item = f32>,
    radio: &mut impl Consumer<Item = f32>,
    adapter: &mut impl Consumer<Item = f32>,
    flush: Arc<Flush>,
    out_rate: u32,
) {
    let mut st = DecodeState {
        source: SourceKind::Cd,
        disc: None,
        tape: None,
        index: 0,
        cd_index: 0,
        tape_index: 0,
        tape_side: Side::A,
        auto_reverse: true,
        tape_is_adapter: false,
        playing: false,
        repeat: false,
        random: false,
        src: None,
        pushed: 0,
        epoch: 0,
        rng: 0x2545_F491_4F6C_DD1D,
    };
    let mut out: Vec<f32> = Vec::with_capacity(16384);
    let mut radio_buf: Vec<f32> = vec![0.0; 8192];

    // Ask the callback to drain and zero the clock, and wait for it to confirm.
    let do_flush = |st: &mut DecodeState| {
        let want = flush.req.fetch_add(1, Ordering::AcqRel) + 1;
        for _ in 0..200 {
            if flush.ack.load(Ordering::Acquire) >= want {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        st.pushed = 0;
    };

    let mark = |st: &DecodeState, events: &Sender<EngineEvent>, index: usize| {
        let _ = events.try_send(EngineEvent::TrackMark {
            epoch: st.epoch,
            source: st.source,
            index,
            side: st.tape_side,
            at_frame: st.pushed,
        });
    };

    loop {
        // Drain pending commands first — controls should feel immediate.
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                EngineCmd::SelectSource { source, epoch } => {
                    st.remember();
                    st.source = source;
                    st.index = match source {
                        SourceKind::Cd => st.cd_index,
                        SourceKind::Tape => st.tape_index,
                        SourceKind::Tuner => 0,
                    };
                    st.src = None;
                    st.playing = source == SourceKind::Tuner;
                    st.epoch = epoch;
                    do_flush(&mut st);
                }
                EngineCmd::Load { disc, start, epoch } => {
                    st.disc = Some(disc);
                    st.cd_index = start;
                    if st.source == SourceKind::Cd {
                        st.index = start;
                        st.src = None;
                        st.playing = false;
                        st.epoch = epoch;
                        do_flush(&mut st);
                    }
                }
                EngineCmd::LoadAdapter { epoch } => {
                    st.tape_is_adapter = true;
                    st.tape = None;
                    st.src = None;
                    st.playing = st.source == SourceKind::Tape;
                    st.epoch = epoch;
                    do_flush(&mut st);
                }
                EngineCmd::LoadTape { tape, epoch } => {
                    st.tape_is_adapter = false;
                    st.tape_side = Side::A;
                    st.tape_index = 0;
                    st.tape = Some(tape);
                    if st.source == SourceKind::Tape {
                        st.index = 0;
                        st.src = None;
                        st.playing = false;
                        st.epoch = epoch;
                        do_flush(&mut st);
                    }
                }
                EngineCmd::Play => {
                    let live = st.source == SourceKind::Tuner
                        || (st.source == SourceKind::Tape && st.tape_is_adapter);
                    if st.program().is_some() || live {
                        st.playing = true;
                    }
                }
                EngineCmd::Pause => st.playing = false,
                EngineCmd::Stop { epoch } => {
                    st.playing = false;
                    st.src = None;
                    st.index = st.range().start;
                    st.remember();
                    st.epoch = epoch;
                    do_flush(&mut st);
                }
                EngineCmd::Cue { index, epoch } => {
                    st.index = index;
                    st.remember();
                    st.src = None;
                    st.playing = true;
                    st.epoch = epoch;
                    do_flush(&mut st);
                }
                EngineCmd::Eject { epoch } => {
                    st.playing = false;
                    st.src = None;
                    match st.source {
                        SourceKind::Cd => {
                            st.disc = None;
                            st.cd_index = 0;
                        }
                        SourceKind::Tape => {
                            st.tape = None;
                            st.tape_index = 0;
                            st.tape_side = Side::A;
                        }
                        SourceKind::Tuner => {}
                    }
                    st.index = 0;
                    st.epoch = epoch;
                    do_flush(&mut st);
                }
                EngineCmd::Flip { epoch } => {
                    // Turning the cassette over cues the first track of the
                    // other side; the counter restarts, as a real one does.
                    st.tape_side = st.tape_side.flip();
                    st.index = st.range().start;
                    st.tape_index = st.index;
                    st.src = None;
                    st.epoch = epoch;
                    do_flush(&mut st);
                }
                EngineCmd::Seek { seconds, epoch } => {
                    if let Some(src) = st.src.as_mut() {
                        let ok = Time::try_from_secs_f64(seconds.max(0.0))
                            .and_then(|time| {
                                src.reader
                                    .seek(
                                        SeekMode::Coarse,
                                        SeekTo::Time { time, track_id: Some(src.track_id) },
                                    )
                                    .ok()
                            })
                            .is_some();
                        if ok {
                            src.decoder.reset();
                            st.epoch = epoch;
                            do_flush(&mut st);
                            mark(&st, &events, st.index);
                        }
                    }
                }
                EngineCmd::SetRepeat(v) => st.repeat = v,
                EngineCmd::SetRandom(v) => st.random = v,
                EngineCmd::SetAutoReverse(v) => st.auto_reverse = v,
            }
        }

        if !st.playing {
            std::thread::sleep(Duration::from_millis(8));
            continue;
        }

        // Back off when the ring is comfortably full — this is what paces the
        // thread; there is no timer anywhere in the decode path.
        if prod.vacant_len() < 4096 {
            std::thread::sleep(Duration::from_millis(4));
            continue;
        }

        // --- live sources: pump their ring straight through ----------------
        // The tuner and the adapter differ only in where the samples came
        // from. Neither decodes, neither seeks, and both drop rather than
        // buffer when the ring is full, because there is nothing to catch up
        // to on a live input.
        let want = radio_buf.len().min(prod.vacant_len()) & !1;
        // `Consumer` is not dyn-compatible, so the pop happens inside the
        // match rather than through a trait object chosen above it.
        let got = match st.source {
            SourceKind::Tuner => Some(radio.pop_slice(&mut radio_buf[..want])),
            SourceKind::Tape if st.tape_is_adapter => {
                Some(adapter.pop_slice(&mut radio_buf[..want]))
            }
            _ => None,
        };
        if let Some(got) = got {
            if got == 0 {
                std::thread::sleep(Duration::from_millis(4));
                continue;
            }
            let n = prod.push_slice(&radio_buf[..got]);
            st.pushed += (n / 2) as u64;
            continue;
        }

        if st.program().is_none() {
            std::thread::sleep(Duration::from_millis(8));
            continue;
        }

        // Open the cued track if we do not have one going.
        if st.src.is_none() {
            let Some(track) = st.program().and_then(|p| p.get(st.index)).cloned() else {
                st.playing = false;
                let _ = events.try_send(EngineEvent::Ended);
                continue;
            };
            match open(&track.path, out_rate) {
                Ok((src, _in_rate)) => {
                    mark(&st, &events, st.index);
                    st.src = Some(src);
                }
                Err(e) => {
                    let _ = events.try_send(EngineEvent::Status(format!(
                        "track {} unreadable: {e}",
                        st.index + 1
                    )));
                    // Skip the bad track rather than stalling the transport.
                    if !advance(&mut st) {
                        st.playing = false;
                        let _ = events.try_send(EngineEvent::Ended);
                    }
                    continue;
                }
            }
        }

        let src = st.src.as_mut().unwrap();
        let packet = match src.reader.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) | Err(_) => {
                // End of track (or an unrecoverable read): roll into the next
                // one. No flush, so the ring stays full and the join is
                // gapless — which is what makes a tape side play as one piece.
                st.src = None;
                if advance(&mut st) {
                    continue;
                }
                st.playing = false;
                let _ = events.try_send(EngineEvent::Ended);
                continue;
            }
        };

        if packet.track_id != src.track_id {
            continue;
        }

        let decoded = match src.decoder.decode(&packet) {
            Ok(d) => d,
            // Decode errors are usually one corrupt packet; skipping it is
            // what a real transport does when it hits a scratch.
            Err(_) => continue,
        };

        src.scratch.clear();
        interleave_stereo(&decoded, &mut src.scratch);

        out.clear();
        src.resampler.process(&src.scratch, &mut out);

        let mut written = 0;
        while written < out.len() {
            let n = prod.push_slice(&out[written..]);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(2));
                // Bail out of the write loop if a command came in, so a STOP
                // during a full ring is not delayed by a whole buffer.
                if !cmds.is_empty() {
                    break;
                }
                continue;
            }
            written += n;
        }
        st.pushed += (written / 2) as u64;
    }
}

/// Move to the next track, honouring repeat, random, sides and auto-reverse.
/// Returns false when the transport has reached the end of what it may play.
fn advance(st: &mut DecodeState) -> bool {
    let range = st.range();
    if range.is_empty() {
        return false;
    }

    if st.random {
        let n = range.len();
        let pick = st.next_random(n);
        st.index = range.start + pick;
        st.remember();
        return true;
    }

    if st.index + 1 < range.end {
        st.index += 1;
        st.remember();
        return true;
    }

    // End of the side, or of the disc.
    if st.source == SourceKind::Tape {
        let flipped = st.tape_side.flip();
        let other = st.tape.as_ref().map(|t| t.side_range(flipped));
        // Auto-reverse turns the tape over once. Coming back to side A only
        // happens on repeat, otherwise the deck stops at the end of side B.
        let may_flip = st.auto_reverse
            && other.as_ref().is_some_and(|r| !r.is_empty())
            && (flipped == Side::B || st.repeat);
        if may_flip {
            st.tape_side = flipped;
            st.index = st.range().start;
            st.remember();
            return true;
        }
    }

    if st.repeat {
        st.index = range.start;
        st.remember();
        return true;
    }
    false
}

/// Flatten any decoded buffer to interleaved stereo f32.
fn interleave_stereo(buf: &GenericAudioBufferRef<'_>, out: &mut Vec<f32>) {
    let channels = buf.spec().channels().count();
    buf.copy_to_vec_interleaved(out);

    match channels {
        0 => out.clear(),
        1 => {
            // Duplicate mono across both output channels.
            let mono = std::mem::take(out);
            out.reserve(mono.len() * 2);
            for s in mono {
                out.push(s);
                out.push(s);
            }
        }
        2 => {}
        n => {
            // Keep the first two channels; a 5.1 source downmix is well past
            // what a 1985 head unit would have done with it.
            let src = std::mem::take(out);
            out.reserve(src.len() / n * 2);
            for frame in src.chunks_exact(n) {
                out.push(frame[0]);
                out.push(frame[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_passthrough_is_exact() {
        let mut r = Resampler::new(48_000, 48_000);
        let input: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn resampler_rate_change_produces_expected_length() {
        // 44.1k -> 48k should produce roughly rate_out/rate_in more frames.
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let block: Vec<f32> = (0..2048).map(|i| ((i / 2) as f32 * 0.01).sin()).collect();
        for _ in 0..10 {
            r.process(&block, &mut out);
        }
        let in_frames = 10 * 1024;
        let expected = in_frames as f64 * 48_000.0 / 44_100.0;
        let got = (out.len() / 2) as f64;
        assert!((got - expected).abs() < 8.0, "got {got}, expected ~{expected}");
    }

    #[test]
    fn resampler_preserves_a_constant() {
        // A DC signal must survive interpolation unchanged — catches sign and
        // coefficient errors in the Hermite kernel.
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let block = vec![0.5f32; 2048];
        for _ in 0..6 {
            r.process(&block, &mut out);
        }
        // Skip the leading zero-history ramp.
        assert!(out[512..].iter().all(|v| (v - 0.5).abs() < 1e-4), "DC not preserved");
    }
}
