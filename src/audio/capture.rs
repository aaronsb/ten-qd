//! Reading the aux input's cable.
//!
//! Whatever is plugged into the auxiliary input arrives here as raw f32
//! stereo and goes into a ring, from which the decoder feeds it to the same
//! DSP chain a disc uses. From the equaliser onward there is no difference
//! between Spotify and a FLAC on disk, which is the entire point of it.
//!
//! Capture is `pw-record --raw`, a subprocess, for the same reason the sink is
//! `pactl`: it is the tool that already exists. `parec` would have been the
//! obvious alternative and was tried first — under pipewire-pulse it exits
//! successfully having written nothing at all, which is a memorable way to
//! spend an afternoon.
//!
//! # Targeting the monitor
//!
//! The target is the **sink**, not `<sink>.monitor`, and the tap is switched on
//! with the `stream.capture.sink` property. That distinction cost a day.
//!
//! `<sink>.monitor` is a PulseAudio-side name. `pactl` lists it as a source and
//! it looks like the obvious thing to record from, but in the PipeWire graph a
//! monitor is not a node — it is a set of monitor *ports* on the sink node
//! itself, and `--target` resolves PipeWire node names. So the name matches
//! nothing, and `pw-record` does not treat that as an error: it falls back to
//! the **default source**, which on a desktop is a microphone.
//!
//! The failure is quiet and deeply misleading. Capture runs, the ring fills,
//! the panel lights up, and the cable carries the room instead of the music —
//! at roughly −45 dBFS, which reads exactly like a gain bug somewhere in the
//! chain. Every volume in the path measures unity, because every volume in the
//! path *is* unity; the audio was simply never coming from the sink.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ringbuf::traits::Producer;
#[cfg(test)]
use ringbuf::traits::Split;

/// How much of the previous block's reading survives into this one, giving the
/// meter a needle's fall rather than a strobe. One block is ~21 ms, so this is
/// about 26 dB per second — peak-meter ballistics, not VU.
const FALL: f32 = 0.94;

/// Shared with the UI so the panel can say whether the cable is actually
/// carrying anything.
pub struct CaptureState {
    running: AtomicBool,
    /// Set when the capture process could not be started at all. Read by the
    /// retry loop rather than the UI: the panel already says "not carrying"
    /// when `running` is false, and why is not something it can act on.
    failed: AtomicBool,
    /// Peak level of the cable, as `f32` bits. Measured here rather than
    /// downstream on purpose: this is the signal as it arrives, before the
    /// ring, before the EQ, and whether or not AUX is the selected source.
    /// A meter further down the chain reads zero when the rack is on CD, which
    /// is exactly when you most want to know the cable is wrong.
    level: AtomicU32,
}

impl CaptureState {
    fn new() -> Self {
        CaptureState {
            running: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            level: AtomicU32::new(0),
        }
    }
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Peak sample magnitude on the cable, 0.0–1.0. Zero when nothing is
    /// being captured at all.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn set_level(&self, v: f32) {
        self.level.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// Peak of a block, folded into the previous reading so the meter falls.
fn fold(previous: f32, block: &[f32]) -> f32 {
    let peak = block.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    // A NaN out of a broken decoder must not stick to the meter forever.
    let decayed = if previous.is_finite() { previous * FALL } else { 0.0 };
    peak.max(decayed).min(1.0)
}

pub struct Capture {
    pub state: Arc<CaptureState>,
    stop: Arc<AtomicBool>,
    /// Whether the sink exists yet. Capture stays idle until it does.
    armed: Arc<AtomicBool>,
}

impl Capture {
    /// Tell capture the sink exists, or has gone away.
    ///
    /// Without this the thread spawned `pw-record` from engine start-up, while
    /// the sink is only created when the operator first opens the aux picker.
    /// An unresolvable `--target` is not an error — see the module docs — and
    /// with `stream.capture.sink` set the fallback is the *default sink's*
    /// monitor. The INPUT meter then showed a real, moving level sourced from
    /// the whole desktop's output, on a meter whose entire job is to make a
    /// cable carrying the wrong thing obvious.
    pub fn arm(&self, on: bool) {
        self.armed.store(on, Ordering::Relaxed);
    }

