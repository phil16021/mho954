//! Magnitude spectrum via real FFT.

use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Window {
    Rectangular,
    #[default]
    Hann,
    Blackman,
    /// Flat-top: accurate amplitudes, wide main lobe.
    FlatTop,
}

impl Window {
    fn coeff(self, i: usize, n: usize) -> f64 {
        let x = 2.0 * PI * i as f64 / (n - 1).max(1) as f64;
        match self {
            Window::Rectangular => 1.0,
            Window::Hann => 0.5 - 0.5 * x.cos(),
            Window::Blackman => 0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos(),
            Window::FlatTop => {
                0.21557895 - 0.41663158 * x.cos() + 0.277263158 * (2.0 * x).cos()
                    - 0.083578947 * (3.0 * x).cos()
                    + 0.006947368 * (4.0 * x).cos()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
pub struct Spectrum {
    /// Bin spacing in Hz.
    pub df: f64,
    /// RMS amplitude per bin in dBV (0 dBV = 1 V RMS).
    pub dbv: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
pub struct SpectrumPeak {
    pub frequency: f64,
    pub dbv: f64,
    /// RMS amplitude in volts.
    pub vrms: f64,
}

/// Computes the single-sided amplitude spectrum of `v` (volts, sample interval `dt`).
/// The mean is removed first so the DC bin does not dominate the display.
pub fn spectrum(v: &[f64], dt: f64, window: Window) -> Spectrum {
    let n = v.len();
    if n < 4 || dt <= 0.0 {
        return Spectrum { df: 0.0, dbv: Vec::new() };
    }
    let mean = v.iter().sum::<f64>() / n as f64;
    let coeffs: Vec<f64> = (0..n).map(|i| window.coeff(i, n)).collect();
    let gain: f64 = coeffs.iter().sum::<f64>() / n as f64;
    let mut input: Vec<f64> = v.iter().zip(&coeffs).map(|(x, w)| (x - mean) * w).collect();

    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut out = fft.make_output_vec();
    fft.process(&mut input, &mut out).expect("buffer sizes match plan");

    let scale = 1.0 / (n as f64 * gain);
    let dbv = out
        .iter()
        .enumerate()
        .map(|(k, c)| {
            // Single-sided peak amplitude → RMS: ×2 for folding, ÷√2 for RMS (DC/Nyquist not folded).
            let edge = k == 0 || (n % 2 == 0 && k == n / 2);
            let peak = c.norm() * scale * if edge { 1.0 } else { 2.0 };
            let rms = if edge { peak } else { peak / 2f64.sqrt() };
            20.0 * rms.max(1e-12).log10()
        })
        .collect();
    Spectrum { df: 1.0 / (n as f64 * dt), dbv }
}

impl Spectrum {
    pub fn frequency(&self, bin: usize) -> f64 {
        bin as f64 * self.df
    }

    /// The `count` strongest local maxima above DC, strongest first.
    pub fn peaks(&self, count: usize) -> Vec<SpectrumPeak> {
        let d = &self.dbv;
        let mut peaks: Vec<SpectrumPeak> = (2..d.len().saturating_sub(1))
            .filter(|&k| d[k] > d[k - 1] && d[k] >= d[k + 1])
            .map(|k| {
                // Parabolic interpolation of the peak position on the dB curve.
                let (a, b, c) = (d[k - 1], d[k], d[k + 1]);
                let denom = a - 2.0 * b + c;
                let off = if denom.abs() > 1e-12 { 0.5 * (a - c) / denom } else { 0.0 };
                SpectrumPeak { frequency: (k as f64 + off) * self.df, dbv: b, vrms: 10f64.powf(b / 20.0) }
            })
            .collect();
        peaks.sort_by(|a, b| b.dbv.total_cmp(&a.dbv));
        peaks.truncate(count);
        peaks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sine_peak() {
        let dt = 1e-6;
        let n = 4096;
        let f0 = 12_345.0;
        let v: Vec<f64> = (0..n).map(|i| (2.0 * PI * f0 * i as f64 * dt).sin()).collect();
        let s = spectrum(&v, dt, Window::FlatTop);
        let p = s.peaks(1)[0];
        assert!((p.frequency - f0).abs() < s.df, "peak at {}", p.frequency);
        // 1 V peak sine = 0.707 V RMS = -3.01 dBV.
        assert!((p.dbv + 3.01).abs() < 0.2, "level {}", p.dbv);
    }
}
