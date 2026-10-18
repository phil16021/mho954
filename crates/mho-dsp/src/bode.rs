//! Evaluation of a measured frequency response: unwrapped phase, −3 dB corner, gain and
//! phase crossovers, stability margins, CSV export.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct BodeSummary {
    /// Gain at the first frequency (dB), the reference for the −3 dB corner.
    pub reference_gain_db: Option<f64>,
    /// First frequency where the gain drops 3 dB below the reference (interpolated).
    pub corner_3db_hz: Option<f64>,
    /// First frequency where the gain crosses 0 dB downwards.
    pub unity_gain_hz: Option<f64>,
    /// 180° + phase at the unity-gain frequency (loop gain measurements).
    pub phase_margin_deg: Option<f64>,
    /// First frequency where the unwrapped phase reaches −180°.
    pub phase_crossover_hz: Option<f64>,
    /// −gain at the phase crossover (dB).
    pub gain_margin_db: Option<f64>,
    pub max_gain_db: Option<f64>,
    pub max_gain_hz: Option<f64>,
}

/// Phase without the ±180° jumps of a wrapped measurement (degrees).
pub fn unwrap_phase(phase_deg: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(phase_deg.len());
    let mut offset = 0.0;
    for (i, &p) in phase_deg.iter().enumerate() {
        if i > 0 {
            let d = p - phase_deg[i - 1];
            if d > 180.0 {
                offset -= 360.0;
            } else if d < -180.0 {
                offset += 360.0;
            }
        }
        out.push(p + offset);
    }
    out
}

/// Frequency where `y` crosses `level` between points `i` and `i + 1`, interpolated on a
/// logarithmic frequency axis.
fn crossing(f: &[f64], y: &[f64], i: usize, level: f64) -> f64 {
    let (y0, y1) = (y[i], y[i + 1]);
    let t = if (y1 - y0).abs() < 1e-12 {
        0.0
    } else {
        (level - y0) / (y1 - y0)
    };
    let (l0, l1) = (f[i].ln(), f[i + 1].ln());
    (l0 + t * (l1 - l0)).exp()
}

/// Value of `y` at frequency `x` (log-interpolated between points `i` and `i + 1`).
fn value_at(f: &[f64], y: &[f64], i: usize, x: f64) -> f64 {
    let (l0, l1) = (f[i].ln(), f[i + 1].ln());
    let t = if (l1 - l0).abs() < 1e-12 {
        0.0
    } else {
        (x.ln() - l0) / (l1 - l0)
    };
    y[i] + t * (y[i + 1] - y[i])
}

/// Summary of points `(frequency Hz, gain dB, phase °)` in increasing frequency.
pub fn bode_summary(freq: &[f64], gain_db: &[f64], phase_deg: &[f64]) -> BodeSummary {
    let n = freq.len().min(gain_db.len()).min(phase_deg.len());
    if n == 0 {
        return BodeSummary::default();
    }
    let (f, g) = (&freq[..n], &gain_db[..n]);
    let ph = unwrap_phase(&phase_deg[..n]);
    let reference = g[0];
    let first_down = |level: f64| (0..n - 1).find(|&i| g[i] >= level && g[i + 1] < level);
    let corner = first_down(reference - 3.0).map(|i| crossing(f, g, i, reference - 3.0));
    let unity = first_down(0.0).map(|i| (i, crossing(f, g, i, 0.0)));
    let phase_x = (0..n - 1)
        .find(|&i| ph[i] > -180.0 && ph[i + 1] <= -180.0)
        .map(|i| (i, crossing(f, &ph, i, -180.0)));
    let (imax, gmax) =
        g.iter().enumerate().fold(
            (0, f64::MIN),
            |acc, (i, &v)| if v > acc.1 { (i, v) } else { acc },
        );
    BodeSummary {
        reference_gain_db: Some(reference),
        corner_3db_hz: corner,
        unity_gain_hz: unity.map(|u| u.1),
        phase_margin_deg: unity.map(|(i, fu)| 180.0 + value_at(f, &ph, i, fu)),
        phase_crossover_hz: phase_x.map(|p| p.1),
        gain_margin_db: phase_x.map(|(i, fp)| -value_at(f, g, i, fp)),
        max_gain_db: Some(gmax),
        max_gain_hz: Some(f[imax]),
    }
}

