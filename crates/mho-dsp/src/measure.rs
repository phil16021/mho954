//! Automatic measurements computed locally from sample data.
//!
//! Definitions follow common oscilloscope conventions: top/base are the histogram modes of
//! the upper/lower half of the signal (falling back to max/min for sine-like signals),
//! timing measurements use the 10 %/50 %/90 % levels between base and top.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
pub struct Measurements {
    pub vmax: f64,
    pub vmin: f64,
    pub vpp: f64,
    pub vtop: f64,
    pub vbase: f64,
    pub vamp: f64,
    pub vavg: f64,
    pub vrms: f64,
    /// RMS with the mean removed.
    pub vrms_ac: f64,
    pub frequency: Option<f64>,
    pub period: Option<f64>,
    /// Positive duty cycle in percent.
    pub duty: Option<f64>,
    pub rise_time: Option<f64>,
    pub fall_time: Option<f64>,
    /// Overshoot in percent of amplitude.
    pub overshoot: Option<f64>,
    /// Preshoot in percent of amplitude.
    pub preshoot: Option<f64>,
    /// Number of samples analysed.
    pub samples: usize,
}

/// Analyses `v` (volts) sampled every `dt` seconds.
pub fn analyze(v: &[f64], dt: f64) -> Measurements {
    if v.is_empty() {
        return Measurements::default();
    }
    let n = v.len() as f64;
    let (vmin, vmax) = v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    let vavg = v.iter().sum::<f64>() / n;
    let vrms = (v.iter().map(|x| x * x).sum::<f64>() / n).sqrt();
    let vrms_ac = (v.iter().map(|x| (x - vavg).powi(2)).sum::<f64>() / n).sqrt();
    let (vtop, vbase) = top_base(v, vmin, vmax);
    let vamp = vtop - vbase;

    let mut m = Measurements {
        vmax,
        vmin,
        vpp: vmax - vmin,
        vtop,
        vbase,
        vamp,
        vavg,
        vrms,
        vrms_ac,
        samples: v.len(),
        ..Default::default()
    };
    if vamp <= f64::EPSILON || dt <= 0.0 {
        return m;
    }

    let mid = vbase + 0.5 * vamp;
    let hyst = 0.1 * vamp;
    let edges = find_edges(v, mid, hyst);

    let rising: Vec<f64> = edges.iter().filter(|e| e.rising).map(|e| e.pos).collect();
    let falling: Vec<f64> = edges.iter().filter(|e| !e.rising).map(|e| e.pos).collect();
    let periods: Vec<f64> = if rising.len() >= 2 {
        rising.windows(2).map(|w| w[1] - w[0]).collect()
    } else if falling.len() >= 2 {
        falling.windows(2).map(|w| w[1] - w[0]).collect()
    } else {
        Vec::new()
    };
    if !periods.is_empty() {
        let period = periods.iter().sum::<f64>() / periods.len() as f64 * dt;
        m.period = Some(period);
        m.frequency = Some(1.0 / period);
        m.duty = duty_cycle(&edges, period / dt).map(|d| d * 100.0);
    }

    let lo = vbase + 0.1 * vamp;
    let hi = vbase + 0.9 * vamp;
    m.rise_time = mean(edges.iter().filter(|e| e.rising).filter_map(|e| transition_time(v, e.pos, lo, hi)))
        .map(|s| s * dt);
    m.fall_time = mean(edges.iter().filter(|e| !e.rising).filter_map(|e| transition_time(v, e.pos, hi, lo)))
        .map(|s| s * dt);
    if !edges.is_empty() {
        m.overshoot = Some(((vmax - vtop) / vamp * 100.0).max(0.0));
        m.preshoot = Some(((vbase - vmin) / vamp * 100.0).max(0.0));
    }
    m
}

fn mean(it: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = it.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    (n > 0).then(|| sum / n as f64)
}

/// Histogram-based top/base. For signals without flat levels (sine, triangle) the modes are
/// not pronounced and max/min are used instead, matching scope behaviour.
fn top_base(v: &[f64], vmin: f64, vmax: f64) -> (f64, f64) {
    let span = vmax - vmin;
    if span <= f64::EPSILON {
        return (vmax, vmin);
    }
    const BINS: usize = 256;
    let mut hist = [0usize; BINS];
    for &x in v {
        let b = (((x - vmin) / span) * (BINS - 1) as f64).round() as usize;
        hist[b.min(BINS - 1)] += 1;
    }
    let level = |b: usize| vmin + span * b as f64 / (BINS - 1) as f64;
    let half = BINS / 2;
    let (top_bin, top_count) = (half..BINS).map(|b| (b, hist[b])).max_by_key(|&(_, c)| c).unwrap();
    let (base_bin, base_count) = (0..half).map(|b| (b, hist[b])).max_by_key(|&(_, c)| c).unwrap();
    // A flat level holds a clear share of the samples; otherwise fall back to extremes.
    let threshold = v.len() / 20;
    let top = if top_count > threshold { level(top_bin) } else { vmax };
    let base = if base_count > threshold { level(base_bin) } else { vmin };
    (top, base)
}

#[derive(Debug, Clone, Copy)]
struct Edge {
    /// Interpolated sample position of the mid-level crossing.
    pos: f64,
    rising: bool,
}

