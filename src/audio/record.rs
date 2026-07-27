//! AUDIO mode — the deck writing the signal to a file.
//!
//! The other half of [`crate::listen`]. TRACK mode writes down *what* played
//! and can say nothing about a signal with no name; this writes the signal
//! exactly as it was and can say nothing about which parts of it were which.
//! Neither mode pretends to the other's knowledge.
//!
//! ## Where it taps
//!
//! Between `cons.pop_slice` and `chain.process` — before the equaliser, which
//! in this chain also means before the volume:
//!
//! ```text
//!   cons.pop_slice(&mut stereo)   ← here
//!   chain.process(&mut stereo)      tone · 18 EQ biquads · fader · GAIN · clip
//! ```
//!
//! That is how real decks were wired and the reason is practical: **record
//! level is independent of listening level.** Turn the volume to zero and the
//! recording is unaffected — you can record at three in the morning with
//! nothing coming out of the speakers. It also means a curve set for your
//! headphones cannot be baked into the file, where you would hear it a second
//! time on playback through something else.
//!
//! It sits *after* the source multiplexer, so it works identically for CD,
//! tape, tuner and aux without knowing which is selected.
//!
//! ## Three states, not two
//!
//! Idle, armed, running. Arming runs the meters with the tape stationary so
//! the level can be set against real signal before anything is committed —
//! which is what REC PAUSE was for, and what makes REC LEVEL usable at all.
//!
//! ## What it writes
//!
//! WAV, 16-bit PCM, at the output rate. Uncompressed is the honest first
//! answer: it is the signal, with nothing decided about it, and it needs no
//! encoder and no dependency. It is also about 660 MB an hour, which the panel
//! says out loud rather than leaving to be discovered.

use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;

use crate::state::Arm;

/// Two seconds of stereo at 48 kHz. The writer only has to keep up on
/// average; this absorbs a disk that stalls without troubling the callback.
const RING_FRAMES: usize = 96_000;

/// Range of the record-level trim, in dB either side of unity.
pub const LEVEL_LIMIT_DB: i8 = 12;

/// Shared with the audio callback. Everything here is touched on the
/// real-time thread, so it is all lock-free and allocation-free.
struct Shared {
    /// 0 idle, 1 armed, 2 running — `Arm` as an integer the callback can read.
    arm: AtomicU32,
    /// Linear gain, published as bits so the callback needs no lock.
    level: AtomicU32,
    /// Peak of the last block after the level trim, likewise.
    peak: AtomicU32,
    /// Frames actually written, which is what the counter reports.
    frames: AtomicU64,
    /// The writer could not keep up and samples were dropped. A recording with
    /// a hole in it must not be reported as a clean one.
    dropped: AtomicBool,
    /// The file could not be written. Latched, for the same reason.
    failed: AtomicBool,
    /// Which take is current. Bumped every time recording starts.
    ///
    /// The writer cannot close on a state *observation* — "not running, and
    /// the ring is empty" — because a stop and a restart can both happen
    /// while it is still draining a backlog, and it would then append the
    /// second take to the first file. A take needs an identity, not a
    /// deduced boundary.
    generation: AtomicU64,
    /// A file is open. Lets an orderly shutdown wait for the header patch
    /// instead of racing process teardown.
    writing: AtomicBool,
}

