//! Logic-analyzer data: transitions of up to 64 digital lines and their VCD export (readable
//! by PulseView/sigrok and GTKWave), plus single-tone analysis for frequency sweeps.

use std::f64::consts::PI;
use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The bus value from `time` on (bit i = line i).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct LogicEvent {
    /// Seconds relative to the trigger.
    pub time: f64,
    pub value: u64,
}

/// Parses a decoded bus value: `0x1F`/`1Fh`-style hex (the scope's HEX format), `0b…`/plain
/// binary digits are not accepted to avoid ambiguity.
pub fn parse_hex_value(s: &str) -> Option<u64> {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let t = t.strip_suffix(['h', 'H']).unwrap_or(t);
    if t.is_empty() {
        return None;
    }
    u64::from_str_radix(t, 16).ok()
}

/// Value Change Dump of `events` for `names.len()` lines (line i = bit i). Times are written
/// in picoseconds relative to the first event.
pub fn write_vcd(events: &[LogicEvent], names: &[String]) -> String {
    let mut out = String::new();
    let ids: Vec<String> = (0..names.len())
        .map(|i| {
            // Printable identifiers '!'..'~', two characters beyond 94 lines.
            let c = |k: usize| char::from(b'!' + (k % 94) as u8);
            if i < 94 {
                c(i).to_string()
            } else {
                format!("{}{}", c(i / 94), c(i))
            }
        })
        .collect();
    let _ = writeln!(out, "$version mho954 $end");
    let _ = writeln!(out, "$timescale 1 ps $end");
    let _ = writeln!(out, "$scope module logic $end");
    for (id, name) in ids.iter().zip(names) {
        let _ = writeln!(out, "$var wire 1 {id} {name} $end");
    }
    let _ = writeln!(out, "$upscope $end");
    let _ = writeln!(out, "$enddefinitions $end");
    let Some(first) = events.first() else {
        return out;
    };
    let t0 = first.time;
    let mut last: Option<u64> = None;
    let mut last_ps: i64 = -1;
    for e in events {
        let ps = ((e.time - t0) * 1e12).round() as i64;
        let changed: Vec<usize> = (0..names.len())
            .filter(|&b| last.is_none_or(|l| (l ^ e.value) >> b & 1 == 1))
            .collect();
        if changed.is_empty() {
            continue;
        }
        if ps != last_ps {
            if last.is_none() {
                let _ = writeln!(out, "#{ps}\n$dumpvars");
            } else {
                let _ = writeln!(out, "#{ps}");
            }
        }
        for b in changed {
            let _ = writeln!(out, "{}{}", e.value >> b & 1, ids[b]);
        }
        if last.is_none() {
            let _ = writeln!(out, "$end");
        }
        last = Some(e.value);
        last_ps = ps;
    }
    out
}

/// Amplitude (peak) and phase (rad) of the component at `freq` in `v`, sampled every `dt`
/// starting at `t0` (single-bin DFT over whole periods).
pub fn tone(v: &[f64], dt: f64, t0: f64, freq: f64) -> Option<(f64, f64)> {
    if v.len() < 8 || dt <= 0.0 || freq <= 0.0 {
        return None;
    }
    // Whole number of periods avoids leakage from the truncated last one.
    let per_period = 1.0 / (freq * dt);
    let periods = (v.len() as f64 / per_period).floor();
    if periods < 1.0 {
        return None;
    }
    let n = ((periods * per_period).round() as usize).min(v.len());
    let mean = v[..n].iter().sum::<f64>() / n as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (k, x) in v[..n].iter().enumerate() {
        let w = 2.0 * PI * freq * (t0 + k as f64 * dt);
        re += (x - mean) * w.cos();
        im -= (x - mean) * w.sin();
    }
    let amp = 2.0 * (re * re + im * im).sqrt() / n as f64;
    // sin(ωt + φ) has DFT phase φ - π/2.
    Some((amp, im.atan2(re) + PI / 2.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_values() {
        assert_eq!(parse_hex_value("0x48"), Some(0x48));
        assert_eq!(parse_hex_value("FFh"), Some(255));
        assert_eq!(parse_hex_value(" a5 "), Some(0xA5));
        assert_eq!(parse_hex_value(""), None);
        assert_eq!(parse_hex_value("xyz"), None);
    }

    #[test]
    fn vcd_output() {
        let names: Vec<String> = (0..3).map(|i| format!("D{i}")).collect();
        let ev = [
            LogicEvent {
                time: -1e-6,
                value: 0b001,
            },
            LogicEvent {
                time: -0.5e-6,
                value: 0b011,
            },
            LogicEvent {
                time: -0.5e-6,
                value: 0b011,
            },
            LogicEvent {
                time: 0.0,
                value: 0b110,
            },
        ];
        let v = write_vcd(&ev, &names);
        assert!(v.contains("$var wire 1 ! D0 $end"));
        assert!(v.contains("#0\n$dumpvars\n1!\n0\"\n0#\n$end"), "{v}");
        assert!(v.contains("#500000\n1\"\n"), "{v}");
        assert!(v.contains("#1000000\n0!\n1#\n"), "{v}");
        assert_eq!(v.matches("#500000").count(), 1);
    }

    #[test]
    fn single_tone() {
        let f = 1e3;
        let dt = 1e-6;
        let v: Vec<f64> = (0..5300)
            .map(|k| 0.3 + 2.0 * (2.0 * PI * f * (k as f64 * dt - 1e-3) + 0.5).sin())
            .collect();
        let (a, p) = tone(&v, dt, -1e-3, f).unwrap();
        assert!((a - 2.0).abs() < 1e-3, "{a}");
        assert!((p - 0.5).abs() < 1e-3, "{p}");
        assert!(tone(&v[..100], dt, 0.0, f).is_none());
    }
}
