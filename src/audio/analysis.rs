//! Nine-band spectrum analysis, feeding the QM-571 meters.
//!
//! ## Why nine bands
//!
//! The ad's power amplifier carries a long row of amber dots, and the HTML
//! prototype modelled that as nine columns. Rather than make those nine columns
//! an arbitrary decoration, they are bound to the *same nine centre frequencies
//! the QE-581 controls*. Pull the 250 Hz slider down and the third meter column
//! visibly drops. That turns the stack into one instrument instead of two
//! unrelated panels, and it is the reason the meters are worth having at all.
//!
//! The FFT runs on its own thread, fed from the post-DSP signal by a ring
//! buffer. Nothing here executes inside the audio callback.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use rustfft::{num_complex::Complex32, Fft, FftPlanner};

use crate::state::BAND_HZ;

pub const FFT_SIZE: usize = 2048;
/// Level floor. Anything quieter than this reads as an unlit column.
const FLOOR_DB: f32 = -66.0;
const CEIL_DB: f32 = -6.0;

/// Shared, lock-free meter readout. The audio side writes, the UI reads.
pub struct Meters {
    bands: [AtomicU32; 9],
    /// Frames the output callback has actually delivered to the device. This
    /// is the clock the elapsed-time display runs on — not the decoder's
    /// position, which runs ahead by the depth of the ring.
    pub frames_out: std::sync::atomic::AtomicU64,
}

impl Default for Meters {
    fn default() -> Self {
        Self::new()
    }
}

impl Meters {
    pub fn new() -> Self {
        Meters {
            bands: std::array::from_fn(|_| AtomicU32::new(0)),
            frames_out: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn set(&self, i: usize, v: f32) {
        self.bands[i].store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn read(&self) -> [f32; 9] {
        std::array::from_fn(|i| f32::from_bits(self.bands[i].load(Ordering::Relaxed)))
    }

    /// Decay every band toward zero. Called when the transport is not running,
    /// so the meters fall away rather than freezing mid-swing.
    pub fn decay(&self, factor: f32) {
        for i in 0..9 {
            let v = f32::from_bits(self.bands[i].load(Ordering::Relaxed)) * factor;
            self.bands[i].store(if v < 0.001 { 0.0 } else { v }.to_bits(), Ordering::Relaxed);
        }
    }
}

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    scratch: Vec<Complex32>,
    /// Geometric band edges around each centre frequency, in bins.
    edges: [(usize, usize); 9],
    /// Previous frame's value per band, for attack/release ballistics.
    smoothed: [f32; 9],
}

impl Analyzer {
    pub fn new(sample_rate: f32) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        // Hann window — the usual choice when you care about level accuracy
        // per band more than about resolving two close tones.
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|n| {
                let t = n as f32 / (FFT_SIZE - 1) as f32;
                0.5 - 0.5 * (std::f32::consts::TAU * t).cos()
            })
            .collect();

        let bin_hz = sample_rate / FFT_SIZE as f32;
        let edges = std::array::from_fn(|i| {
            // Edges sit at the geometric mean between neighbouring centres, so
            // the bands tile the spectrum without overlap or gaps.
            let lo = if i == 0 {
                BAND_HZ[0] / 1.6
            } else {
                (BAND_HZ[i] * BAND_HZ[i - 1]).sqrt()
            };
            let hi = if i == 8 {
                (BAND_HZ[8] * 1.6).min(sample_rate / 2.0 - bin_hz)
            } else {
                (BAND_HZ[i] * BAND_HZ[i + 1]).sqrt()
            };
            let lo_bin = ((lo / bin_hz).floor() as usize).max(1);
            let hi_bin = ((hi / bin_hz).ceil() as usize).min(FFT_SIZE / 2 - 1);
            (lo_bin, hi_bin.max(lo_bin))
        });

        Analyzer {
            fft,
            window,
            scratch: vec![Complex32::new(0.0, 0.0); FFT_SIZE],
            edges,
            smoothed: [0.0; 9],
        }
    }

    /// Analyse one windowful of mono samples and publish the nine band levels.
    pub fn process(&mut self, mono: &[f32], meters: &Meters) {
        debug_assert_eq!(mono.len(), FFT_SIZE);

        for (i, s) in mono.iter().enumerate() {
            self.scratch[i] = Complex32::new(s * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);

        // Coherent gain of a Hann window is 0.5; correct for it so the dB
        // figures mean something rather than being 6 dB light.
        let norm = 2.0 / (FFT_SIZE as f32 * 0.5);

        for b in 0..9 {
            let (lo, hi) = self.edges[b];
            let mut power = 0.0f32;
            for bin in lo..=hi {
                let m = self.scratch[bin].norm() * norm;
                power += m * m;
            }
            let db = 10.0 * (power.max(1e-12)).log10();
            let t = ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0.0, 1.0);

            // Fast attack, slow release — meter ballistics. A meter that falls
            // as fast as it rises reads as noise; this reads as music.
            let prev = self.smoothed[b];
            self.smoothed[b] = if t > prev { t } else { prev * 0.82 + t * 0.18 };
            meters.set(b, self.smoothed[b]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_lights_its_own_band() {
        let fs = 48_000.0;
        let meters = Meters::new();
        let mut a = Analyzer::new(fs);

        // 1 kHz is band index 4.
        let tone: Vec<f32> = (0..FFT_SIZE)
            .map(|n| (std::f32::consts::TAU * 1000.0 * n as f32 / fs).sin() * 0.5)
            .collect();

        // Run a few frames so the ballistics settle.
        for _ in 0..8 {
            a.process(&tone, &meters);
        }
        let v = meters.read();

        assert!(v[4] > 0.5, "1 kHz band should be lit: {v:?}");
        for (i, x) in v.iter().enumerate() {
            if i != 4 {
                assert!(*x < v[4], "band {i} ({x}) should sit below the tone's band");
            }
        }
    }

    #[test]
    fn silence_reads_as_dark() {
        let meters = Meters::new();
        let mut a = Analyzer::new(48_000.0);
        let quiet = vec![0.0f32; FFT_SIZE];
        for _ in 0..20 {
            a.process(&quiet, &meters);
        }
        assert!(meters.read().iter().all(|v| *v < 0.02));
    }

    #[test]
    fn band_edges_are_ordered_and_in_range() {
        let a = Analyzer::new(44_100.0);
        for (i, (lo, hi)) in a.edges.iter().enumerate() {
            assert!(lo <= hi, "band {i} edges inverted");
            assert!(*hi < FFT_SIZE / 2, "band {i} runs past Nyquist");
        }
        for w in a.edges.windows(2) {
            assert!(w[0].0 <= w[1].0, "bands out of order");
        }
    }
}