/// The handle the panel holds.
pub struct Recorder {
    shared: Arc<Shared>,
    /// Where the current take is going, once one has started.
    file: Arc<Mutex<Option<PathBuf>>>,
    stop: Arc<AtomicBool>,
    rate: u32,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The callback's end of the tap. Lives on the audio thread.
pub struct Tap {
    shared: Arc<Shared>,
    prod: <HeapRb<f32> as Split>::Prod,
}

impl Tap {
    /// Fold one block of the pre-DSP signal into the recording, returning how
    /// many samples were queued for the file.
    ///
    /// The count is what makes "armed writes nothing" observable: the writer
    /// refuses to open a file while armed anyway, so a leak into the ring
    /// would show up only later, as audio prepended to the next take.
    ///
    /// Called from the audio callback, so it allocates nothing, takes no lock
    /// and never blocks. A ring with no room drops the block and says so
    /// rather than stalling the output — a recording with a gap is a fault to
    /// report, and a stuttering rack is a fault you cannot even report.
    pub fn feed(&mut self, block: &[f32]) -> usize {
        // Whole frames only. A dropped block shifts the stream by its length,
        // so an odd one would swap left and right for the rest of the take —
        // silently, with the panel reporting only that there was a gap. The
        // caller preserves parity by construction; this is where that
        // invariant is written down, so it breaks here rather than in the
        // file.
        debug_assert_eq!(block.len() % 2, 0, "the tap takes whole stereo frames");
        let arm = self.shared.arm.load(Ordering::Relaxed);
        if arm == 0 {
            return 0;
        }
        let level = f32::from_bits(self.shared.level.load(Ordering::Relaxed));

        let mut peak = 0.0f32;
        for s in block {
            peak = peak.max((s * level).abs());
        }
        self.shared.peak.store(peak.to_bits(), Ordering::Relaxed);

        // Armed means meters live and tape stationary: everything above still
        // happens, and nothing below does.
        if arm != 2 {
            return 0;
        }
        if self.prod.vacant_len() < block.len() {
            self.shared.dropped.store(true, Ordering::Relaxed);
            return 0;
        }
        for s in block {
            let _ = self.prod.try_push(s * level);
        }
        block.len()
    }
}

impl Recorder {
    /// Start the writer thread and hand back the callback's tap.
    pub fn start(rate: u32) -> (Recorder, Tap) {
        let shared = Arc::new(Shared {
            arm: AtomicU32::new(0),
            level: AtomicU32::new(1.0f32.to_bits()),
            peak: AtomicU32::new(0),
            frames: AtomicU64::new(0),
            dropped: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            writing: AtomicBool::new(false),
        });
        let file: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (prod, cons) = HeapRb::<f32>::new(RING_FRAMES * 2).split();

        let (t_shared, t_file, t_stop) = (shared.clone(), file.clone(), stop.clone());
        std::thread::Builder::new()
            .name("ten-qd/record".into())
            .spawn(move || writer(t_shared, t_file, t_stop, cons, rate))
            .ok();

        (Recorder { shared: shared.clone(), file, stop, rate }, Tap { shared, prod })
    }

    pub fn arm(&self) -> Arm {
        match self.shared.arm.load(Ordering::Relaxed) {
            1 => Arm::Armed,
            2 => Arm::Running,
            _ => Arm::Idle,
        }
    }

    /// Move to a new state. Going to `Running` starts a take; leaving it
    /// closes one.
    pub fn set_arm(&self, arm: Arm) {
        if arm == self.arm() {
            return;
        }
        if arm == Arm::Running {
            // Cleared per take, not per session: a hole in the *last* take is
            // not a fault of this one.
            self.shared.dropped.store(false, Ordering::Relaxed);
            self.shared.failed.store(false, Ordering::Relaxed);
            self.shared.frames.store(0, Ordering::Relaxed);
            self.shared.generation.fetch_add(1, Ordering::Relaxed);
        }
        self.shared.arm.store(
            match arm {
                Arm::Idle => 0,
                Arm::Armed => 1,
                Arm::Running => 2,
            },
            Ordering::Release,
        );
    }

    /// Record level, in dB either side of unity. Its own gain stage: the
    /// equaliser's GAIN is downstream of the tap and cannot serve.
    pub fn set_level_db(&self, db: i8) {
        let db = db.clamp(-LEVEL_LIMIT_DB, LEVEL_LIMIT_DB);
        let g = 10f32.powf(db as f32 / 20.0);
        self.shared.level.store(g.to_bits(), Ordering::Relaxed);
    }

    /// Peak of the last block, 0..=1, after the level trim — so the meter
    /// shows what is being written rather than what arrived.
    pub fn peak(&self) -> f32 {
        let p = f32::from_bits(self.shared.peak.load(Ordering::Relaxed));
        if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.0 }
    }

    /// Seconds committed to the file. Frames written, not frames offered.
    pub fn seconds(&self) -> f64 {
        self.shared.frames.load(Ordering::Relaxed) as f64 / self.rate.max(1) as f64
    }

