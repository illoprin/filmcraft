//! Master-output spectrum analysis for the Spectrum Analyzer panel.
//!
//! One Hann-windowed FFT is turned into dBFS per bin, tilted, frequency-smoothed (fractional
//! octave, which is what removes the "comb" of a raw FFT) and temporally smoothed (fast attack,
//! adjustable dB/s release), with a held peak line. The defaults follow Voxengo SPAN: +4.5 dB per
//! octave slope (so a modern mix reads nearly flat) and 1/3-octave smoothing.
//!
//! Allocation happens in [`SpectrumAnalyzer::new`] only; [`SpectrumAnalyzer::process`] is
//! allocation-free and does a single FFT.

use crate::fft::Fft;

/// A bin with no signal reads this (below any display floor).
const SILENCE_DB: f32 = -200.0;

/// Display settings, mirroring the panel's controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectrumConfig {
    /// Spectral tilt added to the display in dB per octave (SPAN's "Slope"), 0..6.
    pub slope_db_per_oct: f32,
    /// Fall speed of a bin once its signal stops, in dB per second (0 = freeze).
    pub decay_db_per_s: f32,
    /// Fractional-octave smoothing window width in octaves (0 = off, 1/3, 1/12…).
    pub smooth_octaves: f32,
    /// Fall speed of the held peak, in dB per second.
    pub peak_decay_db_per_s: f32,
    /// Floor of the dB scale.
    pub min_db: f32,
    /// Top of the dB scale.
    pub max_db: f32,
}

impl Default for SpectrumConfig {
    fn default() -> Self {
        SpectrumConfig { slope_db_per_oct: 4.5, decay_db_per_s: 60.0, smooth_octaves: 1.0 / 3.0, peak_decay_db_per_s: 3.0, min_db: -90.0, max_db: 0.0 }
    }
}

/// Spectral tilt (dB) at `hz` for `slope_db_per_oct`, zero at 1 kHz (SPAN's reference).
pub fn tilt_db(slope_db_per_oct: f32, hz: f32) -> f32 {
    if !hz.is_finite() || hz <= 0.0 {
        return 0.0;
    }
    slope_db_per_oct * (hz / 1000.0).log2()
}

#[inline]
fn clamp_db(v: f32, cfg: &SpectrumConfig) -> f32 {
    if v.is_finite() { v.clamp(cfg.min_db, cfg.max_db) } else { cfg.min_db }
}

/// Streaming spectrum analyzer for one channel (the mono master sum).
///
/// Feed it the newest `fft_size` samples each frame with the elapsed time `dt` (for the release
/// and the peak fall); read the smoothed spectrum with [`SpectrumAnalyzer::db_at`] /
/// [`SpectrumAnalyzer::bins`] and the held peak with [`SpectrumAnalyzer::peak_at`] /
/// [`SpectrumAnalyzer::peak_bins`].
pub struct SpectrumAnalyzer {
    fft: Fft,
    n: usize,
    sr: f32,
    /// Hann window and its coherent-gain normalisation (`2 / Σw`) so a full-scale sine reads 0 dBFS.
    window: Vec<f32>,
    win_norm: f32,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Magnitude in dB per bin with the tilt applied (0..=n/2).
    raw: Vec<f32>,
    /// After fractional-octave smoothing.
    freq: Vec<f32>,
    /// After temporal smoothing (the drawn spectrum).
    smooth: Vec<f32>,
    /// Held peak in dB.
    peak: Vec<f32>,
    /// Scratch for fractional-octave smoothing: linear power and its prefix sum.
    pow: Vec<f64>,
    prefix: Vec<f64>,
}

