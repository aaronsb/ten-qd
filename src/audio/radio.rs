//! LT-581 tuner — broadcast FM off an RTL-SDR.
//!
//! ```text
//!   RTL2832 ─▶ u8 IQ @ 1.024 MHz ─▶ FIR ÷4 ─▶ 256 kHz ─▶ discriminator ─▶ MPX
//!                                                                          │
//!            ┌─────────────────────────────────────────────────────────────┤
//!            ▼                                                             ▼
//!     19 kHz pilot ─▶ ×² ─▶ 38 kHz ─▶ × MPX ─▶ LPF ─▶ (L−R)      LPF 15 kHz ─▶ (L+R)
//!                                                   └──────┬──────────────────┘
//!                                                          ▼
//!                                    de-emphasis 75 µs ─▶ L, R ─▶ resample ─▶ ring
//! ```
//!
//! The demodulation is done here rather than by shelling out to `rtl_fm`
//! because two of the panel's indicators depend on it being ours: the signal
//! meter reads the mean IQ magnitude, and the STEREO lamp lights only when a
//! 19 kHz pilot is actually present. Piping mono audio in from a subprocess
//! would leave both of those as decoration, which is the one thing this build
//! does not do.
//!
//! AM is not offered. The R820T front end starts around 24 MHz, so the AM
//! broadcast band is out of reach without a direct-sampling modification —
//! and a band button that tuned nothing would be a lie.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use ringbuf::traits::Producer;

use crate::state::{FM_HI, FM_LO, FM_STEP};

/// IQ rate off the dongle. High enough that decimating by four still leaves
/// comfortable room around the 200 kHz FM channel.
const IQ_RATE: u32 = 1_024_000;
const DECIM: usize = 4;
/// MPX rate after decimation — must clear 53 kHz for the stereo subcarrier.
const MPX_RATE: f32 = (IQ_RATE / DECIM as u32) as f32;
/// Peak deviation of broadcast FM, used to normalise the discriminator.
const DEVIATION: f32 = 75_000.0;
const PILOT_HZ: f32 = 19_000.0;
const AUDIO_HZ: f32 = 15_000.0;
/// 75 µs, the de-emphasis constant used in the Americas. Europe uses 50 µs.
const DEEMPHASIS_S: f32 = 75e-6;
/// How much stronger the 19 kHz band must be than an empty guard band before
/// we call it a pilot. A tone concentrates; noise spreads — so a ratio
/// separates them where an absolute threshold cannot.
const PILOT_RATIO: f32 = 6.0;
/// Guard band, chosen to sit in the gap between the mono audio (≤15 kHz) and
/// the pilot, where a correctly-modulated signal puts nothing.
const GUARD_HZ: f32 = 16_600.0;

/// Fixed tuner gain, in tenths of a dB.
///
/// Not AGC: automatic gain normalises the IQ magnitude, which is precisely the
/// quantity the signal meter reads. With the AGC on, every channel in the band
/// measures the same — the meter would be reporting the gain loop, not the
/// station. A fixed gain costs some headroom and buys a meter that means
/// something.
///
/// 16.6 dB rather than the ~30 dB that first seemed reasonable: at high gain a
/// strong local transmitter saturates the front end, and the resulting
/// distortion spreads energy into the pilot guard band. The symptom was the
/// two strongest stations in the band being the only ones that failed to
/// report stereo.
const TUNER_GAIN: i32 = 166;

/// Channel-power window the meter spans, in dBFS. Calibrated by sweeping the
/// band with `--radio-check`: empty channels sit near the floor, a strong
/// local transmitter near the ceiling.
const FLOOR_DBFS: f32 = -44.0;
const CEIL_DBFS: f32 = -18.0;

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum RadioCmd {
    Tune(f64),
    /// Scan up or down for the next station above the squelch threshold.
    Seek(i32, bool),
    /// Whether the tuner is the selected source. When it is not, the radio
    /// stops demodulating rather than filling a ring nobody drains.
    Enable(bool),
}

pub struct RadioShared {
    rssi: AtomicU32,
    /// Raw channel power in dBFS, kept alongside the 0..1 meter value so the
    /// mapping can be checked against reality rather than assumed.
    rssi_db: AtomicU32,
    stereo: AtomicBool,
    freq: AtomicU32,
    seeking: AtomicBool,
    enabled: AtomicBool,
    /// Set once the device has been probed. `None` until then.
    device: std::sync::Mutex<Option<Result<String, String>>>,
}