    /// Bytes on disk so far — the honest cost of recording uncompressed.
    pub fn bytes(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed) * 4
    }

    pub fn dropped(&self) -> bool {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Stop recording and wait for the file to be closed properly.
    ///
    /// The header carries two sizes that are only known once the take ends, so
    /// a process that exits without letting the writer patch them leaves a WAV
    /// claiming zero length. Players mostly cope by reading to end of file;
    /// "mostly" is not a good enough reason to leave a 600 MB artefact
    /// malformed. Bounded, because a wedged disk must not stop the rack
    /// quitting.
    pub fn finish_take(&self, wait: Duration) {
        self.set_arm(Arm::Idle);
        let until = std::time::Instant::now() + wait;
        while self.shared.writing.load(Ordering::Acquire) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }

    pub fn file(&self) -> Option<PathBuf> {
        self.file.lock().ok().and_then(|g| g.clone())
    }

    /// Where takes are written.
    pub fn dir() -> Option<PathBuf> {
        Some(crate::memory::state_dir()?.join("recordings"))
    }
}

/// Drains the ring into a WAV file, one file per take.
fn writer(
    shared: Arc<Shared>,
    file: Arc<Mutex<Option<PathBuf>>>,
    stop: Arc<AtomicBool>,
    mut cons: <HeapRb<f32> as Split>::Cons,
    rate: u32,
) {
    let mut open: Option<Wav> = None;
    let mut buf = vec![0.0f32; 8192];

    while !stop.load(Ordering::Relaxed) {
        let arm = shared.arm.load(Ordering::Acquire);
        let generation = shared.generation.load(Ordering::Relaxed);
        let running = arm == 2;

        // A take is over when its generation is superseded, or when recording
        // has stopped and the ring has given up everything it held. The first
        // is what makes stop-then-start always two files: without it, a
        // restart during a long drain — a stalled disk, or REC PAUSE and
        // resume — appends the new take to the old file.
        let superseded = open.as_ref().is_some_and(|w| w.generation != generation);
        let drained = !running && cons.is_empty();
        if (superseded || drained)
            && let Some(w) = open.take()
        {
            shared.writing.store(false, Ordering::Release);
            if w.finish().is_err() {
                shared.failed.store(true, Ordering::Relaxed);
            }
        }

        if running && open.is_none() {
            match Wav::create(rate, generation) {
                Ok(w) => {
                    if let Ok(mut g) = file.lock() {
                        *g = Some(w.path.clone());
                    }
                    shared.writing.store(true, Ordering::Release);
                    open = Some(w);
                }
                Err(_) => {
                    shared.failed.store(true, Ordering::Relaxed);
                    // Do not spin on a directory that cannot be written.
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }

        let n = cons.pop_slice(&mut buf);
        if let Some(w) = open.as_mut() {
            if n > 0 && w.write(&buf[..n]).is_err() {
                shared.failed.store(true, Ordering::Relaxed);
            }
            // Only for the take this file belongs to. A superseded generation
            // is closed above before it can overwrite the new take's count.
            if w.generation == generation {
                shared.frames.store(w.frames(), Ordering::Relaxed);
            }
        }

        if n == 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    if let Some(w) = open.take() {
        shared.writing.store(false, Ordering::Release);
        let _ = w.finish();
    }
}

// ---------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------

/// A WAV file being written.
///
/// The two sizes in a WAV header are only known once the recording ends, so
/// they are written as placeholders and patched on close. A take that dies
/// with the program therefore has a header claiming zero — which every player
/// copes with by reading to end of file, and which `finish` corrects whenever
/// it gets the chance.
struct Wav {
    file: std::fs::File,
    path: PathBuf,
    /// Samples written, not frames.
    ///
    /// The tap pushes one sample at a time, so a read from the ring can land
    /// between a left and its right — and counting `len() / 2` per read threw
    /// away half a frame on every odd one. Over a minute that is hundreds of
    /// frames missing from the header while the audio itself is complete: a
    /// file whose length says one thing and whose contents say another.
    /// Samples add up exactly; frames are derived once, at the end.
    samples: u64,
    rate: u32,
    /// Which take this file belongs to.
    generation: u64,
}

impl Wav {
    /// Open a new take.
    ///
    /// `create_new`, never `create`. Take names are second-resolution, so two
    /// takes in the same wall-clock second — a double-press, entirely
    /// ordinary — would otherwise land on the same path, and `File::create`
    /// truncates: the first recording would be destroyed silently, the open
    /// having *succeeded*. Colliding names take a suffix, and a name that
    /// cannot be found at all fails loudly rather than overwriting anything.
    fn create(rate: u32, generation: u64) -> std::io::Result<Wav> {
        let dir = Recorder::dir()
            .ok_or_else(|| std::io::Error::other("no HOME — nowhere to write a recording"))?;
        std::fs::create_dir_all(&dir)?;

        let stem = crate::listen::session_id(SystemTime::now());
        for n in 1..100 {
            let name =
                if n == 1 { format!("{stem}.wav") } else { format!("{stem}-{n}.wav") };
            let path = dir.join(name);
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(&header(rate, 0))?;
                    return Ok(Wav { file, path, samples: 0, rate, generation });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other("a hundred takes in one second — something is wrong"))
    }

    fn write(&mut self, block: &[f32]) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(block.len() * 2);
        for s in block {
            // Clamped, not wrapped: a sample over full scale is a level set
            // too hot, and it should sound like clipping rather than like the
            // waveform folding inside out.
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
        self.file.write_all(&out)?;
        self.samples += block.len() as u64;
        Ok(())
    }

    /// Patch the two sizes that were only knowable once the take ended.
    ///
    /// The rate is carried rather than read back out of the header: the file
    /// is opened write-only, and reading it would need a second descriptor
    /// for a number we already have.
    fn frames(&self) -> u64 {
        self.samples / 2
    }

    fn finish(mut self) -> std::io::Result<()> {
        let (rate, frames) = (self.rate, self.frames());
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header(rate, frames))?;
        self.file.flush()
    }
}

/// A 44-byte canonical WAV header: 16-bit PCM, two channels.
fn header(rate: u32, frames: u64) -> Vec<u8> {
    let data = (frames * 4).min(u32::MAX as u64 - 36) as u32;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&2u16.to_le_bytes()); // channels
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * 4).to_le_bytes()); // bytes per second
    h.extend_from_slice(&4u16.to_le_bytes()); // block align
    h.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data.to_le_bytes());
    h
}