    /// Start reading the monitor of sink `sink` at `rate`, pushing interleaved
    /// stereo into `prod`. Returns immediately; the reading happens on its own
    /// thread.
    ///
    /// `sink` is the sink's own name — `ten_qd_aux`, not `ten_qd_aux.monitor`.
    /// See the module docs for why that matters.
    pub fn start<P>(sink: &str, rate: u32, mut prod: P) -> Self
    where
        P: Producer<Item = f32> + Send + 'static,
    {
        let state = Arc::new(CaptureState::new());
        let stop = Arc::new(AtomicBool::new(false));
        let armed = Arc::new(AtomicBool::new(false));
        let (t_state, t_stop, src) = (state.clone(), stop.clone(), sink.to_string());
        let t_armed = armed.clone();

        std::thread::Builder::new()
            .name("ten-qd/capture".into())
            .spawn(move || {
                // Retry, because the monitor may not be ready the instant the
                // sink appears, and because a plugged stream ending can take
                // the capture down with it.
                while !t_stop.load(Ordering::Relaxed) {
                    // Nothing to read from until the sink is loaded. Idle
                    // rather than targeting a node that does not exist.
                    if !t_armed.load(Ordering::Relaxed) {
                        t_state.running.store(false, Ordering::Relaxed);
                        t_state.set_level(0.0);
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    match spawn(&src, rate) {
                        Ok(child) => {
                            t_state.failed.store(false, Ordering::Relaxed);
                            pump(child, &mut prod, &t_state, &t_stop, &t_armed);
                        }
                        Err(_) => {
                            t_state.failed.store(true, Ordering::Relaxed);
                            t_state.running.store(false, Ordering::Relaxed);
                        }
                    }
                    if t_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(400));
                }
                t_state.running.store(false, Ordering::Relaxed);
            })
            .ok();

        Capture { state, stop, armed }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The `pw-record` invocation, split out so the one thing that is impossible to
/// notice at runtime can be asserted at test time.
fn args(sink: &str, rate: u32) -> Vec<String> {
    // `stream.capture.sink` is what actually points this at the sink's monitor
    // ports; without it `--target=<sink>` would try to record a sink as though
    // it were a source and land on the default input instead.
    //
    // The rest names the node, so the graph reads correctly in qpwgraph and
    // friends: the aux sink, this capture, and our output should all be
    // identifiable as one device rather than as a stray `pw-record` sitting
    // next to an unrelated `alsa_playback.ten-qd`.
    let props = concat!(
        "{ stream.capture.sink = true ",
        "node.name = \"ten-qd aux capture\" ",
        "media.name = \"aux input\" ",
        "application.name = \"ten-qd\" }",
    );
    [
        "--raw",
        &format!("--target={sink}"),
        &format!("--rate={rate}"),
        "--channels=2",
        "--format=f32",
        "-P",
        props,
        "-",
    ]
    .map(String::from)
    .to_vec()
}

fn spawn(sink: &str, rate: u32) -> std::io::Result<Child> {
    Command::new("pw-record")
        .args(args(sink, rate))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

fn pump<P>(
    mut child: Child,
    prod: &mut P,
    state: &CaptureState,
    stop: &AtomicBool,
    armed: &AtomicBool,
) where
    P: Producer<Item = f32>,
{
    let Some(mut out) = child.stdout.take() else { return };
    // 16 KiB is 1024 stereo frames — about 21 ms at 48 kHz, short enough that
    // stopping feels immediate.
    let mut bytes = [0u8; 16384];
    let mut samples: Vec<f32> = Vec::with_capacity(4096);

    loop {
        if stop.load(Ordering::Relaxed) || !armed.load(Ordering::Relaxed) {
            break;
        }
        match out.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                state.running.store(true, Ordering::Relaxed);
                samples.clear();
                // A short read can split a sample across two reads; the
                // remainder is dropped rather than carried, which costs at
                // most three bytes and keeps this loop free of state.
                for c in bytes[..n].chunks_exact(4) {
                    samples.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
                state.set_level(fold(state.level(), &samples));
                // A full ring means nobody is draining — AUX is not the
                // selected source. Dropping is right for a live input.
                prod.push_slice(&samples);
            }
            Err(_) => break,
        }
    }

    state.running.store(false, Ordering::Relaxed);
    // Nothing is being read any more, so the meter must read nothing rather
    // than hold the last thing it saw.
    state.set_level(0.0);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this guards against is silent: targeting `<sink>.monitor` does
    /// not fail, it records the default input. The adapter then carries the
    /// room, at a level low enough to look like a gain fault somewhere else.
    /// Capture must not target a node that does not exist. `pw-record` treats
    /// an unresolvable target as a cue to record the default device instead, so
    /// running before the sink is loaded meant metering the whole desktop on a
    /// meter built to expose exactly that mistake.
    #[test]
    fn capture_idles_until_it_is_armed() {
        let (prod, _cons) = ringbuf::HeapRb::<f32>::new(64).split();
        let c = Capture::start("no_such_sink_ten_qd_test", 48_000, prod);
        std::thread::sleep(Duration::from_millis(400));
        assert!(!c.state.running(), "capture ran with nothing to target");
        assert_eq!(c.state.level(), 0.0, "an unarmed capture must read nothing");
    }

    #[test]
    fn capture_targets_the_sink_and_asks_for_its_monitor() {
        let a = args(crate::adapter::SINK, 48_000);

        assert!(
            a.contains(&format!("--target={}", crate::adapter::SINK)),
            "must target the sink node itself: {a:?}"
        );
        assert!(
            !a.iter().any(|s| s.contains(".monitor")),
            "`.monitor` is a PulseAudio name; pw-record cannot resolve it: {a:?}"
        );
        assert!(
            a.iter().any(|s| s.contains("stream.capture.sink = true")),
            "without this the target is read as a source: {a:?}"
        );
    }

    #[test]
    fn the_meter_rises_instantly_and_falls_gradually() {
        let hit = fold(0.0, &[0.0, 0.8, -0.9, 0.1]);
        assert_eq!(hit, 0.9, "a peak must show on the block it arrived in");

        // Silence afterwards lets it down rather than dropping it.
        let next = fold(hit, &[0.0; 64]);
        assert!(next < hit && next > hit * 0.5, "fell to {next} from {hit}");

        // And it does reach the bottom rather than hanging just above it.
        let mut v = hit;
        for _ in 0..400 {
            v = fold(v, &[0.0; 64]);
        }
        assert!(v < 1e-6, "meter never settled: {v}");
    }

    #[test]
    fn the_meter_never_reads_past_full_scale_or_sticks_on_a_nan() {
        assert_eq!(fold(0.0, &[3.0, -4.0]), 1.0, "must clamp to full scale");
        assert!(fold(f32::NAN, &[0.5]).is_finite(), "a NaN must not persist");
        assert_eq!(fold(0.5, &[]), 0.5 * FALL, "an empty block still decays");
    }

    #[test]
    fn capture_asks_for_the_format_the_ring_expects() {
        let a = args("s", 44_100);
        assert!(a.contains(&"--format=f32".to_string()), "pump() reads f32: {a:?}");
        assert!(a.contains(&"--channels=2".to_string()), "the ring is interleaved stereo");
        assert!(a.contains(&"--rate=44100".to_string()), "rate must follow the engine");
        assert!(a.contains(&"--raw".to_string()), "a WAV header would land in the ring");
    }
}