impl RadioShared {
    fn new() -> Self {
        RadioShared {
            rssi: AtomicU32::new(0),
            rssi_db: AtomicU32::new(f32::NEG_INFINITY.to_bits()),
            stereo: AtomicBool::new(false),
            freq: AtomicU32::new(88_500),
            seeking: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            device: std::sync::Mutex::new(None),
        }
    }

    pub fn rssi(&self) -> f32 {
        f32::from_bits(self.rssi.load(Ordering::Relaxed))
    }
    pub fn rssi_db(&self) -> f32 {
        f32::from_bits(self.rssi_db.load(Ordering::Relaxed))
    }
    pub fn stereo(&self) -> bool {
        self.stereo.load(Ordering::Relaxed)
    }
    /// Frequency in MHz.
    pub fn freq(&self) -> f64 {
        self.freq.load(Ordering::Relaxed) as f64 / 1000.0
    }
    pub fn seeking(&self) -> bool {
        self.seeking.load(Ordering::Relaxed)
    }
    /// `Ok(name)` once open, `Err(why)` if the radio could not be used.
    pub fn device(&self) -> Option<Result<String, String>> {
        self.device.lock().ok().and_then(|g| g.clone())
    }
}

pub struct RadioHandle {
    pub shared: Arc<RadioShared>,
    tx: Sender<RadioCmd>,
}

impl RadioHandle {
    pub fn send(&self, c: RadioCmd) {
        let _ = self.tx.try_send(c);
    }
}

// ---------------------------------------------------------------------------
// Signal processing
// ---------------------------------------------------------------------------

/// Windowed-sinc low-pass, evaluated only at the samples we keep.
struct FirDecimator {
    taps: Vec<f32>,
    hist_i: Vec<f32>,
    hist_q: Vec<f32>,
    pos: usize,
    phase: usize,
}

impl FirDecimator {
    fn new(len: usize, cutoff: f32) -> Self {
        // `cutoff` in cycles per sample. Hamming window keeps the stopband
        // around -53 dB, which is enough to reject the neighbouring channel.
        let m = len - 1;
        let mut taps: Vec<f32> = (0..len)
            .map(|n| {
                let x = n as f32 - m as f32 / 2.0;
                let sinc = if x.abs() < 1e-6 {
                    2.0 * cutoff
                } else {
                    (std::f32::consts::TAU * cutoff * x).sin() / (std::f32::consts::PI * x)
                };
                let w = 0.54 - 0.46 * (std::f32::consts::TAU * n as f32 / m as f32).cos();
                sinc * w
            })
            .collect();
        let sum: f32 = taps.iter().sum();
        for t in &mut taps {
            *t /= sum;
        }
        FirDecimator {
            hist_i: vec![0.0; len],
            hist_q: vec![0.0; len],
            taps,
            pos: 0,
            phase: 0,
        }
    }

    /// Push one input sample; return the filtered output when this sample
    /// lands on a decimation boundary.
    #[inline]
    fn push(&mut self, i: f32, q: f32) -> Option<(f32, f32)> {
        let n = self.taps.len();
        self.hist_i[self.pos] = i;
        self.hist_q[self.pos] = q;
        self.pos = (self.pos + 1) % n;

        self.phase += 1;
        if self.phase < DECIM {
            return None;
        }
        self.phase = 0;

        let (mut si, mut sq) = (0.0, 0.0);
        let mut idx = self.pos;
        for &t in &self.taps {
            si += self.hist_i[idx] * t;
            sq += self.hist_q[idx] * t;
            idx = (idx + 1) % n;
        }
        Some((si, sq))
    }
}

/// Direct-form-II biquad, private to the radio so the equaliser's filter type
/// stays about equalising.
#[derive(Clone, Copy, Default)]
struct Bq {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Bq {
    fn bandpass(fs: f32, f0: f32, q: f32) -> Self {
        let w0 = std::f32::consts::TAU * f0 / fs;
        let (s, c) = w0.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        Bq {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
            ..Default::default()
        }
    }