/// How much disk a take has taken, phrased the way a person would.
pub fn size(bytes: u64) -> String {
    match bytes {
        b if b < 1_000_000 => format!("{} kB", b / 1000),
        b if b < 1_000_000_000 => format!("{} MB", b / 1_000_000),
        b => format!("{:.1} GB", b as f64 / 1e9),
    }
}

/// Read a WAV back far enough to check it. Used by the tests, and cheap
/// enough to be worth having.
#[cfg(test)]
fn probe(path: &std::path::Path) -> std::io::Result<(u32, u16, u32)> {
    let b = std::fs::read(path)?;
    let at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    Ok((at(24), u16::from_le_bytes([b[22], b[23]]), at(40)))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let d = std::env::temp_dir()
                .join(format!("ten-qd-rec-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&d).expect("scratch");
            Scratch(d)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_header_describes_sixteen_bit_stereo_at_the_output_rate() {
        let h = header(48_000, 0);
        assert_eq!(h.len(), 44);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([h[22], h[23]]), 2, "channels");
        assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), 48_000);
        assert_eq!(u16::from_le_bytes([h[34], h[35]]), 16, "bits");
        // Byte rate must agree with the format, or the file plays at the
        // wrong speed in anything that trusts it.
        assert_eq!(u32::from_le_bytes([h[28], h[29], h[30], h[31]]), 48_000 * 4);
    }

    #[test]
    fn the_sizes_are_patched_when_the_take_ends() {
        let s = Scratch::new("sizes");
        let path = s.0.join("take.wav");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&header(44_100, 0)).unwrap();
        let mut w = Wav { file: f, path: path.clone(), samples: 0, rate: 44_100, generation: 1 };

        w.write(&[0.0; 200]).unwrap();
        assert_eq!(w.frames(), 100, "two samples make one stereo frame");
        w.finish().unwrap();

        let (rate, channels, data) = probe(&path).unwrap();
        assert_eq!(rate, 44_100, "the rate survives the patch");
        assert_eq!(channels, 2);
        assert_eq!(data, 400, "100 frames x 2 channels x 2 bytes");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 444);
    }

    /// The tap pushes one sample at a time, so a read from the ring can land
    /// between a left and its right. Counting `len() / 2` per read threw away
    /// the half-frame every time, and a minute of recording ended up with a
    /// header hundreds of frames short of its own contents.
    #[test]
    fn a_read_that_splits_a_frame_still_adds_up() {
        let s = Scratch::new("odd");
        let path = s.0.join("take.wav");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&header(48_000, 0)).unwrap();
        let mut w = Wav { file: f, path: path.clone(), samples: 0, rate: 48_000, generation: 1 };

        // Four reads, two of them odd, carrying five whole frames between them.
        for n in [3usize, 1, 5, 1] {
            w.write(&vec![0.0; n]).unwrap();
        }
        assert_eq!(w.frames(), 5, "ten samples are five frames however they arrive");
        w.finish().unwrap();

        let (_, _, data) = probe(&path).unwrap();
        assert_eq!(data, 20, "and the header must agree with the bytes on disk");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64);
    }

    /// Rust's float-to-int casts saturate, so nothing here can fold a
    /// waveform inside out. What the clamp buys is symmetry: without it a
    /// hot negative sample saturates to −32768, one step past the +32767 a
    /// hot positive one reaches, and the two rails stop matching.
    #[test]
    fn a_sample_over_full_scale_clips_to_matching_rails() {
        let s = Scratch::new("clip");
        let path = s.0.join("take.wav");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&header(48_000, 0)).unwrap();
        let mut w = Wav { file: f, path: path.clone(), samples: 0, rate: 48_000, generation: 1 };
        w.write(&[2.0, -2.0]).unwrap();
        w.finish().unwrap();

        let b = std::fs::read(&path).unwrap();
        let l = i16::from_le_bytes([b[44], b[45]]);
        let r = i16::from_le_bytes([b[46], b[47]]);
        assert_eq!(l, i16::MAX);
        assert_eq!(r, -i16::MAX, "the negative rail must match the positive one");
    }

    /// Armed means meters live and tape stationary. Asserting only that no
    /// file appears is too weak — the writer refuses to open one while armed
    /// regardless, so the tap could leak into the ring and the test would
    /// still pass. What it would leak into is the *next* take, prepending
    /// audio the operator never recorded, so the sample count is the thing to
    /// watch.
    #[test]
    fn arming_meters_without_writing_anything() {
        let (rec, mut tap) = Recorder::start(48_000);
        rec.set_arm(Arm::Armed);
        let queued: usize = (0..64).map(|_| tap.feed(&[0.5, -0.5])).sum();
        assert!(rec.peak() > 0.4, "armed must meter: {}", rec.peak());
        assert_eq!(queued, 0, "armed must offer the writer nothing");
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(rec.seconds(), 0.0, "and must not commit a frame");
        assert!(rec.file().is_none(), "nor open a file");
    }

    #[test]
    fn idle_meters_nothing_at_all() {
        let (rec, mut tap) = Recorder::start(48_000);
        assert_eq!(tap.feed(&[1.0, 1.0]), 0);
        assert_eq!(rec.peak(), 0.0, "an idle deck is not measuring anything");
    }

    #[test]
    fn the_record_level_is_its_own_gain_stage() {
        let (rec, mut tap) = Recorder::start(48_000);
        rec.set_arm(Arm::Armed);

        rec.set_level_db(0);
        tap.feed(&[0.5, 0.5]);
        let unity = rec.peak();

        rec.set_level_db(6);
        tap.feed(&[0.5, 0.5]);
        let up = rec.peak();

        let db = 20.0 * (up / unity).log10();
        assert!((db - 6.0).abs() < 0.1, "+6 dB read as {db:.2} dB");
    }

    /// Measured with a signal quiet enough that +12 dB does not reach full
    /// scale. At 0.5 the meter saturates at the limit *and* far past it, so
    /// the readings match either way and the meter's own ceiling hides
    /// whether the control clamped at all.
    #[test]
    fn the_level_cannot_be_driven_past_its_limit() {
        let (rec, mut tap) = Recorder::start(48_000);
        rec.set_arm(Arm::Armed);
        rec.set_level_db(120);
        tap.feed(&[0.1, 0.1]);
        let clamped = rec.peak();
        assert!(clamped < 0.99, "the probe must not saturate the meter: {clamped}");

        rec.set_level_db(LEVEL_LIMIT_DB);
        tap.feed(&[0.1, 0.1]);
        assert_eq!(clamped, rec.peak(), "beyond the limit is the limit");
    }

    /// The meter is a readout of what is being written, so it has to sit after
    /// the level — otherwise it would show a healthy signal while the file
    /// took a clipped one.
    #[test]
    fn the_meter_reads_after_the_level_not_before() {
        let (rec, mut tap) = Recorder::start(48_000);
        rec.set_arm(Arm::Armed);
        rec.set_level_db(LEVEL_LIMIT_DB);
        tap.feed(&[0.5, 0.5]);
        assert!(rec.peak() > 0.9, "a level pushing into clip must show it");
    }

    #[test]
    fn a_take_reports_its_own_size_in_something_readable() {
        assert_eq!(size(12_000), "12 kB");
        assert_eq!(size(5_400_000), "5 MB");
        assert_eq!(size(2_300_000_000), "2.3 GB");
    }

    /// Take names are second-resolution, so a double-press lands two takes on
    /// one path. `File::create` would truncate — destroying a recording with
    /// the open *succeeding*, so nothing would latch and the panel would
    /// report a healthy new take over the corpse of the old one.
    #[test]
    fn a_second_take_in_the_same_second_never_overwrites_the_first() {
        let s = Scratch::new("collide");
        unsafe { std::env::set_var("XDG_STATE_HOME", &s.0) };

        let first = Wav::create(48_000, 1).expect("first take");
        std::fs::write(&first.path, b"the first recording").unwrap();
        let second = Wav::create(48_000, 2).expect("second take");

        assert_ne!(first.path, second.path, "two takes must not share a path");
        assert_eq!(
            std::fs::read(&first.path).unwrap(),
            b"the first recording",
            "the first take must survive the second starting"
        );
    }

    /// A take is over when its generation is superseded, not when the writer
    /// happens to observe an idle arm with an empty ring. Stop and restart
    /// during a long drain — a stalled disk, or REC PAUSE and resume — and
    /// the deduced boundary never arrives, so the second take appends to the
    /// first file.
    #[test]
    fn stopping_and_starting_always_makes_two_takes() {
        let s = Scratch::new("gen");
        unsafe { std::env::set_var("XDG_STATE_HOME", &s.0) };
        let (rec, mut tap) = Recorder::start(48_000);

        rec.set_arm(Arm::Running);
        tap.feed(&[0.25; 512]);
        std::thread::sleep(Duration::from_millis(60));
        let one = rec.file().expect("a first take");

        // Straight back to Running, exactly as a pause-and-resume would.
        rec.set_arm(Arm::Idle);
        rec.set_arm(Arm::Running);
        tap.feed(&[0.25; 512]);
        std::thread::sleep(Duration::from_millis(80));
        let two = rec.file().expect("a second take");

        rec.finish_take(Duration::from_millis(500));
        assert_ne!(one, two, "a restart must open its own file, not extend the last");
    }

    /// The header carries two sizes only known once the take ends. Exiting
    /// without letting the writer patch them leaves a WAV claiming zero.
    #[test]
    fn finishing_a_take_waits_for_the_header_to_be_patched() {
        let s = Scratch::new("finish");
        unsafe { std::env::set_var("XDG_STATE_HOME", &s.0) };
        let (rec, mut tap) = Recorder::start(48_000);

        rec.set_arm(Arm::Running);
        for _ in 0..8 {
            tap.feed(&[0.25; 1024]);
        }
        rec.finish_take(Duration::from_millis(1000));

        let path = rec.file().expect("a take");
        let (_, _, data) = probe(&path).expect("read it back");
        let on_disk = std::fs::metadata(&path).unwrap().len();
        assert_eq!(
            data as u64 + 44,
            on_disk,
            "the header must describe the file it is at the head of"
        );
        assert!(data > 0, "a patched header must not still claim zero");
    }

    #[test]
    fn a_recorder_starts_and_stops_without_ever_recording() {
        let (rec, _tap) = Recorder::start(48_000);
        assert_eq!(rec.arm(), Arm::Idle);
        assert!(!rec.failed());
        assert!(!rec.dropped());
        // Dropping must not hang waiting on the writer thread.
        drop(rec);
    }
}