fn find_edges(v: &[f64], mid: f64, hyst: f64) -> Vec<Edge> {
    let mut edges = Vec::new();
    // State: Some(true) = above mid+hyst, Some(false) = below mid-hyst.
    let mut state: Option<bool> = None;
    let mut last_cross: Option<f64> = None;
    for i in 1..v.len() {
        let (a, b) = (v[i - 1], v[i]);
        if (a - mid) * (b - mid) <= 0.0 && a != b {
            last_cross = Some((i - 1) as f64 + (mid - a) / (b - a));
        }
        let high = b > mid + hyst;
        let low = b < mid - hyst;
        match (state, high, low) {
            (Some(false), true, _) | (Some(true), _, true) => {
                if let Some(pos) = last_cross {
                    edges.push(Edge { pos, rising: high });
                }
                state = Some(high);
            }
            (None, true, _) => state = Some(true),
            (None, _, true) => state = Some(false),
            _ => {}
        }
    }
    edges
}

/// Mean high time (rising → next falling edge) relative to the period (in samples).
fn duty_cycle(edges: &[Edge], period: f64) -> Option<f64> {
    let highs: Vec<f64> =
        edges.windows(2).filter(|w| w[0].rising && !w[1].rising).map(|w| w[1].pos - w[0].pos).collect();
    if highs.is_empty() || period <= 0.0 {
        return None;
    }
    Some(highs.iter().sum::<f64>() / highs.len() as f64 / period)
}

/// Time in samples between crossing `from` and `to` around the edge at `pos`.
fn transition_time(v: &[f64], pos: f64, from: f64, to: f64) -> Option<f64> {
    let center = pos.round() as isize;
    let rising = to > from;
    let crosses = |a: f64, b: f64, level: f64| if rising { a < level && b >= level } else { a > level && b <= level };
    let interp = |i: usize, level: f64| {
        let (a, b) = (v[i], v[i + 1]);
        i as f64 + if b != a { (level - a) / (b - a) } else { 0.0 }
    };
    // Search backwards for the `from` crossing, forwards for the `to` crossing.
    let mut start = None;
    let mut i = center.min(v.len() as isize - 2);
    while i >= 0 {
        let k = i as usize;
        if crosses(v[k], v[k + 1], from) {
            start = Some(interp(k, from));
            break;
        }
        i -= 1;
    }
    let mut end = None;
    for k in center.max(0) as usize..v.len().saturating_sub(1) {
        if crosses(v[k], v[k + 1], to) {
            end = Some(interp(k, to));
            break;
        }
    }
    match (start, end) {
        (Some(s), Some(e)) if e > s => Some(e - s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn sine(freq: f64, amp: f64, dt: f64, n: usize) -> Vec<f64> {
        (0..n).map(|i| amp * (2.0 * PI * freq * i as f64 * dt).sin()).collect()
    }

    #[test]
    fn sine_measurements() {
        let dt = 1e-6;
        let v = sine(1e3, 1.0, dt, 10_000);
        let m = analyze(&v, dt);
        assert!((m.vpp - 2.0).abs() < 1e-3, "vpp {}", m.vpp);
        assert!((m.vrms - 1.0 / 2f64.sqrt()).abs() < 1e-3, "vrms {}", m.vrms);
        assert!(m.vavg.abs() < 1e-3);
        let f = m.frequency.unwrap();
        assert!((f - 1e3).abs() < 1.0, "freq {f}");
        assert!((m.duty.unwrap() - 50.0).abs() < 0.5);
    }

    #[test]
    fn square_wave_levels_and_duty() {
        let dt = 1e-6;
        // 1 kHz, 25 % duty, 0..3.3 V with finite rise time of 10 samples.
        let v: Vec<f64> = (0..20_000)
            .map(|i| {
                let ph = (i % 1000) as f64;
                if ph < 10.0 {
                    3.3 * ph / 10.0
                } else if ph < 250.0 {
                    3.3
                } else if ph < 260.0 {
                    3.3 * (1.0 - (ph - 250.0) / 10.0)
                } else {
                    0.0
                }
            })
            .collect();
        let m = analyze(&v, dt);
        assert!((m.vtop - 3.3).abs() < 0.02, "top {}", m.vtop);
        assert!(m.vbase.abs() < 0.02, "base {}", m.vbase);
        assert!((m.frequency.unwrap() - 1e3).abs() < 0.5);
        assert!((m.duty.unwrap() - 25.0).abs() < 0.5, "duty {:?}", m.duty);
        let rise = m.rise_time.unwrap();
        assert!((rise - 8e-6).abs() < 0.5e-6, "rise {rise}");
        let fall = m.fall_time.unwrap();
        assert!((fall - 8e-6).abs() < 0.5e-6, "fall {fall}");
    }

    #[test]
    fn duty_with_single_rising_edge() {
        // high, low, high, low: only one rising edge in the record.
        let v: Vec<f64> = (0..2000).map(|i| if (i / 500) % 2 == 0 { 3.0 } else { 0.0 }).collect();
        let m = analyze(&v, 1e-6);
        assert!((m.duty.unwrap() - 50.0).abs() < 0.5, "duty {:?}", m.duty);
        assert!((m.frequency.unwrap() - 1e3).abs() < 1.0);
    }

    #[test]
    fn flat_line_has_no_timing() {
        let m = analyze(&[0.5; 100], 1e-6);
        assert_eq!(m.vpp, 0.0);
        assert!(m.frequency.is_none());
    }
}
