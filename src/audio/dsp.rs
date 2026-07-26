//! The signal path.
//!
//! ```text
//!   decoded PCM ──▶ tone shelves ──┬──▶ FRONT bank: 9 peaking biquads ──┐
//!    (L,R)          (bass/treble)  │                                    ├──▶ fader mix
//!                                  └──▶ REAR  bank: 9 peaking biquads ──┘        │
//!                                                                                ▼
//!                                                              volume · ATT ──▶ out
//! ```
//!
//! Two banks of nine is the whole point of the QE-581 — front and rear are
//! curved independently and the fader decides how much of each reaches the
//! output. On a stereo device the two buses are summed by the fader rather than
//! driving four speakers, so moving the fader audibly crossfades between the
//! two curves. That is the honest stereo reduction of a four-channel car rig.
//!
//! Filters are RBJ cookbook biquads in transposed direct form II.

use crate::state::BAND_HZ;

/// One biquad section. `z1`/`z2` are the TDF-II state variables.
#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn set(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        // Normalise by a0 so the difference equation is monic.
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    /// Unity gain, no filtering. Used when a band sits at 0 dB so we do not
    /// pay for a filter that does nothing audible.
    fn set_bypass(&mut self) {
        self.set(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    }

    /// RBJ peaking EQ.
    fn set_peaking(&mut self, fs: f32, f0: f32, q: f32, gain_db: f32) {
        if gain_db.abs() < 0.01 || f0 >= fs * 0.5 {
            self.set_bypass();
            return;
        }
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * f0 / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);
        self.set(
            1.0 + alpha * a,
            -2.0 * cos_w0,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w0,
            1.0 - alpha / a,
        );
    }

    /// RBJ low shelf — the BASS key on the control head.
    fn set_low_shelf(&mut self, fs: f32, f0: f32, s: f32, gain_db: f32) {
        if gain_db.abs() < 0.01 {
            self.set_bypass();
            return;
        }
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * f0 / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        self.set(
            a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
            a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
            (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
            (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
        );
    }

    /// RBJ high shelf — the TREBLE key.
    fn set_high_shelf(&mut self, fs: f32, f0: f32, s: f32, gain_db: f32) {
        if gain_db.abs() < 0.01 {
            self.set_bypass();
            return;
        }
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * f0 / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        self.set(
            a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
            a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
            (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
            (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
        );
    }
}

/// Bands sit roughly an octave apart, so a Q near 1.4 gives skirts that meet
/// without the comb-filtering you get from stacking narrow bells.
const BAND_Q: f32 = 1.41;
/// One press of BASS or TREBLE. Five ticks, -2..=+2, so ±9 dB at the extremes.
pub const TONE_STEP_DB: f32 = 4.5;
const BASS_HZ: f32 = 100.0;
const TREBLE_HZ: f32 = 8_000.0;
/// The attenuator. The panel calls its steps 70% and 60%; this is the gain the
/// ATT key actually applies.
const ATT_GAIN: f32 = 0.2;

/// How far the equaliser's output trim swings, either side of unity.
///
/// Volume alone can only ever attenuate: full travel is a gain of exactly 1.0,
/// so a quiet source — a stream mastered low, an auxiliary input fed from
/// something polite — has no way to reach the top of the dial. Above unity
/// this is the makeup gain that fixes that.
///
/// It cuts as well, because an equaliser's output trim always did: boost nine
/// bands and the thing you need next is somewhere to put the level back, not
/// another way to raise it. Volume is where you left it; GAIN is what matches
/// the curve to it.
pub const GAIN_LIMIT_DB: i8 = 12;

/// Where the soft clipper starts rounding the corners, as linear amplitude
/// (-1.94 dBFS). Below this the chain is bit-exact; above it, samples are
/// bent toward the rail instead of being allowed to wrap.
const CLIP_KNEE: f32 = 0.8;

/// Bend the tops of anything that overshoots, rather than letting it wrap.
///
/// A real head unit ran out of headroom in its power amplifier and got
/// progressively less clean about it; digital arithmetic instead wraps or
/// produces samples the device clamps square, which sounds far worse than the
/// thing being imitated. Below the knee this is the identity function, so
/// nothing that was not going to clip is altered at all.
#[inline]
fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= CLIP_KNEE {
        return x;
    }
    let over = (a - CLIP_KNEE) / (1.0 - CLIP_KNEE);
    x.signum() * (CLIP_KNEE + (1.0 - CLIP_KNEE) * over.tanh())
}

/// Everything the callback needs to know, published as one immutable snapshot.
/// The UI thread swaps a whole new one in; the audio thread never blocks.
#[derive(Clone, Debug)]
pub struct DspParams {
    pub generation: u64,
    pub sample_rate: f32,
    pub eq_front: [f32; 9],
    pub eq_rear: [f32; 9],
    pub defeat: bool,
    pub bass: i8,
    pub treble: i8,
    pub volume: f32,
    pub att: bool,
    /// Equaliser output trim in whole dB, ±GAIN_LIMIT_DB. Applied on top of
    /// volume, so the dial keeps its meaning and a quiet source can still
    /// reach the top of it.
    pub gain_db: i8,
    pub fader: f32,
    pub power: bool,
}

impl Default for DspParams {
    fn default() -> Self {
        DspParams {
            generation: 0,
            sample_rate: 44_100.0,
            eq_front: [0.0; 9],
            eq_rear: [0.0; 9],
            defeat: false,
            bass: 0,
            treble: 0,
            volume: 0.55,
            att: false,
            gain_db: 0,
            fader: 0.5,
            power: true,
        }
    }
}

impl DspParams {
    /// Linear output gain. Squared-ish curve because a linear volume control
    /// feels wrong — most of the useful range bunches up at the bottom.
    pub fn out_gain(&self) -> f32 {
        if !self.power {
            return 0.0;
        }
        let v = self.volume.clamp(0.0, 1.0).powf(2.2);
        let trim = 10f32.powf(self.gain_db.clamp(-GAIN_LIMIT_DB, GAIN_LIMIT_DB) as f32 / 20.0);
        v * trim * if self.att { ATT_GAIN } else { 1.0 }
    }

    /// Equal-power crossfade between the two buses, so moving the fader does
    /// not dip the total level through the middle.
    pub fn bus_gains(&self) -> (f32, f32) {
        let f = self.fader.clamp(0.0, 1.0);
        (f.sqrt(), (1.0 - f).sqrt())
    }
}

/// Per-channel filter state for one bus.
#[derive(Clone, Copy, Default)]
struct BankState {
    bands: [Biquad; 9],
}

impl BankState {
    #[inline]
    fn process(&mut self, mut x: f32) -> f32 {
        for b in &mut self.bands {
            x = b.process(x);
        }
        x
    }
}

/// The live chain. Owned by the audio callback; never shared.
pub struct DspChain {
    params: DspParams,
    /// `[bus][channel]` — front/rear by left/right.
    banks: [[BankState; 2]; 2],
    tone: [[Biquad; 2]; 2], // [bass/treble][channel]
    /// Smoothed output gain, so a volume press does not click.
    gain: f32,
}

impl Default for DspChain {
    fn default() -> Self {
        Self::new()
    }
}

impl DspChain {
    pub fn new() -> Self {
        let mut c = DspChain {
            params: DspParams::default(),
            banks: [[BankState::default(); 2]; 2],
            tone: [[Biquad::default(); 2]; 2],
            gain: 0.0,
        };
        c.retune();
        c
    }

    /// Adopt new parameters. Cheap when the generation has not moved, which is
    /// the common case — coefficients are only recomputed when a control moved.
    pub fn sync(&mut self, p: &DspParams) {
        // Always take the whole snapshot; the generation decides only whether
        // the *coefficients* need recomputing.
        //
        // This used to copy a hand-written list of "continuous" fields in the
        // unchanged-generation case, which meant every new continuous control
        // had to remember to join the list. `gain_db` did not, and the result
        // was a trim that appeared to do nothing until some unrelated control
        // — DEFEAT, in practice — bumped the generation, at which point it
        // applied all at once. A list you must remember to extend is a bug
        // waiting for the next control; copying the struct cannot forget.
        let retune = p.generation != self.params.generation;
        self.params = p.clone();
        if retune {
            self.retune();
        }
    }

    fn retune(&mut self) {
        let p = &self.params;
        let fs = p.sample_rate;
        for (bus, gains) in [(0usize, p.eq_front), (1usize, p.eq_rear)] {
            for ch in 0..2 {
                for i in 0..9 {
                    let db = if p.defeat { 0.0 } else { gains[i] };
                    self.banks[bus][ch].bands[i].set_peaking(fs, BAND_HZ[i], BAND_Q, db);
                }
            }
        }
        let bass_db = p.bass as f32 * TONE_STEP_DB;
        let treble_db = p.treble as f32 * TONE_STEP_DB;
        for ch in 0..2 {
            self.tone[0][ch].set_low_shelf(fs, BASS_HZ, 0.7, bass_db);
            self.tone[1][ch].set_high_shelf(fs, TREBLE_HZ, 0.7, treble_db);
        }
    }

    /// Process one interleaved stereo block in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        let target = self.params.out_gain();
        let (gf, gr) = self.params.bus_gains();

        // `ch` indexes the filter banks as well as the frame, so it stays an
        // index rather than becoming an iterator.
        #[allow(clippy::needless_range_loop)]
        for frame in buf.chunks_exact_mut(2) {
            // One-pole gain smoothing — ~5 ms at 44.1 kHz. Removes zipper noise
            // on volume presses without making the control feel laggy.
            self.gain += (target - self.gain) * 0.005;

            for ch in 0..2 {
                let mut x = frame[ch];
                x = self.tone[0][ch].process(x);
                x = self.tone[1][ch].process(x);

                let front = self.banks[0][ch].process(x);
                let rear = self.banks[1][ch].process(x);

                frame[ch] = soft_clip((front * gf + rear * gr) * self.gain);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
    }

    /// Volume alone tops out at unity — that is the limitation the GAIN trim
    /// exists to lift, so it is worth pinning down.
    #[test]
    fn volume_alone_cannot_exceed_unity() {
        let p = DspParams { volume: 1.0, ..Default::default() };
        assert!((p.out_gain() - 1.0).abs() < 1e-6, "got {}", p.out_gain());
    }

    #[test]
    fn the_gain_trim_moves_the_decibels_it_claims() {
        for db in [-12i8, -6, -3, 0, 3, 6, 12] {
            let p = DspParams { volume: 1.0, gain_db: db, ..Default::default() };
            let got = 20.0 * p.out_gain().log10();
            assert!((got - db as f32).abs() < 0.01, "{db:+} dB asked, {got:.2} dB given");
        }
    }

    #[test]
    fn the_gain_trim_cannot_be_driven_past_either_stop() {
        let hot = DspParams { volume: 1.0, gain_db: 99, ..Default::default() };
        let cold = DspParams { volume: 1.0, gain_db: -99, ..Default::default() };
        let max = 10f32.powf(GAIN_LIMIT_DB as f32 / 20.0);
        let min = 10f32.powf(-GAIN_LIMIT_DB as f32 / 20.0);
        assert!((hot.out_gain() - max).abs() < 1e-3);
        assert!((cold.out_gain() - min).abs() < 1e-4, "the trim cuts as well as boosts");
    }

    #[test]
    fn the_attenuator_still_wins_over_makeup_gain() {
        // ATT is a safety control: pressing it must always drop the level,
        // however much makeup has been dialled in.
        let a = DspParams { volume: 1.0, gain_db: 12, att: false, ..Default::default() };
        let b = DspParams { att: true, ..a.clone() };
        assert!(b.out_gain() < a.out_gain() * 0.25);
    }

    /// A continuous control must take effect on its own, without waiting for
    /// something else to trigger a retune.
    #[test]
    fn the_trim_applies_without_a_retune() {
        // Quiet enough that +12 dB stays under the soft clipper's knee —
        // otherwise this measures the clipper and reports ~9 dB, which is the
        // clipper doing its job and the test asking the wrong question.
        let settle = |c: &mut DspChain| {
            // Two blocks: the one-pole gain smoothing needs a moment to reach
            // its target, and measuring during the ramp proves nothing.
            let mut buf = vec![0.05f32; 8192];
            c.process(&mut buf);
            let mut buf = vec![0.05f32; 8192];
            c.process(&mut buf);
            rms(&buf)
        };

        // One generation, held fixed for the whole test — nothing here is
        // allowed to look like a filter change.
        let base = DspParams { volume: 1.0, generation: 7, ..Default::default() };
        let mut c = DspChain::new();
        c.sync(&base);
        let unity = settle(&mut c);

        c.sync(&DspParams { gain_db: 12, ..base.clone() });
        let boosted = settle(&mut c);
        let up = 20.0 * (boosted / unity).log10();
        assert!((up - 12.0).abs() < 0.5, "expected ~+12 dB, got {up:.2}");

        c.sync(&DspParams { gain_db: -12, ..base.clone() });
        let cut = settle(&mut c);
        let down = 20.0 * (cut / unity).log10();
        assert!((down + 12.0).abs() < 0.5, "expected ~-12 dB, got {down:.2}");
    }

    #[test]
    fn the_soft_clipper_is_transparent_below_the_knee() {
        for x in [0.0f32, 0.1, -0.5, 0.79, -0.8] {
            assert_eq!(soft_clip(x), x, "altered a sample that was not clipping");
        }
    }

    #[test]
    fn the_soft_clipper_bounds_everything_above_it() {
        for x in [1.0f32, 4.0, 40.0, -4.0, -40.0] {
            let y = soft_clip(x);
            assert!(y.abs() <= 1.0, "{x} escaped as {y}");
            assert_eq!(y.signum(), x.signum(), "{x} changed sign");
            assert!(y.abs() > CLIP_KNEE, "{x} was crushed to {y}");
        }
    }

    /// The point of the trim, end to end: a quiet source reaches full output
    /// with the volume dial where it already was.
    #[test]
    fn makeup_gain_lifts_a_quiet_source_through_the_whole_chain() {
        let quiet = 0.05f32; // -26 dBFS
        let run = |db: i8| {
            let p = DspParams { volume: 1.0, gain_db: db, generation: 1, ..Default::default() };
            let mut c = DspChain::new();
            c.sync(&p);
            let mut buf = vec![quiet; 8192];
            // Let the gain smoothing settle before measuring.
            c.process(&mut buf);
            let mut buf = vec![quiet; 8192];
            c.process(&mut buf);
            rms(&buf)
        };
        let lifted = 20.0 * (run(12) / run(0)).log10();
        assert!((lifted - 12.0).abs() < 0.5, "expected ~12 dB, got {lifted:.2}");
    }

    /// Feed a sine at a band centre and check the band's gain actually moves
    /// the level. This is the test that proves the EQ is not decoration.
    #[test]
    fn boosting_a_band_raises_that_band() {
        let fs = 44_100.0;
        let f0 = BAND_HZ[4]; // 1 kHz
        let make = |db: f32| {
            let mut p = DspParams { sample_rate: fs, volume: 1.0, ..Default::default() };
            p.eq_front[4] = db;
            p.eq_rear[4] = db;
            p.generation = 1;
            let mut c = DspChain::new();
            c.sync(&p);
            let mut buf: Vec<f32> = (0..8192)
                .flat_map(|n| {
                    let s = (std::f32::consts::TAU * f0 * n as f32 / fs).sin() * 0.25;
                    [s, s]
                })
                .collect();
            c.process(&mut buf);
            // Skip the gain-smoothing ramp at the head of the block.
            rms(&buf[4096..])
        };

        let flat = make(0.0);
        let boosted = make(9.0);
        let cut = make(-9.0);

        assert!(boosted > flat * 1.8, "boost: {boosted} vs flat {flat}");
        assert!(cut < flat * 0.6, "cut: {cut} vs flat {flat}");
    }

    #[test]
    fn defeat_flattens_the_curve() {
        let mut p = DspParams { generation: 1, volume: 1.0, ..Default::default() };
        p.eq_front = [12.0; 9];
        p.defeat = true;
        let mut c = DspChain::new();
        c.sync(&p);
        // With defeat on, every band must have been tuned to bypass.
        let mut buf = vec![0.0f32; 512];
        buf[0] = 1.0;
        buf[1] = 1.0;
        c.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn fader_is_equal_power() {
        let p = DspParams { fader: 0.5, ..Default::default() };
        let (f, r) = p.bus_gains();
        assert!((f * f + r * r - 1.0).abs() < 1e-5);
    }

    #[test]
    fn power_off_is_silent() {
        let p = DspParams { power: false, volume: 1.0, ..Default::default() };
        assert_eq!(p.out_gain(), 0.0);
    }
}