impl SpectrumAnalyzer {
    /// An analyzer for `fft_size` (rounded up to a power of two, ≥ 16) at `sample_rate`.
    pub fn new(fft_size: usize, sample_rate: f32) -> Self {
        let n = fft_size.max(16).next_power_of_two();
        let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
        let win_norm = 2.0 / window.iter().sum::<f32>().max(1e-12);
        let bins = n / 2 + 1;
        SpectrumAnalyzer {
            fft: Fft::new(n),
            n,
            sr: sample_rate.max(1.0),
            window,
            win_norm,
            re: vec![0.0; n],
            im: vec![0.0; n],
            raw: vec![SILENCE_DB; bins],
            freq: vec![SILENCE_DB; bins],
            smooth: vec![SILENCE_DB; bins],
            peak: vec![SILENCE_DB; bins],
            pow: vec![0.0; bins],
            prefix: vec![0.0; bins + 1],
        }
    }

    /// Planned FFT size (a power of two).
    pub fn fft_size(&self) -> usize {
        self.n
    }

    /// Sample rate the bin frequencies are computed from.
    pub fn sample_rate(&self) -> f32 {
        self.sr
    }

    /// Number of bins (`fft_size / 2 + 1`, DC … Nyquist).
    pub fn len(&self) -> usize {
        self.smooth.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// The frequency of bin `i`, in Hz.
    pub fn bin_hz(&self, i: usize) -> f32 {
        i as f32 * self.sr / self.n as f32
    }

    /// The smoothed spectrum, dB per bin.
    pub fn bins(&self) -> &[f32] {
        &self.smooth
    }

    /// The held peak, dB per bin.
    pub fn peak_bins(&self) -> &[f32] {
        &self.peak
    }

    /// Analyze a window of `fft_size` samples (shorter windows are zero-padded at the end) and
    /// advance the smoothing by `dt` seconds.
    pub fn process(&mut self, samples: &[f32], dt: f32, cfg: &SpectrumConfig) {
        let n = self.n;
        for i in 0..n {
            let x = samples.get(i).copied().unwrap_or(0.0);
            self.re[i] = if x.is_finite() { x * self.window[i] } else { 0.0 };
            self.im[i] = 0.0;
        }
        self.fft.forward(&mut self.re, &mut self.im);
        let bins = self.raw.len();
        for k in 0..bins {
            let re = self.re.get(k).copied().unwrap_or(0.0);
            let im = self.im.get(k).copied().unwrap_or(0.0);
            let mag = (re * re + im * im).sqrt() * self.win_norm;
            let db = 20.0 * mag.max(1e-10).log10() + tilt_db(cfg.slope_db_per_oct, self.bin_hz(k));
            self.raw[k] = db;
        }
        self.fractional_octave(cfg.smooth_octaves);
        let fall = (cfg.decay_db_per_s.max(0.0) * dt.max(0.0)).min(1e6);
        let peak_fall = (cfg.peak_decay_db_per_s.max(0.0) * dt.max(0.0)).min(1e6);
        for k in 0..bins {
            let instant = clamp_db(self.freq[k], cfg);
            // Fast attack, adjustable decay: a bin jumps up at once and falls at `decay` dB/s.
            let v = instant.max(self.smooth[k] - fall);
            self.smooth[k] = clamp_db(v, cfg);
            let p = self.peak[k] - peak_fall;
            self.peak[k] = clamp_db(p.max(self.smooth[k]), cfg);
        }
    }

    /// dB at an arbitrary frequency, linearly interpolated between the smoothed bins.
    pub fn db_at(&self, hz: f32) -> f32 {
        self.at(&self.smooth, hz)
    }

    /// Held-peak dB at an arbitrary frequency.
    pub fn peak_at(&self, hz: f32) -> f32 {
        self.at(&self.peak, hz)
    }

    fn at(&self, buf: &[f32], hz: f32) -> f32 {
        let bins = buf.len();
        if bins == 0 {
            return SILENCE_DB;
        }
        let x = (hz.max(0.0) * self.n as f32 / self.sr).clamp(0.0, (bins - 1) as f32);
        let i = x.floor() as usize;
        let frac = x - i as f32;
        let a = buf.get(i).copied().unwrap_or(SILENCE_DB);
        let b = buf.get(i + 1).copied().unwrap_or(a);
        a + (b - a) * frac
    }

    /// Reset the held peak to the floor so it rebuilds from the next frames.
    pub fn reset_peak(&mut self) {
        self.peak.iter_mut().for_each(|p| *p = SILENCE_DB);
    }

    /// Average the linear power over a fractional-octave window. Because the bins are uniform in
    /// frequency, the window of `oct` octaves is a constant fraction of the bin index: `k*2^(±oct/2)`.
    fn fractional_octave(&mut self, oct: f32) {
        let bins = self.raw.len();
        if !oct.is_finite() || oct <= 0.0 || bins < 2 {
            self.freq.copy_from_slice(&self.raw);
            return;
        }
        let half = (oct * 0.5).clamp(0.0, 8.0);
        let lo = 2f32.powf(-half);
        let hi = 2f32.powf(half);
        for k in 0..bins {
            self.pow[k] = 10f64.powf(self.raw[k] as f64 / 10.0);
        }
        self.prefix[0] = 0.0;
        for k in 0..bins {
            self.prefix[k + 1] = self.prefix[k] + self.pow[k];
        }
        for k in 0..bins {
            let c = k as f32;
            let j0 = (c * lo).ceil().max(0.0) as usize;
            let j1 = ((c * hi).floor().max(0.0) as usize).min(bins - 1);
            let (j0, j1) = (j0.min(bins - 1).min(j1), j1.max(j0.min(bins - 1)));
            let count = (j1 - j0 + 1) as f64;
            let avg = (self.prefix[j1 + 1] - self.prefix[j0]) / count.max(1.0);
            self.freq[k] = if avg > 0.0 { (10.0 * avg.log10()) as f32 } else { SILENCE_DB };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    const SR: f32 = 48000.0;

    fn flat_cfg() -> SpectrumConfig {
        SpectrumConfig { slope_db_per_oct: 0.0, decay_db_per_s: 0.0, smooth_octaves: 0.0, peak_decay_db_per_s: 0.0, min_db: -120.0, max_db: 20.0 }
    }

    fn tone(n: usize, hz: f32, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / SR).sin()).collect()
    }

    #[test]
    fn a_full_scale_tone_reads_0_dbfs_at_its_bin() {
        let n = 4096;
        let mut a = SpectrumAnalyzer::new(n, SR);
        let k = 171usize;
        let f = k as f32 * SR / n as f32;
        a.process(&tone(n, f, 1.0), 0.0, &flat_cfg());
        assert_eq!(a.len(), n / 2 + 1);
        assert!((a.db_at(f) - 0.0).abs() < 0.5, "{}", a.db_at(f));
        // Far from the tone the spectrum is deep.
        assert!(a.db_at(f * 2.0) < -60.0);
    }

    #[test]
    fn tilt_is_zero_at_1khz_and_adds_the_slope_per_octave() {
        assert!(tilt_db(4.5, 1000.0).abs() < 1e-6);
        assert!((tilt_db(4.5, 2000.0) - 4.5).abs() < 1e-4);
        assert!((tilt_db(4.5, 500.0) + 4.5).abs() < 1e-4);
        assert_eq!(tilt_db(4.5, 0.0), 0.0);
        assert_eq!(tilt_db(4.5, -100.0), 0.0);
    }

    #[test]
    fn a_bin_falls_at_the_decay_rate_after_the_signal_stops() {
        let n = 2048;
        let mut a = SpectrumAnalyzer::new(n, SR);
        let cfg = SpectrumConfig { decay_db_per_s: 60.0, ..flat_cfg() };
        let f = 1000.0;
        a.process(&tone(n, f, 1.0), 0.02, &cfg);
        let before = a.db_at(f);
        a.process(&vec![0.0; n], 0.1, &cfg);
        let after = a.db_at(f);
        assert!((before - after - 6.0).abs() < 1.0, "{before} → {after}");
    }

    #[test]
    fn peak_holds_the_maximum_then_resets() {
        let n = 2048;
        let mut a = SpectrumAnalyzer::new(n, SR);
        let cfg = SpectrumConfig { decay_db_per_s: 60.0, peak_decay_db_per_s: 0.0, ..flat_cfg() };
        let f = 43.0 * SR / n as f32; // exactly on a bin
        a.process(&tone(n, f, 1.0), 0.02, &cfg);
        assert!(a.peak_at(f) > -0.5, "{}", a.peak_at(f));
        a.process(&vec![0.0; n], 0.5, &cfg);
        assert!(a.db_at(f) < -20.0, "spectrum fell: {}", a.db_at(f));
        assert!(a.peak_at(f) > -0.5, "peak still held: {}", a.peak_at(f));
        a.reset_peak();
        a.process(&vec![0.0; n], 0.0, &cfg);
        assert!((a.peak_at(f) - a.db_at(f)).abs() < 1e-3, "peak reset to the spectrum");
    }

    #[test]
    fn fractional_octave_smoothing_spreads_a_single_tone() {
        let n = 4096;
        let f = 93.0 * SR / n as f32;
        let mut sharp = SpectrumAnalyzer::new(n, SR);
        let mut soft = SpectrumAnalyzer::new(n, SR);
        sharp.process(&tone(n, f, 1.0), 0.0, &flat_cfg());
        soft.process(&tone(n, f, 1.0), 0.0, &SpectrumConfig { smooth_octaves: 1.0 / 3.0, ..flat_cfg() });
        assert!(soft.db_at(f) < sharp.db_at(f) - 3.0, "{} vs {}", soft.db_at(f), sharp.db_at(f));
        // Energy is spread: within the 1/6-octave half-window the level is lifted.
        assert!(soft.db_at(f * 1.05) > sharp.db_at(f * 1.05) + 5.0, "{} vs {}", soft.db_at(f * 1.05), sharp.db_at(f * 1.05));
    }

    #[test]
    fn hostile_samples_never_panic_and_stay_finite() {
        let n = 1024;
        let mut a = SpectrumAnalyzer::new(n, SR);
        let mut x = vec![f32::NAN; n];
        x[0] = f32::INFINITY;
        x[1] = f32::NEG_INFINITY;
        x[2] = 1e30;
        a.process(&x, 0.016, &flat_cfg());
        assert!(a.bins().iter().all(|v| v.is_finite()));
        assert!(a.peak_bins().iter().all(|v| v.is_finite()));
        // A short block is zero-padded rather than panicking.
        a.process(&[0.5, -0.5], 0.016, &flat_cfg());
        assert!(a.bins().iter().all(|v| v.is_finite()));
        a.process(&[], 0.016, &flat_cfg());
        assert!(a.db_at(1000.0).is_finite());
    }

    #[test]
    fn a_noisy_sweep_is_smooth_between_neighbours() {
        // A raw FFT of noise is jagged; with 1/3-octave smoothing neighbouring bins differ by
        // little (no comb), which is exactly the display requirement.
        let n = 4096;
        let mut a = SpectrumAnalyzer::new(n, SR);
        let mut rng = Rng::new(9);
        let x: Vec<f32> = (0..n).map(|_| rng.uniform() * 0.5).collect();
        a.process(&x, 0.0, &SpectrumConfig { smooth_octaves: 1.0 / 3.0, ..flat_cfg() });
        let hz = a.bin_hz(300);
        let mut prev = a.db_at(hz);
        for i in 1..200 {
            let v = a.db_at(hz * (1.0 + i as f32 * 0.001));
            assert!((v - prev).abs() < 2.0, "bins {i} differ by {}", (v - prev).abs());
            prev = v;
        }
    }
}