    fn lowpass(fs: f32, f0: f32, q: f32) -> Self {
        let w0 = std::f32::consts::TAU * f0 / fs;
        let (s, c) = w0.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = (1.0 - c) / 2.0;
        Bq {
            b0: b / a0,
            b1: (1.0 - c) / a0,
            b2: b / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
            ..Default::default()
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Everything that turns IQ into stereo audio. Lives on the reader thread.
struct Demod {
    decim: FirDecimator,
    last_i: f32,
    last_q: f32,
    /// Two cascaded sections each for a steeper skirt at 15 kHz.
    lpf_sum: [Bq; 2],
    lpf_diff: [Bq; 2],
    pilot_bp: Bq,
    guard_bp: Bq,
    sub_bp: Bq,
    deemph: [f32; 2],
    deemph_a: f32,
    pilot_pow: f32,
    guard_pow: f32,
    mag_acc: f64,
    mag_n: u64,
}

impl Demod {
    fn new() -> Self {
        let fs = MPX_RATE;
        // Cutoff as a fraction of the *input* rate: 100 kHz of 1.024 MHz.
        let decim = FirDecimator::new(31, 100_000.0 / IQ_RATE as f32);
        let dt = 1.0 / fs;
        Demod {
            decim,
            last_i: 0.0,
            last_q: 0.0,
            lpf_sum: [Bq::lowpass(fs, AUDIO_HZ, 0.54), Bq::lowpass(fs, AUDIO_HZ, 1.31)],
            lpf_diff: [Bq::lowpass(fs, AUDIO_HZ, 0.54), Bq::lowpass(fs, AUDIO_HZ, 1.31)],
            pilot_bp: Bq::bandpass(fs, PILOT_HZ, 60.0),
            guard_bp: Bq::bandpass(fs, GUARD_HZ, 60.0),
            sub_bp: Bq::bandpass(fs, 2.0 * PILOT_HZ, 12.0),
            deemph: [0.0; 2],
            // One-pole from the RC time constant.
            deemph_a: dt / (DEEMPHASIS_S + dt),
            pilot_pow: 0.0,
            guard_pow: 0.0,
            mag_acc: 0.0,
            mag_n: 0,
        }
    }

    /// Consume one buffer of interleaved u8 IQ, appending interleaved stereo
    /// f32 at `MPX_RATE` to `out`.
    fn process(&mut self, iq: &[u8], out: &mut Vec<f32>) {
        // The discriminator's output is a phase step; this maps full deviation
        // to unity so the audio level does not depend on the sample rate.
        let disc_scale = MPX_RATE / (std::f32::consts::TAU * DEVIATION);

        for pair in iq.chunks_exact(2) {
            let i = (pair[0] as f32 - 127.5) * (1.0 / 127.5);
            let q = (pair[1] as f32 - 127.5) * (1.0 / 127.5);

            let Some((di, dq)) = self.decim.push(i, q) else { continue };

            // Signal strength is the power of the *tuned channel*, so it is
            // measured after the ±100 kHz filter. Measuring the raw 1.024 MHz
            // passband instead reads the whole band at once, which in a busy
            // FM market pegs the meter on every frequency.
            self.mag_acc += (di * di + dq * dq) as f64;
            self.mag_n += 1;

            // Quadrature discriminator: the argument of s[n]·conj(s[n-1]).
            let pr = di * self.last_i + dq * self.last_q;
            let pi = dq * self.last_i - di * self.last_q;
            self.last_i = di;
            self.last_q = dq;
            let mpx = pi.atan2(pr) * disc_scale;

            // L+R
            let mut sum = mpx;
            for f in &mut self.lpf_sum {
                sum = f.process(sum);
            }

            // 19 kHz pilot, and the 38 kHz reference recovered by squaring it.
            let pilot = self.pilot_bp.process(mpx);
            let guard = self.guard_bp.process(mpx);
            self.pilot_pow += (pilot * pilot - self.pilot_pow) * 0.0005;
            self.guard_pow += (guard * guard - self.guard_pow) * 0.0005;
            let sub_ref = self.sub_bp.process(2.0 * pilot * pilot - 1.0);

            let stereo = self.stereo();
            let mut diff = if stereo { mpx * sub_ref * 2.0 } else { 0.0 };
            for f in &mut self.lpf_diff {
                diff = f.process(diff);
            }

            let (mut l, mut r) = (sum + diff, sum - diff);

            // De-emphasis, one pole per channel.
            self.deemph[0] += (l - self.deemph[0]) * self.deemph_a;
            self.deemph[1] += (r - self.deemph[1]) * self.deemph_a;
            l = self.deemph[0];
            r = self.deemph[1];

            out.push(l.clamp(-1.0, 1.0));
            out.push(r.clamp(-1.0, 1.0));
        }
    }

    /// Run the channel filter for its power reading alone, skipping
    /// demodulation. Used while the tuner is not the selected source, so SEEK
    /// and the meter stay live without producing audio nobody is listening to.
    fn measure_only(&mut self, iq: &[u8]) {
        for pair in iq.chunks_exact(2) {
            let i = (pair[0] as f32 - 127.5) * (1.0 / 127.5);
            let q = (pair[1] as f32 - 127.5) * (1.0 / 127.5);
            if let Some((di, dq)) = self.decim.push(i, q) {
                self.mag_acc += (di * di + dq * dq) as f64;
                self.mag_n += 1;
            }
        }
    }

    /// Mean IQ magnitude since the last call, mapped to 0..=1. This is the
    /// signal-strength reading the panel's meter shows.
    fn take_rssi(&mut self) -> (f32, f32) {
        if self.mag_n == 0 {
            return (0.0, f32::NEG_INFINITY);
        }
        let mean = (self.mag_acc / self.mag_n as f64) as f32;
        self.mag_acc = 0.0;
        self.mag_n = 0;
        let db = 10.0 * mean.max(1e-12).log10();
        (((db - FLOOR_DBFS) / (CEIL_DBFS - FLOOR_DBFS)).clamp(0.0, 1.0), db)
    }

    /// A stereo lock means a real tone at 19 kHz, not merely energy there.
    fn stereo(&self) -> bool {
        self.pilot_pow > self.guard_pow * PILOT_RATIO && self.pilot_pow > 1e-6
    }
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

/// Start the radio. Never fails: if the SDR cannot be opened the reason is
/// recorded in `RadioShared::device` and the panel reports it.
pub fn spawn<P>(prod: P, out_rate: u32) -> RadioHandle
where
    P: Producer<Item = f32> + Send + 'static,
{
    let shared = Arc::new(RadioShared::new());
    let (tx, rx) = bounded::<RadioCmd>(32);

    let t_shared = shared.clone();
    std::thread::Builder::new()
        .name("ten-qd/radio".into())
        .spawn(move || radio_thread(prod, rx, t_shared, out_rate))
        .ok();

    RadioHandle { shared, tx }
}

fn radio_thread<P>(mut prod: P, cmds: Receiver<RadioCmd>, shared: Arc<RadioShared>, out_rate: u32)
where
    P: Producer<Item = f32> + Send + 'static,
{
    let (mut ctl, mut reader) = match rtlsdr_mt::open(0) {
        Ok(pair) => pair,
        Err(_) => {
            let why = if rtlsdr_mt::devices().count() == 0 {
                "no RTL-SDR found".to_string()
            } else {
                "RTL-SDR busy — is the DVB-T driver loaded?".to_string()
            };
            *shared.device.lock().unwrap() = Some(Err(why));
            // Still drain commands so senders never block on a full channel.
            while cmds.recv().is_ok() {}
            return;
        }
    };

    let name = rtlsdr_mt::devices()
        .next()
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_else(|| "RTL-SDR".into());

    if ctl.set_sample_rate(IQ_RATE).is_err() {
        *shared.device.lock().unwrap() = Some(Err("device rejected 1.024 Msps".into()));
        return;
    }
    // Manual gain, deliberately — see TUNER_GAIN.
    let mut gains = rtlsdr_mt::TunerGains::default();
    let nearest = ctl
        .tuner_gains(&mut gains)
        .iter()
        .copied()
        .min_by_key(|g| (g - TUNER_GAIN).abs())
        .unwrap_or(TUNER_GAIN);
    let _ = ctl.set_tuner_gain(nearest);
    let _ = ctl.set_center_freq((shared.freq() * 1e6) as u32);
    let _ = ctl.reset_buffer();
    *shared.device.lock().unwrap() = Some(Ok(name));

    // --- reader thread: IQ in, audio out ---------------------------------
    let rx_shared = shared.clone();
    let rx_thread = std::thread::Builder::new()
        .name("ten-qd/radio-rx".into())
        .spawn(move || {
            let mut demod = Demod::new();
            let mut mpx: Vec<f32> = Vec::with_capacity(16384);
            let mut audio: Vec<f32> = Vec::with_capacity(8192);
            let mut resampler = super::Resampler::new(MPX_RATE as u32, out_rate);
            let mut since_report = 0u32;

            let _ = reader.read_async(8, 32_768, |bytes| {
                since_report += 1;
                if since_report >= 4 {
                    since_report = 0;
                    let (rssi, db) = demod.take_rssi();
                    rx_shared.rssi.store(rssi.to_bits(), Ordering::Relaxed);
                    rx_shared.rssi_db.store(db.to_bits(), Ordering::Relaxed);
                    rx_shared.stereo.store(demod.stereo(), Ordering::Relaxed);
                }

                if !rx_shared.enabled.load(Ordering::Relaxed) {
                    // Keep the magnitude accumulator fed so seek still works
                    // with the tuner unselected, but do not produce audio.
                    demod.measure_only(bytes);
                    return;
                }

                mpx.clear();
                demod.process(bytes, &mut mpx);
                audio.clear();
                resampler.process(&mpx, &mut audio);
                // A full ring means the decoder is not draining; dropping is
                // correct for a live source — there is nothing to catch up to.
                prod.push_slice(&audio);
            });
        })
        .ok();

    // --- control thread: tuning and seek ---------------------------------
    while let Ok(cmd) = cmds.recv() {
        match cmd {
            RadioCmd::Enable(v) => shared.enabled.store(v, Ordering::Relaxed),
            RadioCmd::Tune(mhz) => {
                let mhz = mhz.clamp(FM_LO, FM_HI);
                if ctl.set_center_freq((mhz * 1e6) as u32).is_ok() {
                    shared.freq.store((mhz * 1000.0).round() as u32, Ordering::Relaxed);
                }
            }
            RadioCmd::Seek(dir, local) => {
                shared.seeking.store(true, Ordering::Relaxed);
                seek(&mut ctl, &shared, dir, local);
                shared.seeking.store(false, Ordering::Relaxed);
            }
        }
    }

    ctl.cancel_async_read();
    if let Some(h) = rx_thread {
        let _ = h.join();
    }
}

/// Step through the band until the signal meter clears the squelch.
///
/// LOCAL raises the threshold, which is exactly what the button did: on a
/// motorway you want the scan to skip everything but the strong stations.
fn seek(ctl: &mut rtlsdr_mt::Controller, shared: &Arc<RadioShared>, dir: i32, local: bool) {
    let threshold = if local { 0.55 } else { 0.32 };
    let steps = ((FM_HI - FM_LO) / FM_STEP).round() as i32;
    let mut mhz = shared.freq();

    for _ in 0..steps {
        mhz += FM_STEP * dir as f64;
        if mhz > FM_HI {
            mhz = FM_LO;
        } else if mhz < FM_LO {
            mhz = FM_HI;
        }
        if ctl.set_center_freq((mhz * 1e6) as u32).is_err() {
            break;
        }
        shared.freq.store((mhz * 1000.0).round() as u32, Ordering::Relaxed);

        // Let the tuner settle and the meter's averaging catch up before
        // judging the channel.
        std::thread::sleep(Duration::from_millis(60));
        if shared.rssi() >= threshold {
            return;
        }
    }
    // Nothing cleared the squelch; the caller is left where the scan ended.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fir_taps_sum_to_unity() {
        let f = FirDecimator::new(31, 0.1);
        let sum: f32 = f.taps.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "taps sum to {sum}");
    }

    #[test]
    fn decimator_emits_one_sample_in_four() {
        let mut f = FirDecimator::new(15, 0.1);
        let mut n = 0;
        for i in 0..400 {
            if f.push(i as f32, 0.0).is_some() {
                n += 1;
            }
        }
        assert_eq!(n, 100);
    }

    /// A carrier with a steady frequency offset must demodulate to a steady
    /// DC level — the sign and scale of the discriminator both matter, and a
    /// swapped conjugate shows up here as an inverted result.
    #[test]
    fn discriminator_tracks_a_frequency_offset() {
        let mut d = Demod::new();
        let offset = 25_000.0f64; // a third of full deviation
        let n = 40_000;
        let iq: Vec<u8> = (0..n)
            .flat_map(|k| {
                let phase = std::f64::consts::TAU * offset * k as f64 / IQ_RATE as f64;
                let i = (phase.cos() * 100.0 + 127.5) as u8;
                let q = (phase.sin() * 100.0 + 127.5) as u8;
                [i, q]
            })
            .collect();

        let mut out = Vec::new();
        d.process(&iq, &mut out);

        // Skip the filter start-up, then average the left channel.
        let tail: Vec<f32> = out.chunks_exact(2).skip(2000).map(|f| f[0]).collect();
        let mean: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
        let expected = (offset / DEVIATION as f64) as f32;
        assert!(
            (mean - expected).abs() < 0.12,
            "demodulated {mean}, expected about {expected}"
        );
    }

    #[test]
    fn silence_reads_as_no_signal_and_no_pilot() {
        let mut d = Demod::new();
        let iq = vec![127u8; 8192];
        let mut out = Vec::new();
        d.process(&iq, &mut out);
        assert!(!d.stereo(), "noise floor must not claim a stereo lock");
        assert!(d.take_rssi().0 < 0.1);
    }
}