/// CSV with the summary as `#` comment lines, then `frequency_hz,gain_db,phase_deg,
/// phase_unwrapped_deg`.
pub fn bode_csv(freq: &[f64], gain_db: &[f64], phase_deg: &[f64]) -> String {
    let s = bode_summary(freq, gain_db, phase_deg);
    let ph = unwrap_phase(phase_deg);
    let mut out = String::new();
    let mut note = |k: &str, v: Option<f64>| {
        if let Some(v) = v {
            out.push_str(&format!("# {k}: {v}\n"));
        }
    };
    note("corner_3db_hz", s.corner_3db_hz);
    note("unity_gain_hz", s.unity_gain_hz);
    note("phase_margin_deg", s.phase_margin_deg);
    note("phase_crossover_hz", s.phase_crossover_hz);
    note("gain_margin_db", s.gain_margin_db);
    out.push_str("frequency_hz,gain_db,phase_deg,phase_unwrapped_deg\n");
    for i in 0..freq.len().min(gain_db.len()).min(ph.len()) {
        out.push_str(&format!(
            "{},{},{},{}\n",
            freq[i], gain_db[i], phase_deg[i], ph[i]
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two cascaded first-order low-passes at 1 kHz with a gain of 100 (40 dB).
    fn two_pole(points: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let f: Vec<f64> = (0..points)
            .map(|i| 10f64.powf(1.0 + 5.0 * i as f64 / (points - 1) as f64))
            .collect();
        let (mut g, mut p) = (vec![], vec![]);
        for &x in &f {
            let r = x / 1e3;
            g.push(40.0 - 20.0 * (1.0 + r * r).log10());
            let deg = -2.0 * r.atan().to_degrees();
            // wrapped like a measurement
            p.push(if deg < -180.0 { deg + 360.0 } else { deg });
        }
        (f, g, p)
    }

    #[test]
    fn unwrap() {
        assert_eq!(
            unwrap_phase(&[-170.0, 175.0, 160.0]),
            vec![-170.0, -185.0, -200.0]
        );
        assert_eq!(unwrap_phase(&[10.0, -10.0]), vec![10.0, -10.0]);
    }

    #[test]
    fn summary_of_two_pole_response() {
        let (f, g, p) = two_pole(201);
        let s = bode_summary(&f, &g, &p);
        // −3 dB of two coincident poles at fc·√(√2 − 1) ≈ 0.6436 fc
        let c = s.corner_3db_hz.unwrap();
        assert!((c / 643.6 - 1.0).abs() < 0.01, "{c}");
        // |H| = 1 where (1 + r²) = 100 → r = √99
        let u = s.unity_gain_hz.unwrap();
        assert!((u / (99f64.sqrt() * 1e3) - 1.0).abs() < 0.01, "{u}");
        let pm = s.phase_margin_deg.unwrap();
        let expect = 180.0 - 2.0 * 99f64.sqrt().atan().to_degrees();
        assert!((pm - expect).abs() < 0.5, "{pm} vs {expect}");
        // two poles never reach −180°
        assert_eq!(s.phase_crossover_hz, None);
        assert!((s.max_gain_db.unwrap() - 40.0).abs() < 0.01);
        let csv = bode_csv(&f, &g, &p);
        assert!(csv.starts_with("# corner_3db_hz: "));
        assert_eq!(csv.lines().filter(|l| !l.starts_with('#')).count(), 202);
    }

    #[test]
    fn empty_and_flat() {
        assert_eq!(bode_summary(&[], &[], &[]), BodeSummary::default());
        let s = bode_summary(&[1e3, 1e4], &[0.0, 0.0], &[0.0, 0.0]);
        assert_eq!((s.corner_3db_hz, s.unity_gain_hz), (None, None));
    }
}
